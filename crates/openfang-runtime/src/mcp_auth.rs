//! MCP OAuth2 Dynamic Client Registration (DCR) and token management.
//!
//! Implements RFC 8414 (OAuth2 Server Metadata Discovery), RFC 7591 (Dynamic
//! Client Registration), and token acquisition for MCP server authentication.
//!
//! Flow:
//! 1. Discover authorization server metadata at `/.well-known/oauth-authorization-server`
//! 2. Register client dynamically via the registration endpoint
//! 3. Obtain access token via client_credentials grant
//! 4. Attach Bearer token to MCP requests
//! 5. Refresh token before expiry

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// OAuth2 Server Metadata (RFC 8414)
// ---------------------------------------------------------------------------

/// OAuth2 Authorization Server Metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthServerMetadata {
    /// The authorization server's issuer identifier.
    pub issuer: String,
    /// URL of the authorization endpoint (for authorization_code flow).
    #[serde(default)]
    pub authorization_endpoint: Option<String>,
    /// URL of the token endpoint.
    pub token_endpoint: String,
    /// URL of the dynamic client registration endpoint (RFC 7591).
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    /// Supported grant types.
    #[serde(default)]
    pub grant_types_supported: Vec<String>,
    /// Supported response types.
    #[serde(default)]
    pub response_types_supported: Vec<String>,
    /// Supported scopes.
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    /// Token endpoint authentication methods.
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,
    /// URL of the revocation endpoint.
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
}

// ---------------------------------------------------------------------------
// Dynamic Client Registration (RFC 7591)
// ---------------------------------------------------------------------------

/// Request body for dynamic client registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRegistrationRequest {
    /// Human-readable client name.
    pub client_name: String,
    /// Requested grant types (e.g. ["client_credentials"]).
    #[serde(default)]
    pub grant_types: Vec<String>,
    /// Requested response types.
    #[serde(default)]
    pub response_types: Vec<String>,
    /// Redirect URIs (for authorization_code flow).
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// Token endpoint authentication method.
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
    /// Requested scope (space-separated).
    #[serde(default)]
    pub scope: Option<String>,
    /// Software identifier.
    #[serde(default)]
    pub software_id: Option<String>,
    /// Software version.
    #[serde(default)]
    pub software_version: Option<String>,
}

/// Response from a successful dynamic client registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRegistrationResponse {
    /// Issued client ID.
    pub client_id: String,
    /// Issued client secret (for confidential clients).
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Expiry time for the client secret (epoch seconds).
    #[serde(default)]
    pub client_secret_expires_at: Option<u64>,
    /// Registration access token for managing the registration.
    #[serde(default)]
    pub registration_access_token: Option<String>,
    /// URI for updating/deleting the registration.
    #[serde(default)]
    pub registration_client_uri: Option<String>,
    /// Granted grant types.
    #[serde(default)]
    pub grant_types: Vec<String>,
    /// Token endpoint auth method.
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// OAuth2 token response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    /// The access token.
    pub access_token: String,
    /// Token type (usually "Bearer").
    pub token_type: String,
    /// Lifetime in seconds.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// Refresh token (if issued).
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Granted scope.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Cached token with expiry tracking.
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    refresh_token: Option<String>,
    obtained_at: Instant,
    expires_in: Duration,
}

impl CachedToken {
    fn is_expired(&self) -> bool {
        // Consider expired 30 seconds before actual expiry for safety margin
        let margin = Duration::from_secs(30);
        self.obtained_at.elapsed() + margin >= self.expires_in
    }
}

// ---------------------------------------------------------------------------
// MCP Auth Client
// ---------------------------------------------------------------------------

/// Configuration for MCP OAuth2 authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpAuthConfig {
    /// Base URL of the MCP server (used for metadata discovery).
    pub server_url: String,
    /// Pre-configured client ID (skips DCR if set).
    #[serde(default)]
    pub client_id: Option<String>,
    /// Pre-configured client secret.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Requested scopes (space-separated).
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Whether to use DCR when client_id is not set.
    #[serde(default = "default_true")]
    pub enable_dcr: bool,
}

fn default_scope() -> String {
    "mcp".to_string()
}

fn default_true() -> bool {
    true
}

/// OAuth2 authentication client for MCP servers.
///
/// Handles metadata discovery, DCR, token acquisition, refresh, and caching.
pub struct McpAuthClient {
    config: McpAuthConfig,
    http: reqwest::Client,
    metadata: RwLock<Option<OAuthServerMetadata>>,
    registration: RwLock<Option<ClientRegistrationResponse>>,
    token: RwLock<Option<CachedToken>>,
}

