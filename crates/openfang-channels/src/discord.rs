//! Discord Gateway adapter for the OpenFang channel bridge.
//!
//! Uses Discord Gateway WebSocket (v10) for receiving messages and the REST API
//! for sending responses. No external Discord crate — just `tokio-tungstenite` + `reqwest`.

use crate::types::{
    split_message, ChannelAdapter, ChannelContent, ChannelMessage, ChannelType, ChannelUser,
};
use async_trait::async_trait;
use futures::{SinkExt, Stream, StreamExt};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, RwLock};
use tracing::{debug, error, info, warn};
use zeroize::Zeroizing;

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DISCORD_MSG_LIMIT: usize = 2000;

/// Discord Gateway opcodes.
mod opcode {
    pub const DISPATCH: u64 = 0;
    pub const HEARTBEAT: u64 = 1;
    pub const IDENTIFY: u64 = 2;
    pub const RESUME: u64 = 6;
    pub const RECONNECT: u64 = 7;
    pub const INVALID_SESSION: u64 = 9;
    pub const HELLO: u64 = 10;
    pub const HEARTBEAT_ACK: u64 = 11;
}

/// Discord Gateway adapter using WebSocket.
pub struct DiscordAdapter {
    /// SECURITY: Bot token is zeroized on drop to prevent memory disclosure.
    token: Zeroizing<String>,
    client: reqwest::Client,
    allowed_guilds: Vec<u64>,
    allowed_channels: Vec<String>,
    allowed_users: Vec<String>,
    read_only_channels: Vec<String>,
    intents: u64,
    shutdown_tx: Arc<watch::Sender<bool>>,
    shutdown_rx: watch::Receiver<bool>,
    /// Bot's own user ID (populated after READY event).
    bot_user_id: Arc<RwLock<Option<String>>>,
    /// Session ID for resume (populated after READY event).
    session_id: Arc<RwLock<Option<String>>>,
    /// Resume gateway URL.
    resume_gateway_url: Arc<RwLock<Option<String>>>,
}

