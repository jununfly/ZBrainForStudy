//! Token verification queries — access token validation and AuthInfo.
//!
//! This trait is the Rust counterpart of `oauthProvider.verifyAccessToken()`
//! in the TypeScript codebase. It is used by the `/mcp` and `/token` HTTP
//! handlers to validate bearer tokens before dispatching requests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::error::Result;

// ── AuthInfo ─────────────────────────────────────────────────────────────────

/// Identity and authorisation info extracted from a verified OAuth access token.
///
/// Mirrors the ZBrain `AuthInfo` interface in `src/core/operations.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthInfo {
    /// The raw bearer token string presented by the client.
    pub token: String,
    /// OAuth `client_id` of the token owner.
    pub client_id: String,
    /// Human-readable client name from `oauth_clients.client_name` (if set).
    pub client_name: Option<String>,
    /// Scopes granted to this token (space-separated strings stored as a Vec).
    pub scopes: Vec<String>,
    /// Expiry timestamp (Unix seconds). Always populated; expired tokens are rejected.
    pub expires_at: i64,
    /// Per-client source scope from `oauth_clients.source_id`.
    pub source_id: Option<String>,
    /// Resource URI associated with the token (from `oauth_tokens.resource`).
    pub resource: Option<String>,
    /// Federated read source IDs from `oauth_clients.federated_read`.
    /// Corresponds to `allowedSources` in the TS `AuthInfo`.
    pub allowed_sources: Option<Vec<String>>,
}

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors returned by [`TokenQueries::verify_access_token`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TokenError {
    /// The token was not found in the database, or is structurally invalid.
    #[error("invalid token")]
    Invalid,
    /// The token exists but its `expires_at` is in the past.
    #[error("token expired")]
    Expired,
    /// A storage-layer error occurred during lookup.
    #[error("token lookup failed: {0}")]
    Storage(String),
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Token verification query trait.
///
/// Implementations must be `Send + Sync` so they can be stored in `AppState`
/// behind an `Arc<dyn TokenQueries>`.
#[async_trait]
pub trait TokenQueries: Debug + Send + Sync {
    /// Verify a bearer token and return the associated [`AuthInfo`].
    ///
    /// Steps:
    /// 1. SHA-256 hash the raw token string.
    /// 2. Query `oauth_tokens` JOIN `oauth_clients` for `token_hash` + `token_type = 'access'`.
    /// 3. Validate `expires_at` (NULL or past → `TokenError::Expired`).
    /// 4. Fallback: if not found in `oauth_tokens`, check legacy `access_tokens` table.
    /// 5. Return `TokenError::Invalid` if neither table has a match.
    async fn verify_access_token(&self, token: &str) -> std::result::Result<AuthInfo, TokenError>;
}
