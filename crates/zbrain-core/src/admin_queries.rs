//! Admin dashboard data-access trait.
//!
//! Separate from `BrainEngine` because admin queries read
//! `oauth_clients` / `access_tokens` / `mcp_request_log` / `api_keys`
//! tables — a different concern from brain content (pages, tags, files).
//!
//! Defined in zbrain-core so both the engine and zbrain-web can depend on
//! it without a circular dependency.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::error::Result;

// ── value types ───────────────────────────────────────────────────────────

/// Dashboard summary statistics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub connected_agents: i64,
    pub active_tokens: i64,
    pub active_api_keys: i64,
    pub requests_today: i64,
}

/// Full health probe (mirrors TS `probeHealth`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FullStats {
    pub page_count: i64,
    pub chunk_count: i64,
    pub engine_ok: bool,
}

/// Early-warning indicators: tokens expiring soon, elevated error rate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthIndicators {
    pub expiring_soon: i64,
    pub error_rate: f64,
}

/// Unified agent entry (OAuth client or legacy API key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    /// `"oauth"` or `"api_key"`.
    pub auth_type: String,
    /// Last time this agent was used (ISO-8601 or null).
    pub last_used_at: Option<String>,
    /// For OAuth agents: the tool method they use most.
    pub method: Option<String>,
}

/// API key row from `access_tokens`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// Paginated response envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub limit: u32,
}

/// Filters for `list_requests`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLogFilters {
    pub source: Option<String>,
    pub method: Option<String>,
    pub status: Option<String>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

impl RequestLogFilters {
    pub fn page(&self) -> u32 { self.page.unwrap_or(1).max(1) }
    pub fn limit(&self) -> u32 { self.limit.unwrap_or(50).min(100).max(1) }
    pub fn offset(&self) -> u32 { (self.page() - 1) * self.limit() }
}

/// A single request log entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestLogEntry {
    pub id: i64,
    pub token_name: Option<String>,
    pub agent_name: Option<String>,
    pub operation: String,
    pub latency_ms: Option<i64>,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: String,
}

// ── trait ─────────────────────────────────────────────────────────────────

/// Admin-oriented queries against OAuth/API-key/request-log tables.
///
/// Object-safe — all methods can be called through `dyn AdminQueries`.
#[async_trait]
pub trait AdminQueries: Debug + Send + Sync {
    /// Dashboard summary: agent count, active tokens, API keys, today's requests.
    async fn get_stats(&self) -> Result<Stats>;

    /// Full health probe including page/chunk count and engine status.
    async fn get_full_stats(&self) -> Result<FullStats>;

    /// Early-warning indicators (expiring tokens, error rate).
    async fn check_health_indicators(&self) -> Result<HealthIndicators>;

    /// Unified agent list (OAuth clients + legacy API keys).
    async fn list_agents(&self) -> Result<Vec<AgentInfo>>;

    /// List non-revoked API keys (from `access_tokens`).
    async fn list_api_keys(&self) -> Result<Vec<ApiKey>>;

    /// Create a new API key (inserts into `access_tokens`).
    /// Returns the created key with its id and generated token.
    async fn create_api_key(&self, name: &str) -> Result<ApiKey>;

    /// Revoke an API key by name (sets `revoked_at`).
    async fn revoke_api_key(&self, name: &str) -> Result<()>;

    /// Paginated request log with optional filters.
    async fn list_requests(&self, filters: &RequestLogFilters) -> Result<Paginated<RequestLogEntry>>;
}
