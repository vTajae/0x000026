//! MCP Sampling — handle LLM completion requests from MCP servers.
//!
//! The MCP sampling feature lets servers request completions from the
//! client's LLM. This enables sophisticated server-side tools that need
//! to reason about data before returning results.
//!
//! MCP protocol method:
//! - `sampling/createMessage` — server requests a completion from the client

use serde::{Deserialize, Serialize};

/// A sampling request from an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingRequest {
    /// Messages to send to the LLM.
    pub messages: Vec<SamplingMessage>,
    /// Optional model preference hints.
    #[serde(default)]
    pub model_preferences: Option<ModelPreferences>,
    /// Optional system prompt.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Whether to include context from the MCP server.
    #[serde(default)]
    pub include_context: Option<IncludeContext>,
    /// Maximum tokens to generate.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Temperature for generation.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Stop sequences.
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    /// Additional metadata.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// A message in a sampling request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingMessage {
    /// Role: "user" or "assistant".
    pub role: String,
    /// Content of the message.
    pub content: SamplingContent,
}

/// Content types for sampling messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SamplingContent {
    /// Text content.
    Text { text: String },
    /// Image content (base64 encoded).
    Image { data: String, mime_type: String },
}

/// Model preference hints from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPreferences {
    /// Preferred model hints (not binding).
    #[serde(default)]
    pub hints: Vec<ModelHint>,
    /// Priority for cost optimization (0.0 = ignore, 1.0 = prioritize).
    #[serde(default)]
    pub cost_priority: Option<f32>,
    /// Priority for speed (0.0 = ignore, 1.0 = prioritize).
    #[serde(default)]
    pub speed_priority: Option<f32>,
    /// Priority for intelligence (0.0 = ignore, 1.0 = prioritize).
    #[serde(default)]
    pub intelligence_priority: Option<f32>,
}

/// A model hint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelHint {
    /// Model name pattern (e.g. "claude-3", "gpt-4").
    #[serde(default)]
    pub name: Option<String>,
}

/// Context inclusion mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IncludeContext {
    /// Include no extra context.
    None,
    /// Include context from the requesting server only.
    ThisServer,
    /// Include context from all connected servers.
    AllServers,
}

/// Response to a sampling request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingResponse {
    /// Role of the generated message (usually "assistant").
    pub role: String,
    /// Generated content.
    pub content: SamplingContent,
    /// Model that was actually used.
    pub model: String,
    /// Why generation stopped.
    #[serde(default)]
    pub stop_reason: Option<String>,
}

/// Configuration for sampling request handling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingConfig {
    /// Whether to allow MCP servers to request completions.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum tokens per sampling request (caps server requests).
    #[serde(default = "default_max_tokens")]
    pub max_tokens_cap: u32,
    /// Maximum messages per sampling request.
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
    /// Whether to require human approval for sampling requests.
    #[serde(default)]
    pub require_approval: bool,
    /// Allowed model patterns (empty = any).
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_max_messages() -> usize {
    20
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tokens_cap: 4096,
            max_messages: 20,
            require_approval: false,
            allowed_models: Vec::new(),
        }
    }
}

/// Validate a sampling request against the config limits.
pub fn validate_request(
    request: &SamplingRequest,
    config: &SamplingConfig,
) -> Result<(), SamplingError> {
    if !config.enabled {
        return Err(SamplingError::Disabled);
    }

    if request.messages.is_empty() {
        return Err(SamplingError::EmptyMessages);
    }

    if request.messages.len() > config.max_messages {
        return Err(SamplingError::TooManyMessages {
            requested: request.messages.len(),
            max: config.max_messages,
        });
    }

    if let Some(max_tokens) = request.max_tokens {
        if max_tokens > config.max_tokens_cap {
            return Err(SamplingError::TokensExceeded {
                requested: max_tokens,
                max: config.max_tokens_cap,
            });
        }
    }

    Ok(())
}

/// Parse a `sampling/createMessage` JSON-RPC request into a `SamplingRequest`.
pub fn parse_sampling_request(params: &serde_json::Value) -> Result<SamplingRequest, SamplingError> {
    serde_json::from_value(params.clone())
        .map_err(|e| SamplingError::InvalidRequest(e.to_string()))
}

/// Build a JSON-RPC response for a completed sampling request.
pub fn build_sampling_response(
    id: Option<serde_json::Value>,
    response: &SamplingResponse,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": response,
    })
}