impl DiscordAdapter {
    pub fn new(
        token: String,
        allowed_guilds: Vec<u64>,
        allowed_channels: Vec<String>,
        allowed_users: Vec<String>,
        read_only_channels: Vec<String>,
        intents: u64,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            token: Zeroizing::new(token),
            client: reqwest::Client::new(),
            allowed_guilds,
            allowed_channels,
            allowed_users,
            read_only_channels,
            intents,
            shutdown_tx: Arc::new(shutdown_tx),
            shutdown_rx,
            bot_user_id: Arc::new(RwLock::new(None)),
            session_id: Arc::new(RwLock::new(None)),
            resume_gateway_url: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the WebSocket gateway URL from the Discord API.
    async fn get_gateway_url(&self) -> Result<String, Box<dyn std::error::Error>> {
        let url = format!("{DISCORD_API_BASE}/gateway/bot");
        let resp: serde_json::Value = self
            .client
            .get(&url)
            .header("Authorization", format!("Bot {}", self.token.as_str()))
            .send()
            .await?
            .json()
            .await?;

        let ws_url = resp["url"]
            .as_str()
            .ok_or("Missing 'url' in gateway response")?;

        Ok(format!("{ws_url}/?v=10&encoding=json"))
    }

    /// Send a message to a Discord channel via REST API.
    async fn api_send_message(
        &self,
        channel_id: &str,
        text: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages");
        let chunks = split_message(text, DISCORD_MSG_LIMIT);

        for chunk in chunks {
            let body = serde_json::json!({ "content": chunk });
            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("Bot {}", self.token.as_str()))
                .json(&body)
                .send()
                .await?;

            if !resp.status().is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                warn!("Discord sendMessage failed: {body_text}");
            }
        }
        Ok(())
    }

    /// Send typing indicator to a Discord channel.
    async fn api_send_typing(&self, channel_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/typing");
        let _ = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.token.as_str()))
            .send()
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for DiscordAdapter {
    fn name(&self) -> &str {
        "discord"
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::Discord
    }

    async fn start(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = ChannelMessage> + Send>>, Box<dyn std::error::Error>>
    {
        let gateway_url = self.get_gateway_url().await?;
        info!("Discord gateway URL obtained");

        let (tx, rx) = mpsc::channel::<ChannelMessage>(256);

        let token = self.token.clone();
        let intents = self.intents;
        let allowed_guilds = self.allowed_guilds.clone();
        let allowed_channels = self.allowed_channels.clone();
        let allowed_users = self.allowed_users.clone();
        let read_only_channels = self.read_only_channels.clone();
        let bot_user_id = self.bot_user_id.clone();
        let session_id_store = self.session_id.clone();
        let resume_url_store = self.resume_gateway_url.clone();
        let mut shutdown = self.shutdown_rx.clone();

        tokio::spawn(async move {
            let mut backoff = INITIAL_BACKOFF;
            let mut connect_url = gateway_url;
            // Sequence persists across reconnections for RESUME
            let sequence: Arc<RwLock<Option<u64>>> = Arc::new(RwLock::new(None));

            loop {
                if *shutdown.borrow() {
                    break;
                }

                info!("Connecting to Discord gateway...");

                let ws_result = tokio_tungstenite::connect_async(&connect_url).await;
                let ws_stream = match ws_result {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        warn!("Discord gateway connection failed: {e}, retrying in {backoff:?}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                };

                backoff = INITIAL_BACKOFF;
                info!("Discord gateway connected");

                let (mut ws_tx, mut ws_rx) = ws_stream.split();
                let mut heartbeat_timer = tokio::time::interval(tokio::time::Duration::from_secs(45));
                heartbeat_timer.tick().await; // consume first immediate tick

                // Inner message loop — returns true if we should reconnect
                let should_reconnect = 'inner: loop {
                    let msg = tokio::select! {
                        msg = ws_rx.next() => msg,
                        _ = heartbeat_timer.tick() => {
                            // Send periodic heartbeat
                            let seq = *sequence.read().await;
                            let hb = serde_json::json!({ "op": opcode::HEARTBEAT, "d": seq });
                            if let Err(e) = ws_tx
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    serde_json::to_string(&hb).unwrap(),
                                ))
                                .await
                            {
                                warn!("Discord: failed to send heartbeat: {e}");
                                break 'inner true;
                            }
                            debug!("Discord heartbeat sent (seq={seq:?})");
                            continue;
                        }
                        _ = shutdown.changed() => {
                            if *shutdown.borrow() {
                                info!("Discord shutdown requested");
                                let _ = ws_tx.close().await;
                                return;
                            }
                            continue;
                        }
                    };

                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            warn!("Discord WebSocket error: {e}");
                            break 'inner true;
                        }
                        None => {
                            info!("Discord WebSocket closed");
                            break 'inner true;
                        }
                    };

                    let text = match msg {
                        tokio_tungstenite::tungstenite::Message::Text(t) => t,
                        tokio_tungstenite::tungstenite::Message::Close(_) => {
                            info!("Discord gateway closed by server");
                            break 'inner true;
                        }
                        _ => continue,
                    };

                    let payload: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!("Discord: failed to parse gateway message: {e}");
                            continue;
                        }
                    };

                    let op = payload["op"].as_u64().unwrap_or(999);

                    // Update sequence number
                    if let Some(s) = payload["s"].as_u64() {
                        *sequence.write().await = Some(s);
                    }

                    match op {
                        opcode::HELLO => {
                            let interval =
                                payload["d"]["heartbeat_interval"].as_u64().unwrap_or(45000);
                            heartbeat_timer = tokio::time::interval(tokio::time::Duration::from_millis(interval));
                            heartbeat_timer.tick().await; // consume first immediate tick
                            debug!("Discord HELLO: heartbeat_interval={interval}ms");

                            // Try RESUME if we have a session, otherwise IDENTIFY
                            let has_session = session_id_store.read().await.is_some();
                            let has_seq = sequence.read().await.is_some();

                            let gateway_msg = if has_session && has_seq {
                                let sid = session_id_store.read().await.clone().unwrap();
                                let seq = *sequence.read().await;
                                info!("Discord: sending RESUME (session={sid})");
                                serde_json::json!({
                                    "op": opcode::RESUME,
                                    "d": {
                                        "token": token.as_str(),
                                        "session_id": sid,
                                        "seq": seq
                                    }
                                })
                            } else {
                                info!("Discord: sending IDENTIFY");
                                serde_json::json!({
                                    "op": opcode::IDENTIFY,
                                    "d": {
                                        "token": token.as_str(),
                                        "intents": intents,
                                        "properties": {
                                            "os": "linux",
                                            "browser": "openfang",
                                            "device": "openfang"
                                        }
                                    }
                                })
                            };

                            if let Err(e) = ws_tx
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    serde_json::to_string(&gateway_msg).unwrap(),
                                ))
                                .await
                            {
                                error!("Discord: failed to send IDENTIFY/RESUME: {e}");
                                break 'inner true;
                            }
                        }

                        opcode::DISPATCH => {
                            let event_name = payload["t"].as_str().unwrap_or("");
                            let d = &payload["d"];

                            match event_name {
                                "READY" => {
                                    let user_id =
                                        d["user"]["id"].as_str().unwrap_or("").to_string();
                                    let username =
                                        d["user"]["username"].as_str().unwrap_or("unknown");
                                    let sid = d["session_id"].as_str().unwrap_or("").to_string();
                                    let resume_url =
                                        d["resume_gateway_url"].as_str().unwrap_or("").to_string();

                                    *bot_user_id.write().await = Some(user_id.clone());
                                    *session_id_store.write().await = Some(sid);
                                    if !resume_url.is_empty() {
                                        *resume_url_store.write().await = Some(resume_url);
                                    }

                                    info!("Discord bot ready: {username} ({user_id})");
                                }

                                "MESSAGE_CREATE" | "MESSAGE_UPDATE" => {
                                    if let Some(msg) =
                                        parse_discord_message(
                                            d,
                                            &bot_user_id,
                                            &allowed_guilds,
                                            &allowed_channels,
                                            &allowed_users,
                                            &read_only_channels,
                                        )
                                            .await
                                    {
                                        debug!(
                                            "Discord {event_name} from {}: {:?}",
                                            msg.sender.display_name, msg.content
                                        );
                                        if tx.send(msg).await.is_err() {
                                            return;
                                        }
                                    }
                                }

                                "RESUMED" => {
                                    info!("Discord session resumed successfully");
                                }

                                _ => {
                                    debug!("Discord event: {event_name}");
                                }
                            }
                        }

                        opcode::HEARTBEAT => {
                            // Server requests immediate heartbeat
                            let seq = *sequence.read().await;
                            let hb = serde_json::json!({ "op": opcode::HEARTBEAT, "d": seq });
                            let _ = ws_tx
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    serde_json::to_string(&hb).unwrap(),
                                ))
                                .await;
                        }

                        opcode::HEARTBEAT_ACK => {
                            debug!("Discord heartbeat ACK received");
                        }

                        opcode::RECONNECT => {
                            info!("Discord: server requested reconnect");
                            break 'inner true;
                        }

                        opcode::INVALID_SESSION => {
                            let resumable = payload["d"].as_bool().unwrap_or(false);
                            if resumable {
                                info!("Discord: invalid session (resumable)");
                            } else {
                                info!("Discord: invalid session (not resumable), clearing session");
                                *session_id_store.write().await = None;
                                *sequence.write().await = None;
                            }
                            break 'inner true;
                        }

                        _ => {
                            debug!("Discord: unknown opcode {op}");
                        }
                    }
                };

                if !should_reconnect || *shutdown.borrow() {
                    break;
                }

                // Try resume URL if available
                if let Some(ref url) = *resume_url_store.read().await {
                    connect_url = format!("{url}/?v=10&encoding=json");
                }

                warn!("Discord: reconnecting in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }

            info!("Discord gateway loop stopped");
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn send(
        &self,
        user: &ChannelUser,
        content: ChannelContent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // platform_id is the channel_id for Discord
        let channel_id = &user.platform_id;
        match content {
            ChannelContent::Text(text) => {
                self.api_send_message(channel_id, &text).await?;
            }
            _ => {
                self.api_send_message(channel_id, "(Unsupported content type)")
                    .await?;
            }
        }
        Ok(())
    }

    async fn send_typing(&self, user: &ChannelUser) -> Result<(), Box<dyn std::error::Error>> {
        self.api_send_typing(&user.platform_id).await
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.shutdown_tx.send(true);
        Ok(())
    }
}

