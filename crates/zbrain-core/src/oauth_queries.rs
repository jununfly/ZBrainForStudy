//! OAuth client management queries and token exchange — register, update TTL,
//! revoke, client credentials, authorization code, and refresh token flows.
//!
//! The trait decouples the web layer from the storage backend
//! (libsql / InMemory / postgres).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::error::Result;

// ── Client management request / response types ─────────────────────────

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

// ── Token exchange types ───────────────────────────────────────────────

/// Full OAuth client information retrieved from storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientInfo {
    pub client_id: String,
    /// The stored SHA-256 hex hash of the client secret, or `None` for
    /// public clients (`token_endpoint_auth_method = "none"`).
    pub client_secret_hash: Option<String>,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    /// Space-separated scope string (the client's registered scope).
    pub scope: Option<String>,
    pub token_endpoint_auth_method: Option<String>,
    pub client_id_issued_at: Option<i64>,
    pub client_secret_expires_at: Option<i64>,
    /// Per-client TTL override in seconds (NULL = use server default).
    pub token_ttl: Option<i64>,
}

/// Token response returned by the /token endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExchangeTokens {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    /// Space-separated scope string.
    pub scope: String,
    /// Refresh token (only for authorization_code and refresh_token grants).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

// ── Trait ───────────────────────────────────────────────────────────────

/// OAuth queries: client lifecycle management + token exchange flows.
#[async_trait]
pub trait OAuthQueries: Debug + Send + Sync {
    // ── Client management ──────────────────────────────────────────────

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

    /// Look up a client by id. Returns `None` if not found.
    async fn get_client(&self, client_id: &str) -> Result<Option<OAuthClientInfo>>;

    // ── Token exchange ─────────────────────────────────────────────────

    /// Client credentials grant (RFC 6749 §4.4).
    /// Validates the client_id + client_secret, checks grant type, clamps
    /// requested scope against the client's registered scope, and issues an
    /// access token (no refresh token per RFC 6749 §4.4.3).
    async fn exchange_client_credentials(
        &self,
        client_id: &str,
        client_secret: &str,
        requested_scope: Option<&str>,
    ) -> Result<ExchangeTokens>;

    /// Verify a confidential client's secret without spending it.
    /// Returns the validated client info on success.
    /// Public clients (`client_secret_hash = None`) are refused.
    async fn verify_confidential_client_secret(
        &self,
        client_id: &str,
        presented_secret: &str,
    ) -> Result<OAuthClientInfo>;

    /// Authorization code grant (RFC 6749 §4.1.3).
    /// Atomically deletes the code row (single-use), validates client_id +
    /// redirect_uri, and issues access + refresh tokens.
    async fn exchange_authorization_code(
        &self,
        client_id: &str,
        authorization_code: &str,
        redirect_uri: Option<&str>,
    ) -> Result<ExchangeTokens>;

    /// Refresh token grant (RFC 6749 §6) with rotation.
    /// Atomically deletes the refresh token row (rotation), validates scope
    /// subset enforcement, and issues new access + refresh tokens.
    async fn exchange_refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        requested_scopes: Option<&[String]>,
    ) -> Result<ExchangeTokens>;
}
