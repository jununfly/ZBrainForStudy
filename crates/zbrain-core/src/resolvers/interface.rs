//! Resolver SDK — typed interface for external lookups.
//!
//! Ported from TS `src/core/resolvers/interface.ts`. A Resolver maps a typed
//! input (type-erased to `serde_json::Value` at the registry boundary, exactly
//! like the TS `Resolver<unknown, unknown>` erasure) to a `ResolverResult` with
//! confidence + provenance.
//!
//! Design rules (mirrors TS header):
//!   - Every result carries confidence (0.0-1.0) and source attribution.
//!   - LLM-backed resolvers return confidence < 1.0 by convention; deterministic
//!     backends (brain-local, direct API match) return 1.0.
//!   - `raw` preserves the full upstream response for put_raw_data sidecars.
//!
//! Sync-by-default. ScheduledResolver (later PR) layers cron/idempotency/retry
//! on top via Minions. Read-only lookups do not pay queue latency.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::Value as Json;
use tokio::sync::Notify;

// ---------------------------------------------------------------------------
// Cost tiers
// ---------------------------------------------------------------------------

/// Cost tier of a resolver backend. Mirrors TS `ResolverCost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverCost {
    Free,
    RateLimited,
    Paid,
}

impl ResolverCost {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::RateLimited => "rate-limited",
            Self::Paid => "paid",
        }
    }
}

