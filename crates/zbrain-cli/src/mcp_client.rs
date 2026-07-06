//! Outbound HTTP MCP client for thin-client mode (multi-topology v1).
//!
//! Mirrors `src/core/mcp-client.ts` in the TypeScript codebase.
//! Provides the `call_remote_tool` function that routes operations
//! through a remote MCP server when running in thin-client mode.

use std::collections::BTreeMap;
use std::sync::Arc;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::Config;

// ──────────────────────────────────────────────────────────────────────────
// Remote MCP Types
// ──────────────────────────────────────────────────────────────────────────

/// Error type for remote MCP calls.
/// Mirrors `RemoteMcpError` in TypeScript.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RemoteMcpError {
    /// Remote MCP config not present
    NotConfigured,
    /// OAuth token acquisition failed
    TokenAcquisitionFailed { message: String },
    /// HTTP transport error
    TransportError { message: String },
    /// Response parsing failed
    ParseError { message: String },
    /// Remote tool returned an error
    ToolError {
        code: String, message: String,
        #[serde(default)]
        data: Option<serde_json::Value>,
    },
    /// Operation timed out
    Timeout,
    /// Operation cancelled
    Cancelled,
}

impl std::fmt::Display for RemoteMcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteMcpError::NotConfigured => write!(f, "remote_mcp not configured"),
            RemoteMcpError::TokenAcquisitionFailed { message } => {
                write!(f, "token acquisition failed: {message}")
            }
            RemoteMcpError::TransportError { message } => {
                write!(f, "transport error: {message}")
            }
            RemoteMcpError::ParseError { message } => {
                write!(f, "response parse error: {message}")
            }
            RemoteMcpError::ToolError { code, message, .. } => {
                write!(f, "{code}: {message}")
            }
            RemoteMcpError::Timeout => write!(f, "operation timed out"),
            RemoteMcpError::Cancelled => write!(f, "operation cancelled"),
        }
    }
}

impl std::error::Error for RemoteMcpError {}