/// Fetch the text content of a Discord CDN attachment URL.
///
/// Used to inline .txt, .md, .json, and other text-based file attachments
/// so the agent sees the content directly in the message.
async fn fetch_attachment_text(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Fetch failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.text()
        .await
        .map_err(|e| format!("Read body failed: {e}"))
}

/// Parse a Discord MESSAGE_CREATE or MESSAGE_UPDATE payload into a `ChannelMessage`.
async fn parse_discord_message(
    d: &serde_json::Value,
    bot_user_id: &Arc<RwLock<Option<String>>>,
    allowed_guilds: &[u64],
    allowed_channels: &[String],
    allowed_users: &[String],
    read_only_channels: &[String],
) -> Option<ChannelMessage> {
    let author = d.get("author")?;
    let author_id = author["id"].as_str()?;
    let username = author["username"].as_str().unwrap_or("Unknown");
    let channel_id = d["channel_id"].as_str()?;

    let is_read_only = !read_only_channels.is_empty() && read_only_channels.contains(&channel_id.to_string());

    // Filter out bot's own messages (always — even in read-only channels)
    if let Some(ref bid) = *bot_user_id.read().await {
        if author_id == bid {
            return None;
        }
    }

    // Filter out other bots — UNLESS this is a read-only channel (transcriber is a bot)
    if !is_read_only && author["bot"].as_bool() == Some(true) {
        return None;
    }

    // Filter by allowed guilds
    if !allowed_guilds.is_empty() {
        if let Some(guild_id) = d["guild_id"].as_str() {
            let gid: u64 = guild_id.parse().unwrap_or(0);
            if !allowed_guilds.contains(&gid) {
                return None;
            }
        }
    }

    // Filter by allowed channels (read-only channels are implicitly allowed)
    if !allowed_channels.is_empty() && !is_read_only && !allowed_channels.contains(&channel_id.to_string()) {
        return None;
    }

    // Filter by allowed users (skip for read-only channels — transcriber bot won't be listed)
    if !allowed_users.is_empty() && !is_read_only {
        let id_match = allowed_users.contains(&author_id.to_string());
        let name_match = allowed_users.contains(&username.to_string());
        if !id_match && !name_match {
            return None;
        }
    }

    let content_text = d["content"].as_str().unwrap_or("");

    // Extract attachment info (files, images sent alongside or instead of text)
    let attachments = d["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // For .txt file attachments, download the content inline so the agent sees it
    let mut attachment_texts: Vec<String> = Vec::new();
    let mut attachment_metadata: Vec<serde_json::Value> = Vec::new();
    for att in &attachments {
        let filename = att["filename"].as_str().unwrap_or("");
        let url = att["url"].as_str().unwrap_or("");
        let size = att["size"].as_u64().unwrap_or(0);
        let content_type = att["content_type"].as_str().unwrap_or("");

        // Store metadata for all attachments
        attachment_metadata.push(serde_json::json!({
            "filename": filename,
            "url": url,
            "size": size,
            "content_type": content_type,
        }));

        // Auto-fetch text-based attachments (txt, md, json, csv, log, yaml, toml, etc.)
        let is_text_file = filename.ends_with(".txt")
            || filename.ends_with(".md")
            || filename.ends_with(".json")
            || filename.ends_with(".csv")
            || filename.ends_with(".log")
            || filename.ends_with(".yaml")
            || filename.ends_with(".yml")
            || filename.ends_with(".toml")
            || filename.ends_with(".xml")
            || filename.ends_with(".py")
            || filename.ends_with(".rs")
            || filename.ends_with(".js")
            || filename.ends_with(".ts")
            || content_type.starts_with("text/");

        // Only fetch reasonably sized text files (< 100KB)
        if is_text_file && !url.is_empty() && size < 100_000 {
            match fetch_attachment_text(url).await {
                Ok(text) => {
                    attachment_texts.push(format!("--- Attached file: {filename} ---\n{text}\n--- End of {filename} ---"));
                }
                Err(e) => {
                    tracing::warn!(filename, url, error = %e, "Failed to fetch Discord attachment");
                    attachment_texts.push(format!("[Attachment: {filename} ({size} bytes) — fetch failed: {e}]"));
                }
            }
        } else if !url.is_empty() {
            // Non-text or too large — just note the attachment
            attachment_texts.push(format!("[Attachment: {filename} ({size} bytes, {content_type})]"));
        }
    }

    // Build final text: message content + attachment contents
    let combined_text = if content_text.is_empty() && attachment_texts.is_empty() {
        return None; // No text and no attachments — nothing to process
    } else if content_text.is_empty() {
        attachment_texts.join("\n\n")
    } else if attachment_texts.is_empty() {
        content_text.to_string()
    } else {
        format!("{content_text}\n\n{}", attachment_texts.join("\n\n"))
    };

    let message_id = d["id"].as_str().unwrap_or("0");
    let discriminator = author["discriminator"].as_str().unwrap_or("0000");
    let display_name = if discriminator == "0" {
        username.to_string()
    } else {
        format!("{username}#{discriminator}")
    };

    let timestamp = d["timestamp"]
        .as_str()
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);

    // Parse commands (messages starting with /)
    let content = if combined_text.starts_with('/') {
        let parts: Vec<&str> = combined_text.splitn(2, ' ').collect();
        let cmd_name = &parts[0][1..];
        let args = if parts.len() > 1 {
            parts[1].split_whitespace().map(String::from).collect()
        } else {
            vec![]
        };
        ChannelContent::Command {
            name: cmd_name.to_string(),
            args,
        }
    } else {
        ChannelContent::Text(combined_text)
    };

    // Build metadata with author info and read-only flag
    let mut metadata = HashMap::new();
    metadata.insert("author_id".to_string(), serde_json::Value::String(author_id.to_string()));
    metadata.insert("author_username".to_string(), serde_json::Value::String(username.to_string()));
    if !attachment_metadata.is_empty() {
        metadata.insert("attachments".to_string(), serde_json::Value::Array(attachment_metadata));
    }
    if is_read_only {
        metadata.insert("read_only".to_string(), serde_json::Value::Bool(true));
    }

    Some(ChannelMessage {
        channel: ChannelType::Discord,
        platform_message_id: message_id.to_string(),
        sender: ChannelUser {
            platform_id: channel_id.to_string(),
            display_name,
            openfang_user: None,
        },
        content,
        target_agent: None,
        timestamp,
        is_group: true,
        thread_id: None,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_discord_message_basic() {
        let bot_id = Arc::new(RwLock::new(Some("bot123".to_string())));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "Hello agent!",
            "author": {
                "id": "user456",
                "username": "alice",
                "discriminator": "0",
                "bot": false
            },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });

        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await.unwrap();
        assert_eq!(msg.channel, ChannelType::Discord);
        assert_eq!(msg.sender.display_name, "alice");
        assert_eq!(msg.sender.platform_id, "ch1");
        assert!(matches!(msg.content, ChannelContent::Text(ref t) if t == "Hello agent!"));
    }

    #[tokio::test]
    async fn test_parse_discord_message_filters_bot() {
        let bot_id = Arc::new(RwLock::new(Some("bot123".to_string())));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "My own message",
            "author": {
                "id": "bot123",
                "username": "openfang",
                "discriminator": "0"
            },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });

        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await;
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_parse_discord_message_filters_other_bots() {
        let bot_id = Arc::new(RwLock::new(Some("bot123".to_string())));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "Bot message",
            "author": {
                "id": "other_bot",
                "username": "somebot",
                "discriminator": "0",
                "bot": true
            },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });

        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await;
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_parse_discord_message_guild_filter() {
        let bot_id = Arc::new(RwLock::new(Some("bot123".to_string())));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "guild_id": "999",
            "content": "Hello",
            "author": {
                "id": "user1",
                "username": "bob",
                "discriminator": "0"
            },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });

        // Not in allowed guilds
        let msg = parse_discord_message(&d, &bot_id, &[111, 222], &[], &[], &[]).await;
        assert!(msg.is_none());

        // In allowed guilds
        let msg = parse_discord_message(&d, &bot_id, &[999], &[], &[], &[]).await;
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_parse_discord_command() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "/agent hello-world",
            "author": {
                "id": "user1",
                "username": "alice",
                "discriminator": "0"
            },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });

        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await.unwrap();
        match &msg.content {
            ChannelContent::Command { name, args } => {
                assert_eq!(name, "agent");
                assert_eq!(args, &["hello-world"]);
            }
            other => panic!("Expected Command, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_parse_discord_empty_content() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "",
            "author": {
                "id": "user1",
                "username": "alice",
                "discriminator": "0"
            },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });

        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await;
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_parse_discord_discriminator() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "Hi",
            "author": {
                "id": "user1",
                "username": "alice",
                "discriminator": "1234"
            },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });

        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await.unwrap();
        assert_eq!(msg.sender.display_name, "alice#1234");
    }

    #[tokio::test]
    async fn test_parse_discord_message_update() {
        let bot_id = Arc::new(RwLock::new(Some("bot123".to_string())));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "Edited message content",
            "author": {
                "id": "user456",
                "username": "alice",
                "discriminator": "0",
                "bot": false
            },
            "timestamp": "2024-01-01T00:00:00+00:00",
            "edited_timestamp": "2024-01-01T00:01:00+00:00"
        });

        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await.unwrap();
        assert_eq!(msg.channel, ChannelType::Discord);
        assert!(
            matches!(msg.content, ChannelContent::Text(ref t) if t == "Edited message content")
        );
    }

    #[test]
    fn test_discord_adapter_creation() {
        let adapter = DiscordAdapter::new(
            "test-token".to_string(),
            vec![123, 456],
            vec![],
            vec![],
            vec![],
            33280,
        );
        assert_eq!(adapter.name(), "discord");
        assert_eq!(adapter.channel_type(), ChannelType::Discord);
    }

    // --- New tests for channel/user/read-only filtering ---

    #[tokio::test]
    async fn test_channel_filter_blocks_unlisted_channel() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch999",
            "content": "Hello",
            "author": { "id": "u1", "username": "alice", "discriminator": "0" },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        let allowed_ch = vec!["ch1".to_string(), "ch2".to_string()];
        let msg = parse_discord_message(&d, &bot_id, &[], &allowed_ch, &[], &[]).await;
        assert!(msg.is_none());

        // Allowed channel passes
        let d2 = serde_json::json!({
            "id": "msg2",
            "channel_id": "ch1",
            "content": "Hello",
            "author": { "id": "u1", "username": "alice", "discriminator": "0" },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        let msg = parse_discord_message(&d2, &bot_id, &[], &allowed_ch, &[], &[]).await;
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_user_filter_by_id() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "Hello",
            "author": { "id": "u999", "username": "stranger", "discriminator": "0" },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        let allowed_users = vec!["u1".to_string(), "u2".to_string()];
        let msg = parse_discord_message(&d, &bot_id, &[], &[], &allowed_users, &[]).await;
        assert!(msg.is_none());

        // Matching ID passes
        let d2 = serde_json::json!({
            "id": "msg2",
            "channel_id": "ch1",
            "content": "Hello",
            "author": { "id": "u1", "username": "stranger", "discriminator": "0" },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        let msg = parse_discord_message(&d2, &bot_id, &[], &[], &allowed_users, &[]).await;
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_user_filter_by_username() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "Hello",
            "author": { "id": "u999", "username": "xomclovin", "discriminator": "0" },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        let allowed_users = vec!["xomclovin".to_string(), "iso.cry".to_string()];
        let msg = parse_discord_message(&d, &bot_id, &[], &[], &allowed_users, &[]).await;
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_read_only_channel_metadata() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ro_ch",
            "content": "Transcribed audio",
            "author": { "id": "bot_transcriber", "username": "transcriber", "discriminator": "0", "bot": true },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        let read_only = vec!["ro_ch".to_string()];
        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &read_only).await.unwrap();
        assert_eq!(msg.metadata.get("read_only").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(msg.metadata.get("author_id").and_then(|v| v.as_str()), Some("bot_transcriber"));
    }

    #[tokio::test]
    async fn test_read_only_bypasses_allowed_channels() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ro_ch",
            "content": "Transcribed text",
            "author": { "id": "bot_t", "username": "transcriber", "discriminator": "0", "bot": true },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        // ro_ch is NOT in allowed_channels, but IS in read_only_channels — should pass
        let allowed_ch = vec!["ch1".to_string()];
        let read_only = vec!["ro_ch".to_string()];
        let msg = parse_discord_message(&d, &bot_id, &[], &allowed_ch, &[], &read_only).await;
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_metadata_contains_author_info() {
        let bot_id = Arc::new(RwLock::new(None));
        let d = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "Hi",
            "author": { "id": "u42", "username": "alice", "discriminator": "0" },
            "timestamp": "2024-01-01T00:00:00+00:00"
        });
        let msg = parse_discord_message(&d, &bot_id, &[], &[], &[], &[]).await.unwrap();
        assert_eq!(msg.metadata.get("author_id").and_then(|v| v.as_str()), Some("u42"));
        assert_eq!(msg.metadata.get("author_username").and_then(|v| v.as_str()), Some("alice"));
        assert!(!msg.metadata.contains_key("read_only"));
    }
}