/// Build a JSON-RPC error for a failed sampling request.
pub fn build_sampling_error(
    id: Option<serde_json::Value>,
    error: &SamplingError,
) -> serde_json::Value {
    let (code, message) = match error {
        SamplingError::Disabled => (-32600, "Sampling is disabled"),
        SamplingError::EmptyMessages => (-32602, "Messages array is empty"),
        SamplingError::TooManyMessages { .. } => (-32602, "Too many messages"),
        SamplingError::TokensExceeded { .. } => (-32602, "Token limit exceeded"),
        SamplingError::InvalidRequest(_) => (-32602, "Invalid sampling request"),
        SamplingError::LlmError(_) => (-32603, "LLM completion failed"),
        SamplingError::ApprovalDenied => (-32600, "Sampling request denied by user"),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": error.to_string(),
        },
    })
}

/// Map model preferences to a model selection hint.
pub fn select_model_hint(prefs: &ModelPreferences) -> Option<String> {
    // Check explicit hints first
    for hint in &prefs.hints {
        if let Some(ref name) = hint.name {
            return Some(name.clone());
        }
    }

    // Fall back to priority-based selection
    let cost = prefs.cost_priority.unwrap_or(0.0);
    let speed = prefs.speed_priority.unwrap_or(0.0);
    let intelligence = prefs.intelligence_priority.unwrap_or(0.0);

    if intelligence > cost && intelligence > speed {
        Some("high".to_string()) // Map to high-capability model
    } else if speed > cost {
        Some("fast".to_string()) // Map to fast model
    } else if cost > 0.0 {
        Some("cheap".to_string()) // Map to cost-effective model
    } else {
        None // No preference
    }
}

/// Extract plain text from a sampling request's messages.
pub fn messages_to_text(messages: &[SamplingMessage]) -> String {
    messages
        .iter()
        .map(|m| match &m.content {
            SamplingContent::Text { text } => format!("{}: {}", m.role, text),
            SamplingContent::Image { .. } => format!("{}: [image]", m.role),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SamplingError {
    Disabled,
    EmptyMessages,
    TooManyMessages { requested: usize, max: usize },
    TokensExceeded { requested: u32, max: u32 },
    InvalidRequest(String),
    LlmError(String),
    ApprovalDenied,
}

impl std::fmt::Display for SamplingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "Sampling is disabled"),
            Self::EmptyMessages => write!(f, "Messages array is empty"),
            Self::TooManyMessages { requested, max } => {
                write!(f, "Too many messages ({requested}, max {max})")
            }
            Self::TokensExceeded { requested, max } => {
                write!(f, "Token limit exceeded ({requested}, max {max})")
            }
            Self::InvalidRequest(msg) => write!(f, "Invalid request: {msg}"),
            Self::LlmError(msg) => write!(f, "LLM error: {msg}"),
            Self::ApprovalDenied => write!(f, "Sampling request denied"),
        }
    }
}