/// Convert a reqwest transport error into a typed [`RemoteMcpError`].
///
/// A client-side timeout (the reqwest `Client` was built with `.timeout(...)`)
/// maps to [`RemoteMcpError::Timeout`] so the CLI dispatcher can render the
/// timeout-specific hint; every other transport failure (DNS, connection
/// refused, TLS) maps to [`RemoteMcpError::TransportError`].
fn map_reqwest_error(e: &reqwest::Error) -> RemoteMcpError {
    if e.is_timeout() {
        RemoteMcpError::Timeout
    } else {
        RemoteMcpError::TransportError { message: e.to_string() }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// OAuth Token Handling
// ──────────────────────────────────────────────────────────────────────────

/// OAuth 2.0 token response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Cached access token with expiry.
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: std::time::Instant,
}

/// Thread-safe token cache.
#[derive(Debug, Default)]
pub struct TokenCache {
    inner: RwLock<Option<CachedToken>>,
}

impl TokenCache {
    /// Create a new empty token cache.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Get a cached token if it exists and is not expired.
    pub async fn get(&self) -> Option<String> {
        let guard = self.inner.read().await;
        if let Some(ref cached) = *guard {
            if cached.expires_at > std::time::Instant::now() {
                return Some(cached.access_token.clone());
            }
        }
        None
    }

    /// Store a token in the cache with expiry.
    pub async fn set(&self, token: String, expires_in: u64) {
        // Subtract 60s to account for clock drift and network latency
        let effective_expiry = std::time::Duration::from_secs(expires_in.saturating_sub(60));
        let expires_at = std::time::Instant::now() + effective_expiry;
        *self.inner.write().await = Some(CachedToken { access_token: token, expires_at });
    }

    /// Clear the cached token (force refresh on next call).
    pub async fn clear(&self) {
        *self.inner.write().await = None;
    }
}

// ──────────────────────────────────────────────────────────────────────────
// MCP Client
// ──────────────────────────────────────────────────────────────────────────

/// MCP Client for thin-client mode.
pub struct McpClient {
    config: Config,
    http_client: Client,
    token_cache: Arc<TokenCache>,
}

impl McpClient {
    /// Create a new MCP client from a config with a per-call wall-clock timeout.
    ///
    /// The timeout is applied to the underlying reqwest `Client` (token mint +
    /// tool call), mirroring the TS behavior where `--timeout` (default: 180s
    /// for `think`, 30s otherwise) caps the whole routed call. On expiry, HTTP
    /// requests fail and surface as [`RemoteMcpError::Timeout`].
    pub fn new(config: Config, timeout: std::time::Duration) -> Self {
        let http_client = Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            config,
            http_client,
            token_cache: Arc::new(TokenCache::new()),
        }
    }

    /// Get or refresh the access token for MCP calls.
    /// Mirrors `getAccessToken` in TypeScript.
    async fn get_access_token(&self, force_refresh: bool) -> anyhow::Result<String> {
        let remote_mcp = self
            .config
            .remote_mcp
            .as_ref()
            .ok_or_else(|| RemoteMcpError::NotConfigured)?;

        // Try cache first unless force_refresh is true
        if !force_refresh {
            if let Some(token) = self.token_cache.get().await {
                return Ok(token);
            }
        }

        // Build token request
        let client_id = &remote_mcp.oauth_client_id;
        let env_secret = std::env::var("ZBRAIN_REMOTE_CLIENT_SECRET").ok();
        let client_secret = remote_mcp
            .oauth_client_secret
            .as_deref()
            .or(env_secret.as_deref())
            .ok_or_else(|| {
                RemoteMcpError::TokenAcquisitionFailed {
                    message: "oauth_client_secret not configured (set via config or ZBRAIN_REMOTE_CLIENT_SECRET env)".into(),
                }
            })?;

        let mut form = BTreeMap::new();
        form.insert("grant_type", "client_credentials");
        form.insert("client_id", client_id);
        form.insert("client_secret", client_secret);

        let token_url = format!("{}/oauth2/token", remote_mcp.issuer_url.trim_end_matches('/'));

        let response = self
            .http_client
            .post(&token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| map_reqwest_error(&e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(RemoteMcpError::TokenAcquisitionFailed {
                message: format!("token request failed with status {status}: {body}"),
            }.into());
        }

        let token_response: TokenResponse = response.json().await.map_err(|e| {
            RemoteMcpError::ParseError { message: format!("failed to parse token response: {e}") }
        })?;

        // Cache the token
        let expires_in = token_response.expires_in.unwrap_or(3600);
        self.token_cache.set(token_response.access_token.clone(), expires_in).await;

        Ok(token_response.access_token)
    }

    /// Call a remote MCP tool.
    /// Mirrors `callRemoteTool` in TypeScript.
    pub async fn call_tool(&self, tool_name: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // The per-call timeout (TS global flag `--timeout=<Ns|Nms|Nm>`, default
        // 180s for `think` / 30s otherwise) is applied on the reqwest `Client`
        // built in `McpClient::new`. A client-side timeout on any hop (token
        // mint or tool call) surfaces as `RemoteMcpError::Timeout` via
        // `map_reqwest_error`. See roadmap 1-2-1. (The local read-only
        // wall-clock timeout — TS cli.ts:1125-1170 — is a separate unmigrated
        // feature tracked by roadmap 1-2-3.)
        let remote_mcp = self
            .config
            .remote_mcp
            .as_ref()
            .ok_or_else(|| RemoteMcpError::NotConfigured)?;

        // Get access token (with one retry on 401)
        let mut token = self.get_access_token(false).await?;
        let mut retry_count = 0;

        loop {
            let mcp_url = format!("{}/tools/{tool_name}", remote_mcp.mcp_url.trim_end_matches('/'));

            let response = self
                .http_client
                .post(&mcp_url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .json(&args)
                .send()
                .await
                .map_err(|e| map_reqwest_error(&e))?;

            let status = response.status();

            // Handle 401 with one retry (token refresh)
            if status == reqwest::StatusCode::UNAUTHORIZED && retry_count < 1 {
                retry_count += 1;
                self.token_cache.clear().await;
                token = self.get_access_token(true).await?;
                continue;
            }

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                // Try to parse as structured error
                if let Ok(error_response) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(code) = error_response.get("code").and_then(|v| v.as_str()) {
                        let message = error_response
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&body);
                        return Err(RemoteMcpError::ToolError {
                            code: code.to_string(),
                            message: message.to_string(),
                            data: error_response.get("data").cloned(),
                        }.into());
                    }
                }
                return Err(RemoteMcpError::ToolError {
                    code: status.to_string(),
                    message: body,
                    data: None,
                }.into());
            }

            let result: serde_json::Value = response.json().await.map_err(|e| {
                RemoteMcpError::ParseError { message: format!("failed to parse tool response: {e}") }
            })?;

            // Extract result.content as per MCP spec
            return Ok(result.get("content").cloned().unwrap_or(result));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Build a Config in thin-client shape pointing at the given base URL.
    fn thin_client_config(base_url: &str) -> Config {
        let mut config = Config::default();
        config.remote_mcp = Some(crate::config::RemoteMcpConfig {
            issuer_url: base_url.to_string(),
            mcp_url: format!("{base_url}/mcp"),
            oauth_client_id: "test-client".to_string(),
            oauth_client_secret: Some("test-secret".to_string()),
        });
        config
    }

    /// A hanging server: accepts TCP connections but never writes a response,
    /// so any HTTP request against it blocks until the client-side timeout
    /// fires. Returns the bound `http://127.0.0.1:PORT` base URL.
    async fn spawn_hanging_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                // Accept and hold the socket open forever (drop nothing).
                if let Ok((stream, _)) = listener.accept().await {
                    // Leak the stream so the connection stays open and idle.
                    std::mem::forget(stream);
                }
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn call_tool_maps_client_timeout_to_timeout_variant() {
        let base = spawn_hanging_server().await;
        let config = thin_client_config(&base);

        // A tiny timeout so the test is fast; the token request (first network
        // hop) will hang and trip the client-side timeout.
        let client = McpClient::new(config, Duration::from_millis(150));
        let err = client
            .call_tool("query", serde_json::json!({}))
            .await
            .expect_err("hanging server must not succeed");

        let remote = err
            .downcast_ref::<RemoteMcpError>()
            .expect("error should be a RemoteMcpError");
        assert!(
            matches!(remote, RemoteMcpError::Timeout),
            "client timeout must map to RemoteMcpError::Timeout, got: {remote:?}"
        );
    }
}