impl McpAuthClient {
    /// Create a new auth client for the given MCP server.
    pub fn new(config: McpAuthConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");

        Self {
            config,
            http,
            metadata: RwLock::new(None),
            registration: RwLock::new(None),
            token: RwLock::new(None),
        }
    }

    /// Get a valid access token, performing discovery/registration/refresh as needed.
    pub async fn get_token(&self) -> Result<String, McpAuthError> {
        // Fast path: cached token still valid
        {
            let guard = self.token.read().await;
            if let Some(ref cached) = *guard {
                if !cached.is_expired() {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        // Slow path: need to acquire or refresh
        self.acquire_token().await
    }

    /// Discover OAuth2 server metadata.
    pub async fn discover_metadata(&self) -> Result<OAuthServerMetadata, McpAuthError> {
        // Return cached if available
        {
            let guard = self.metadata.read().await;
            if let Some(ref meta) = *guard {
                return Ok(meta.clone());
            }
        }

        let base = self.config.server_url.trim_end_matches('/');
        let url = format!("{base}/.well-known/oauth-authorization-server");
        debug!(url = %url, "Discovering OAuth2 server metadata");

        let resp = self
            .http
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| McpAuthError::Discovery(format!("HTTP error: {e}")))?;

        if !resp.status().is_success() {
            return Err(McpAuthError::Discovery(format!(
                "Server returned {} for metadata discovery",
                resp.status()
            )));
        }

        let meta: OAuthServerMetadata = resp
            .json()
            .await
            .map_err(|e| McpAuthError::Discovery(format!("Invalid metadata JSON: {e}")))?;

        info!(issuer = %meta.issuer, "Discovered OAuth2 server metadata");

        let mut guard = self.metadata.write().await;
        *guard = Some(meta.clone());

        Ok(meta)
    }

    /// Register dynamically with the authorization server (RFC 7591).
    pub async fn register_client(&self) -> Result<ClientRegistrationResponse, McpAuthError> {
        // Return cached if available
        {
            let guard = self.registration.read().await;
            if let Some(ref reg) = *guard {
                return Ok(reg.clone());
            }
        }

        let meta = self.discover_metadata().await?;

        let reg_endpoint = meta
            .registration_endpoint
            .ok_or_else(|| McpAuthError::Registration("Server does not support DCR".into()))?;

        let request = ClientRegistrationRequest {
            client_name: "OpenFang Agent OS".to_string(),
            grant_types: vec!["client_credentials".to_string()],
            response_types: vec![],
            redirect_uris: vec![],
            token_endpoint_auth_method: Some("client_secret_post".to_string()),
            scope: Some(self.config.scope.clone()),
            software_id: Some("openfang".to_string()),
            software_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };

        debug!(endpoint = %reg_endpoint, "Registering client dynamically");

        let resp = self
            .http
            .post(&reg_endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|e| McpAuthError::Registration(format!("HTTP error: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(McpAuthError::Registration(format!(
                "Registration failed ({status}): {body}"
            )));
        }

        let reg: ClientRegistrationResponse = resp
            .json()
            .await
            .map_err(|e| McpAuthError::Registration(format!("Invalid registration JSON: {e}")))?;

        info!(client_id = %reg.client_id, "Dynamic client registration successful");

        let mut guard = self.registration.write().await;
        *guard = Some(reg.clone());

        Ok(reg)
    }

    /// Acquire or refresh an access token.
    async fn acquire_token(&self) -> Result<String, McpAuthError> {
        // Check if we have a refresh token to use
        let refresh_token = {
            let guard = self.token.read().await;
            guard.as_ref().and_then(|c| c.refresh_token.clone())
        };

        if let Some(ref rt) = refresh_token {
            match self.refresh_token(rt).await {
                Ok(token) => return Ok(token),
                Err(e) => {
                    warn!(error = %e, "Token refresh failed, falling back to new grant");
                }
            }
        }

        // Get client credentials (from config or DCR)
        let (client_id, client_secret) = self.resolve_client_credentials().await?;

        // Request token via client_credentials grant
        let meta = self.discover_metadata().await?;

        let mut params = HashMap::new();
        params.insert("grant_type", "client_credentials".to_string());
        params.insert("client_id", client_id);
        if let Some(ref secret) = client_secret {
            params.insert("client_secret", secret.clone());
        }
        if !self.config.scope.is_empty() {
            params.insert("scope", self.config.scope.clone());
        }

        debug!(endpoint = %meta.token_endpoint, "Requesting access token");

        let resp = self
            .http
            .post(&meta.token_endpoint)
            .form(&params)
            .send()
            .await
            .map_err(|e| McpAuthError::TokenAcquisition(format!("HTTP error: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(McpAuthError::TokenAcquisition(format!(
                "Token request failed ({status}): {body}"
            )));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| McpAuthError::TokenAcquisition(format!("Invalid token JSON: {e}")))?;

        let access_token = token_resp.access_token.clone();
        let expires_in = Duration::from_secs(token_resp.expires_in.unwrap_or(3600));

        info!(
            expires_in_secs = expires_in.as_secs(),
            "Access token acquired"
        );

        let cached = CachedToken {
            access_token: access_token.clone(),
            refresh_token: token_resp.refresh_token,
            obtained_at: Instant::now(),
            expires_in,
        };

        let mut guard = self.token.write().await;
        *guard = Some(cached);

        Ok(access_token)
    }