impl std::fmt::Display for ResolverCost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Result of a successful resolve. `value` is type-erased JSON (the caller
/// deserializes into its expected output shape). Mirrors TS `ResolverResult<O>`
/// with `value` carrying the generic `O`.
#[derive(Debug, Clone)]
pub struct ResolverResult {
    pub value: Json,
    /// 0.0-1.0. 1.0 = deterministic ground truth (direct API response,
    /// brain-local slug lookup). <1.0 = inferred (LLM extraction, fuzzy match,
    /// heuristic). Callers use this to gate auto-writes (e.g. `zbrain integrity
    /// --auto` only applies confidence >= threshold). Mirrors TS `confidence`.
    pub confidence: f64,
    /// Stable backend id, e.g. "x-api-v2", "brain-local", "head-check".
    /// Mirrors TS `source`.
    pub source: String,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    /// Estimated dollar cost of this call. 0 for free/rate-limited backends.
    pub cost_estimate: Option<f64>,
    /// Full upstream response, for put_raw_data sidecar preservation. Unused if empty.
    pub raw: Option<Json>,
}

// ---------------------------------------------------------------------------
// Context — flows through every resolve() call
// ---------------------------------------------------------------------------

/// Logger sink passed through every resolve() call. All methods default to
/// no-ops so a resolver can log without requiring the caller to wire a sink.
/// Mirrors TS `ResolverLogger`.
pub trait ResolverLogger: Send + Sync {
    fn debug(&self, _msg: &str, _meta: Option<&Json>) {}
    fn info(&self, _msg: &str, _meta: Option<&Json>) {}
    fn warn(&self, _msg: &str, _meta: Option<&Json>) {}
    fn error(&self, _msg: &str, _meta: Option<&Json>) {}
}

/// No-op logger. The default for [`ResolverContext`].
pub struct NoopResolverLogger;
impl ResolverLogger for NoopResolverLogger {}

/// Context flowing through every resolve() call. Mirrors TS `ResolverContext`,
/// with the TS `engine?` / `storage?` / `deadline?` / `signal?` fields deferred
/// to the slices that need them (engine/storage are out of scope for the
/// resolver migration; `signal` lands with url_reachable in 1-6-4-10-2). The
/// `secret` resolver is a closure so resolvers stay transport/IO-agnostic and
/// fully offline-testable (tests inject a closure returning a canned token).
#[derive(Clone)]
pub struct ResolverContext {
    pub config: Json,
    pub logger: Arc<dyn ResolverLogger>,
    pub request_id: String,
    /// True = untrusted caller (MCP, HTTP). Resolvers that write or enumerate
    /// sensitive paths MUST tighten behavior when remote=true. Mirrors TS and
    /// feeds every security gate (SSRF, path traversal, auto-link skip).
    pub remote: bool,
    pub deadline: Option<SystemTime>,
    /// Resolves a secret by name (e.g. "X_API_BEARER_TOKEN"). Provided by a
    /// closure so it is injectable in tests. Mirrors `ctx.secret()` in TS.
    pub secret: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    /// Shared abort switch. Mirrors `ctx.signal` (AbortSignal) in TS. Resolvers
    /// race their in-flight transport against this; the live `HttpClient`
    /// honors it directly. Defaults to a fresh, never-fired `Notify`.
    pub abort: Arc<Notify>,
}

impl ResolverContext {
    pub fn new() -> Self {
        Self {
            config: Json::Object(Default::default()),
            logger: Arc::new(NoopResolverLogger),
            request_id: "anon".to_string(),
            remote: false,
            deadline: None,
            secret: Arc::new(|_| None),
            abort: Arc::new(Notify::new()),
        }
    }

    /// Resolve a secret by name. Returns `None` when unset.
    pub fn secret(&self, name: &str) -> Option<String> {
        (self.secret)(name)
    }
}

impl Default for ResolverContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// Per-call request handed to [`Resolver::resolve`]. Carries the (erased)
/// input, the shared context, and an optional per-call timeout. Mirrors TS
/// `ResolverRequest<I>`. Only `Clone` is derived (not `Debug`) because
/// [`ResolverContext`] intentionally omits `Debug` — its `Arc<dyn Fn>` secret
/// resolver and `Arc<dyn ResolverLogger>` are not `Debug`.
#[derive(Clone)]
pub struct ResolverRequest {
    pub input: Json,
    pub context: ResolverContext,
    pub timeout_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Resolver trait
// ---------------------------------------------------------------------------

/// A Resolver maps an erased input to a [`ResolverResult`]. Implementations
/// live under `crate::resolvers::*` (builtin) or are registered at runtime.
/// Mirrors TS `Resolver<I, O>` — generic `I`/`O` are erased to JSON at this
/// boundary so the registry can hold heterogeneous resolvers in one map.
#[async_trait::async_trait]
pub trait Resolver: Send + Sync {
    /// Stable id, slug-cased. e.g. "x_handle_to_tweet", "url_reachable". Used
    /// for registry + metrics. Mirrors TS `id`.
    fn id(&self) -> &str;
    /// Cost tier. Mirrors TS `cost`.
    fn cost(&self) -> ResolverCost;
    /// Backend label — "x-api-v2", "perplexity", "brain-local", "head-check".
    fn backend(&self) -> &str;
    /// Optional description for `zbrain resolvers list`.
    fn description(&self) -> Option<&str> {
        None
    }
    /// Optional JSON Schema (loose) for input validation. Caller may inspect.
    fn input_schema(&self) -> Option<&Json> {
        None
    }
    fn output_schema(&self) -> Option<&Json> {
        None
    }

    /// Can this resolver run in the given context? Registry.resolve() calls
    /// this before resolve() — an unavailable resolver yields `Unavailable`.
    async fn available(&self, ctx: &ResolverContext) -> bool;

    async fn resolve(&self, req: ResolverRequest) -> Result<ResolverResult, ResolverError>;
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error codes for resolver failures. Mirrors TS `ResolverErrorCode` exactly
/// (9 variants). There is deliberately NO `config` code — missing
/// config/token surfaces as [`ResolverErrorCode::Unavailable`] (see the x_api
/// slice 1-6-4-10-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverErrorCode {
    NotFound,       // registry.get on unknown id
    AlreadyRegistered,
    Unavailable,    // available() returned false
    Timeout,
    RateLimited,
    Auth,           // API rejected credentials
    Schema,         // malformed response / schema validation failed
    Aborted,        // AbortSignal fired
    Upstream,       // generic upstream failure (network, 5xx)
}

impl ResolverErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::AlreadyRegistered => "already_registered",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::Auth => "auth",
            Self::Schema => "schema",
            Self::Aborted => "aborted",
            Self::Upstream => "upstream",
        }
    }
}

impl std::fmt::Display for ResolverErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolver error. Mirrors TS `ResolverError` (code + message + resolverId).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverError {
    pub code: ResolverErrorCode,
    pub message: String,
    pub resolver_id: Option<String>,
    pub cause: Option<String>,
}

impl ResolverError {
    pub fn new(code: ResolverErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            resolver_id: None,
            cause: None,
        }
    }

    pub fn with_resolver(
        code: ResolverErrorCode,
        message: impl Into<String>,
        resolver_id: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            resolver_id: Some(resolver_id.into()),
            cause: None,
        }
    }

    pub fn with_cause(code: ResolverErrorCode, message: impl Into<String>, cause: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            resolver_id: None,
            cause: Some(cause.into()),
        }
    }
}

impl std::fmt::Display for ResolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ResolverError {}
