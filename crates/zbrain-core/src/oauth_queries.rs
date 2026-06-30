//! OAuth client management queries — register, update TTL, revoke.
//!
//! Admin-facing CRUD for OAuth clients. The trait decouples the web layer
//! from the storage backend (libsql / InMemory / postgres).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::error::Result;

// ── Request / Response types ────────────────────────────────────────────

/// Input for registering a new OAuth client from the admin dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterClientRequest {
    pub name: String,
    /// Space-separated scope string (already normalized by the handler).
    pub scope: String,
    pub grant_types: Vec<String>,
    pub redirect_uris: Vec<String>,
    pub token_endpoint_auth_method: Option<String>,
    pub token_ttl: Option<i64>,
}

/// Returned after a client is registered.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterClientResponse {
    pub client_id: String,
    pub client_secret: String,
}

/// Returned after updating a client's token TTL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientTtlResponse {
    pub updated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_ttl: Option<i64>,
}

/// Returned after revoking a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeClientResponse {
    pub revoked: bool,
}

// ── Trait ───────────────────────────────────────────────────────────────

/// Admin queries for OAuth client lifecycle management.
#[async_trait]
pub trait OAuthQueries: Debug + Send + Sync {
    /// Register a new OAuth client. Generates `client_id` and `client_secret`;
    /// hashes the secret before storing.
    async fn register_client(&self, req: RegisterClientRequest) -> Result<RegisterClientResponse>;

    /// Update the per-client token TTL. `ttl` of `None` or 0 resets to the
    /// server default (NULL in the DB).
    async fn update_client_ttl(
        &self,
        client_id: &str,
        ttl: Option<i64>,
    ) -> Result<UpdateClientTtlResponse>;

    /// Soft-delete a client (set `deleted_at`) and revoke all its active tokens.
    async fn revoke_client(&self, client_id: &str) -> Result<RevokeClientResponse>;
}