    /// Refresh an existing token.
    async fn refresh_token(&self, refresh_token: &str) -> Result<String, McpAuthError> {
        let (client_id, client_secret) = self.resolve_client_credentials().await?;
        let meta = self.discover_metadata().await?;

        let mut params = HashMap::new();
        params.insert("grant_type", "refresh_token".to_string());
        params.insert("refresh_token", refresh_token.to_string());
        params.insert("client_id", client_id);
        if let Some(ref secret) = client_secret {
            params.insert("client_secret", secret.clone());
        }

        let resp = self
            .http
            .post(&meta.token_endpoint)
            .form(&params)
            .send()
            .await
            .map_err(|e| McpAuthError::TokenRefresh(format!("HTTP error: {e}")))?;

        if !resp.status().is_success() {
            return Err(McpAuthError::TokenRefresh(format!(
                "Refresh failed ({})",
                resp.status()
            )));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| McpAuthError::TokenRefresh(format!("Invalid JSON: {e}")))?;

        let access_token = token_resp.access_token.clone();
        let expires_in = Duration::from_secs(token_resp.expires_in.unwrap_or(3600));

        debug!(expires_in_secs = expires_in.as_secs(), "Token refreshed");

        let cached = CachedToken {
            access_token: access_token.clone(),
            refresh_token: token_resp.refresh_token.or(Some(refresh_token.to_string())),
            obtained_at: Instant::now(),
            expires_in,
        };

        let mut guard = self.token.write().await;
        *guard = Some(cached);

        Ok(access_token)
    }

    /// Resolve client credentials — either from config or via DCR.
    async fn resolve_client_credentials(
        &self,
    ) -> Result<(String, Option<String>), McpAuthError> {
        if let Some(ref id) = self.config.client_id {
            return Ok((id.clone(), self.config.client_secret.clone()));
        }

        if !self.config.enable_dcr {
            return Err(McpAuthError::Registration(
                "No client_id configured and DCR is disabled".into(),
            ));
        }

        let reg = self.register_client().await?;
        Ok((reg.client_id, reg.client_secret))
    }

    /// Check if this auth client has a valid (non-expired) token cached.
    pub async fn has_valid_token(&self) -> bool {
        let guard = self.token.read().await;
        guard.as_ref().is_some_and(|c| !c.is_expired())
    }

    /// Clear all cached state (metadata, registration, tokens).
    pub async fn clear_cache(&self) {
        *self.metadata.write().await = None;
        *self.registration.write().await = None;
        *self.token.write().await = None;
    }

    /// Get the server URL this client is configured for.
    pub fn server_url(&self) -> &str {
        &self.config.server_url
    }
}

// ---------------------------------------------------------------------------
// Auth token store — manages auth clients for multiple MCP servers
// ---------------------------------------------------------------------------

/// Manages OAuth2 auth clients for multiple MCP servers.
pub struct McpAuthStore {
    clients: RwLock<HashMap<String, Arc<McpAuthClient>>>,
}