impl std::error::Error for SamplingError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(msg_count: usize) -> SamplingRequest {
        let messages = (0..msg_count)
            .map(|i| SamplingMessage {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: SamplingContent::Text {
                    text: format!("Message {i}"),
                },
            })
            .collect();
        SamplingRequest {
            messages,
            model_preferences: None,
            system_prompt: None,
            include_context: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: vec![],
            metadata: None,
        }
    }

    #[test]
    fn test_validate_request_ok() {
        let req = make_request(3);
        let config = SamplingConfig::default();
        assert!(validate_request(&req, &config).is_ok());
    }

    #[test]
    fn test_validate_disabled() {
        let req = make_request(1);
        let config = SamplingConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(matches!(
            validate_request(&req, &config),
            Err(SamplingError::Disabled)
        ));
    }

    #[test]
    fn test_validate_empty_messages() {
        let req = make_request(0);
        let config = SamplingConfig::default();
        assert!(matches!(
            validate_request(&req, &config),
            Err(SamplingError::EmptyMessages)
        ));
    }

    #[test]
    fn test_validate_too_many_messages() {
        let req = make_request(25);
        let config = SamplingConfig {
            max_messages: 10,
            ..Default::default()
        };
        assert!(matches!(
            validate_request(&req, &config),
            Err(SamplingError::TooManyMessages { .. })
        ));
    }

    #[test]
    fn test_validate_tokens_exceeded() {
        let mut req = make_request(1);
        req.max_tokens = Some(10000);
        let config = SamplingConfig {
            max_tokens_cap: 4096,
            ..Default::default()
        };
        assert!(matches!(
            validate_request(&req, &config),
            Err(SamplingError::TokensExceeded { .. })
        ));
    }

    #[test]
    fn test_parse_sampling_request() {
        let params = serde_json::json!({
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "Hello"}}
            ],
            "max_tokens": 100,
            "temperature": 0.7,
        });
        let req = parse_sampling_request(&params).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.max_tokens, Some(100));
        assert!((req.temperature.unwrap() - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_parse_sampling_request_invalid() {
        let params = serde_json::json!({"messages": "not an array"});
        assert!(parse_sampling_request(&params).is_err());
    }

    #[test]
    fn test_build_sampling_response() {
        let resp = SamplingResponse {
            role: "assistant".into(),
            content: SamplingContent::Text {
                text: "Hello!".into(),
            },
            model: "groq/llama-3-70b".into(),
            stop_reason: Some("end_turn".into()),
        };
        let json = build_sampling_response(Some(serde_json::json!(42)), &resp);
        assert_eq!(json["id"], 42);
        assert_eq!(json["result"]["role"], "assistant");
        assert_eq!(json["result"]["model"], "groq/llama-3-70b");
    }

    #[test]
    fn test_build_sampling_error() {
        let err = SamplingError::Disabled;
        let json = build_sampling_error(Some(serde_json::json!(5)), &err);
        assert_eq!(json["error"]["code"], -32600);
    }

    #[test]
    fn test_select_model_hint_explicit() {
        let prefs = ModelPreferences {
            hints: vec![ModelHint {
                name: Some("claude-3-opus".into()),
            }],
            cost_priority: None,
            speed_priority: None,
            intelligence_priority: None,
        };
        assert_eq!(select_model_hint(&prefs), Some("claude-3-opus".into()));
    }

    #[test]
    fn test_select_model_hint_intelligence() {
        let prefs = ModelPreferences {
            hints: vec![],
            cost_priority: Some(0.2),
            speed_priority: Some(0.3),
            intelligence_priority: Some(0.9),
        };
        assert_eq!(select_model_hint(&prefs), Some("high".into()));
    }

    #[test]
    fn test_select_model_hint_speed() {
        let prefs = ModelPreferences {
            hints: vec![],
            cost_priority: Some(0.1),
            speed_priority: Some(0.8),
            intelligence_priority: Some(0.2),
        };
        assert_eq!(select_model_hint(&prefs), Some("fast".into()));
    }

    #[test]
    fn test_select_model_hint_cost() {
        let prefs = ModelPreferences {
            hints: vec![],
            cost_priority: Some(0.9),
            speed_priority: Some(0.1),
            intelligence_priority: Some(0.1),
        };
        assert_eq!(select_model_hint(&prefs), Some("cheap".into()));
    }

    #[test]
    fn test_select_model_hint_none() {
        let prefs = ModelPreferences {
            hints: vec![],
            cost_priority: None,
            speed_priority: None,
            intelligence_priority: None,
        };
        assert_eq!(select_model_hint(&prefs), None);
    }

    #[test]
    fn test_messages_to_text() {
        let messages = vec![
            SamplingMessage {
                role: "user".into(),
                content: SamplingContent::Text {
                    text: "What is 2+2?".into(),
                },
            },
            SamplingMessage {
                role: "assistant".into(),
                content: SamplingContent::Text {
                    text: "4".into(),
                },
            },
        ];
        let text = messages_to_text(&messages);
        assert!(text.contains("user: What is 2+2?"));
        assert!(text.contains("assistant: 4"));
    }

    #[test]
    fn test_messages_to_text_with_image() {
        let messages = vec![SamplingMessage {
            role: "user".into(),
            content: SamplingContent::Image {
                data: "base64data".into(),
                mime_type: "image/png".into(),
            },
        }];
        let text = messages_to_text(&messages);
        assert!(text.contains("[image]"));
    }

    #[test]
    fn test_sampling_config_default() {
        let config = SamplingConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_tokens_cap, 4096);
        assert_eq!(config.max_messages, 20);
        assert!(!config.require_approval);
    }

    #[test]
    fn test_sampling_config_serde() {
        let json = r#"{"enabled": false, "max_tokens_cap": 2048, "require_approval": true}"#;
        let config: SamplingConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.max_tokens_cap, 2048);
        assert!(config.require_approval);
    }

    #[test]
    fn test_sampling_response_serde() {
        let resp = SamplingResponse {
            role: "assistant".into(),
            content: SamplingContent::Text {
                text: "result".into(),
            },
            model: "test-model".into(),
            stop_reason: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: SamplingResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.model, "test-model");
    }

    #[test]
    fn test_error_display() {
        let err = SamplingError::TooManyMessages {
            requested: 30,
            max: 20,
        };
        assert!(err.to_string().contains("30"));
        assert!(err.to_string().contains("20"));
    }

    #[test]
    fn test_include_context_serde() {
        let ctx: IncludeContext =
            serde_json::from_str(r#""thisServer""#).unwrap();
        assert!(matches!(ctx, IncludeContext::ThisServer));

        let ctx: IncludeContext =
            serde_json::from_str(r#""allServers""#).unwrap();
        assert!(matches!(ctx, IncludeContext::AllServers));
    }
}