impl McpAuthStore {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
        }
    }

    /// Get or create an auth client for a server.
    pub async fn get_or_create(&self, config: McpAuthConfig) -> Arc<McpAuthClient> {
        let key = config.server_url.clone();

        // Check existing
        {
            let guard = self.clients.read().await;
            if let Some(client) = guard.get(&key) {
                return client.clone();
            }
        }

        // Create new
        let client = Arc::new(McpAuthClient::new(config));
        let mut guard = self.clients.write().await;
        guard.entry(key).or_insert(client).clone()
    }

    /// Get a token for a server URL (creates client if needed).
    pub async fn get_token(&self, config: McpAuthConfig) -> Result<String, McpAuthError> {
        let client = self.get_or_create(config).await;
        client.get_token().await
    }

    /// Remove a server's auth state.
    pub async fn remove(&self, server_url: &str) {
        let mut guard = self.clients.write().await;
        guard.remove(server_url);
    }

    /// List all servers with auth state.
    pub async fn servers(&self) -> Vec<String> {
        let guard = self.clients.read().await;
        guard.keys().cloned().collect()
    }
}

impl Default for McpAuthStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors from MCP OAuth2 operations.
#[derive(Debug, Clone)]
pub enum McpAuthError {
    /// Failed to discover OAuth2 server metadata.
    Discovery(String),
    /// Dynamic client registration failed.
    Registration(String),
    /// Token acquisition failed.
    TokenAcquisition(String),
    /// Token refresh failed.
    TokenRefresh(String),
}

impl std::fmt::Display for McpAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discovery(msg) => write!(f, "OAuth2 discovery error: {msg}"),
            Self::Registration(msg) => write!(f, "DCR error: {msg}"),
            Self::TokenAcquisition(msg) => write!(f, "Token error: {msg}"),
            Self::TokenRefresh(msg) => write!(f, "Token refresh error: {msg}"),
        }
    }
}

impl std::error::Error for McpAuthError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cached_token_expiry() {
        let fresh = CachedToken {
            access_token: "tok123".into(),
            refresh_token: None,
            obtained_at: Instant::now(),
            expires_in: Duration::from_secs(3600),
        };
        assert!(!fresh.is_expired());

        // Token obtained 2 hours ago with 1 hour expiry
        let expired = CachedToken {
            access_token: "tok456".into(),
            refresh_token: Some("ref789".into()),
            obtained_at: Instant::now() - Duration::from_secs(7200),
            expires_in: Duration::from_secs(3600),
        };
        assert!(expired.is_expired());
    }

    #[test]
    fn test_cached_token_safety_margin() {
        // Token that expires in 20 seconds should be considered expired (30s margin)
        let almost_expired = CachedToken {
            access_token: "tok".into(),
            refresh_token: None,
            obtained_at: Instant::now() - Duration::from_secs(3580),
            expires_in: Duration::from_secs(3600),
        };
        assert!(almost_expired.is_expired());
    }

    #[test]
    fn test_auth_config_defaults() {
        let config: McpAuthConfig = serde_json::from_str(
            r#"{"server_url": "https://example.com"}"#,
        )
        .unwrap();
        assert_eq!(config.scope, "mcp");
        assert!(config.enable_dcr);
        assert!(config.client_id.is_none());
    }

    #[test]
    fn test_auth_config_with_client_id() {
        let config: McpAuthConfig = serde_json::from_str(
            r#"{"server_url": "https://example.com", "client_id": "my-app", "client_secret": "secret123", "enable_dcr": false}"#,
        )
        .unwrap();
        assert_eq!(config.client_id.as_deref(), Some("my-app"));
        assert_eq!(config.client_secret.as_deref(), Some("secret123"));
        assert!(!config.enable_dcr);
    }

    #[test]
    fn test_server_metadata_deserialization() {
        let json = r#"{
            "issuer": "https://auth.example.com",
            "token_endpoint": "https://auth.example.com/token",
            "registration_endpoint": "https://auth.example.com/register",
            "grant_types_supported": ["client_credentials", "authorization_code"],
            "scopes_supported": ["mcp", "tools"]
        }"#;
        let meta: OAuthServerMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.issuer, "https://auth.example.com");
        assert_eq!(meta.token_endpoint, "https://auth.example.com/token");
        assert!(meta.registration_endpoint.is_some());
        assert_eq!(meta.grant_types_supported.len(), 2);
    }

    #[test]
    fn test_registration_request_serialization() {
        let req = ClientRegistrationRequest {
            client_name: "OpenFang".into(),
            grant_types: vec!["client_credentials".into()],
            response_types: vec![],
            redirect_uris: vec![],
            token_endpoint_auth_method: Some("client_secret_post".into()),
            scope: Some("mcp tools".into()),
            software_id: Some("openfang".into()),
            software_version: Some("0.3.45".into()),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["client_name"], "OpenFang");
        assert_eq!(json["grant_types"][0], "client_credentials");
        assert_eq!(json["software_id"], "openfang");
    }

    #[test]
    fn test_registration_response_deserialization() {
        let json = r#"{
            "client_id": "dyn-client-123",
            "client_secret": "secret-abc",
            "client_secret_expires_at": 0,
            "grant_types": ["client_credentials"],
            "token_endpoint_auth_method": "client_secret_post"
        }"#;
        let resp: ClientRegistrationResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.client_id, "dyn-client-123");
        assert_eq!(resp.client_secret.as_deref(), Some("secret-abc"));
    }

    #[test]
    fn test_token_response_deserialization() {
        let json = r#"{
            "access_token": "eyJhbGciOiJSUzI1NiIs...",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "mcp tools"
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.token_type, "Bearer");
        assert_eq!(resp.expires_in, Some(3600));
        assert!(resp.refresh_token.is_none());
    }

    #[test]
    fn test_token_response_with_refresh() {
        let json = r#"{
            "access_token": "at_123",
            "token_type": "Bearer",
            "expires_in": 1800,
            "refresh_token": "rt_456",
            "scope": "mcp"
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.refresh_token.as_deref(), Some("rt_456"));
        assert_eq!(resp.expires_in, Some(1800));
    }

    #[test]
    fn test_auth_error_display() {
        let err = McpAuthError::Discovery("404 not found".into());
        assert!(err.to_string().contains("discovery"));
        assert!(err.to_string().contains("404 not found"));

        let err = McpAuthError::Registration("server rejected".into());
        assert!(err.to_string().contains("DCR"));

        let err = McpAuthError::TokenAcquisition("invalid_grant".into());
        assert!(err.to_string().contains("Token error"));

        let err = McpAuthError::TokenRefresh("expired".into());
        assert!(err.to_string().contains("refresh"));
    }

    #[test]
    fn test_auth_store_default() {
        let store = McpAuthStore::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert!(store.servers().await.is_empty());
        });
    }

    #[test]
    fn test_auth_client_creation() {
        let config = McpAuthConfig {
            server_url: "https://mcp.example.com".into(),
            client_id: Some("test-client".into()),
            client_secret: Some("test-secret".into()),
            scope: "mcp".into(),
            enable_dcr: false,
        };
        let client = McpAuthClient::new(config);
        assert_eq!(client.server_url(), "https://mcp.example.com");
    }

    #[test]
    fn test_auth_store_get_or_create() {
        let store = McpAuthStore::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = McpAuthConfig {
                server_url: "https://mcp.example.com".into(),
                client_id: Some("test".into()),
                client_secret: None,
                scope: "mcp".into(),
                enable_dcr: false,
            };

            let client1 = store.get_or_create(config.clone()).await;
            let client2 = store.get_or_create(config).await;

            // Same Arc — should be the same client
            assert!(Arc::ptr_eq(&client1, &client2));
            assert_eq!(store.servers().await.len(), 1);
        });
    }

    #[test]
    fn test_auth_store_remove() {
        let store = McpAuthStore::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = McpAuthConfig {
                server_url: "https://mcp.example.com".into(),
                client_id: Some("test".into()),
                client_secret: None,
                scope: "mcp".into(),
                enable_dcr: false,
            };
            store.get_or_create(config).await;
            assert_eq!(store.servers().await.len(), 1);

            store.remove("https://mcp.example.com").await;
            assert!(store.servers().await.is_empty());
        });
    }

    #[test]
    fn test_clear_cache() {
        let config = McpAuthConfig {
            server_url: "https://mcp.example.com".into(),
            client_id: Some("test".into()),
            client_secret: None,
            scope: "mcp".into(),
            enable_dcr: false,
        };
        let client = McpAuthClient::new(config);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert!(!client.has_valid_token().await);
            client.clear_cache().await;
            assert!(!client.has_valid_token().await);
        });
    }

    #[test]
    fn test_minimal_metadata() {
        // Minimal valid metadata (only required fields)
        let json = r#"{
            "issuer": "https://auth.example.com",
            "token_endpoint": "https://auth.example.com/oauth/token"
        }"#;
        let meta: OAuthServerMetadata = serde_json::from_str(json).unwrap();
        assert!(meta.registration_endpoint.is_none());
        assert!(meta.authorization_endpoint.is_none());
        assert!(meta.grant_types_supported.is_empty());
    }
}
