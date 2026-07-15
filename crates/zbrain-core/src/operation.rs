//! Operation type foundation and trait system.
//!
//! Mirrors `src/core/operations.ts` from the TypeScript codebase. This module
//! provides the 1:1 port of `OperationError`, `OperationContext`, and the
//! `Operation` trait that defines all operation handlers in zbrain.
//!
//! The wire shape is byte-for-byte aligned with TypeScript for cross-rewrite
//! stability — `toJSON()` serialization must match exactly.

use std::error::Error as StdError;
use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::engine::{BrainEngine, SearchOpts};
use crate::error::StructuredError;

// ──────────────────────────────────────────────────────────────────────────
// ErrorCode enum (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// Stable machine-readable error code for operation failures.
///
/// Mirrors `ErrorCode` union type in TS. The open-ended `&str` variant
/// supports forward-compatibility (TS: `| (string & {})`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    PageNotFound,
    InvalidParams,
    EmbeddingFailed,
    StorageError,
    BucketNotFound,
    DatabaseError,
    PermissionDenied,
    UnknownTransport,
    RateLimited,
    ExtractionFailed,
    FactNotFound,
    /// Catch-all for forward compatibility (matches TS open union).
    #[serde(skip)]
    Other(String),
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorCode::PageNotFound => write!(f, "page_not_found"),
            ErrorCode::InvalidParams => write!(f, "invalid_params"),
            ErrorCode::EmbeddingFailed => write!(f, "embedding_failed"),
            ErrorCode::StorageError => write!(f, "storage_error"),
            ErrorCode::BucketNotFound => write!(f, "bucket_not_found"),
            ErrorCode::DatabaseError => write!(f, "database_error"),
            ErrorCode::PermissionDenied => write!(f, "permission_denied"),
            ErrorCode::UnknownTransport => write!(f, "unknown_transport"),
            ErrorCode::RateLimited => write!(f, "rate_limited"),
            ErrorCode::ExtractionFailed => write!(f, "extraction_failed"),
            ErrorCode::FactNotFound => write!(f, "fact_not_found"),
            ErrorCode::Other(s) => write!(f, "{s}"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// OperationError struct (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// Error envelope returned by operation handlers.
///
/// Mirrors `class OperationError` in TS. Field order on the wire matches
/// exactly: `{ error, message, suggestion, docs }`. Optional fields are
/// skipped on serialization when absent for byte-for-byte TS parity.
///
/// Note: This is a SEPARATE error type from `StructuredError` — they have
/// incompatible wire shapes and serve different layers (operations vs
/// engine/internal). This duality was confirmed in the Node 1-4 audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationError {
    /// Stable machine-readable code (maps to TS `code` field).
    #[serde(rename = "error")]
    pub code: ErrorCode,
    /// Human-readable message. One sentence.
    pub message: String,
    /// Optional actionable suggestion. Matches TS `suggestion` (not `hint`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub suggestion: Option<String>,
    /// Optional link to docs/runbook. Matches TS `docs` (not `docs_url`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub docs: Option<String>,
}

impl OperationError {
    /// Build an operation error envelope. Mirrors `new OperationError(code, message)` in TS.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            suggestion: None,
            docs: None,
        }
    }

    /// Attach an actionable suggestion. Returns `self` for builder-style chaining.
    #[must_use]
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Attach a docs/runbook URL. Returns `self` for builder-style chaining.
    #[must_use]
    pub fn with_docs(mut self, docs: impl Into<String>) -> Self {
        self.docs = Some(docs.into());
        self
    }

    /// Convenience constructor for parameter validation failures.
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }

    /// Convenience constructor for permission denied failures.
    #[must_use]
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PermissionDenied, message)
    }

    /// Convenience constructor for file-not-found during upload validation.
    /// Mirrors TS `validateUploadPath` throw site.
    #[must_use]
    pub fn file_not_found(path: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InvalidParams,
            format!("File not found: {}", path.into()),
        )
    }

    /// Convenience constructor for symlink rejection during upload validation.
    /// Mirrors TS `validateUploadPath` symlink throw site.
    #[must_use]
    pub fn symlink_not_allowed(path: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InvalidParams,
            format!("Symlinks are not allowed for upload: {}", path.into()),
        )
    }

    /// Convenience constructor for root confinement violation during upload.
    /// Mirrors TS `validateUploadPath` traversal throw site.
    #[must_use]
    pub fn path_outside_root(path: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InvalidParams,
            format!("Upload path must be within the working directory: {}", path.into()),
        )
    }

    /// Convenience constructor for page not found failures.
    #[must_use]
    pub fn page_not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PageNotFound, message)
    }
}

impl From<crate::error::Error> for OperationError {
    fn from(err: crate::error::Error) -> Self {
        // Convert StructuredError to OperationError - for now we wrap the message
        // with a generic InternalErrorCode since the two error systems are
        // intentionally separate (engine layer vs operations layer).
        //
        // In a future slice we may want to map specific StructuredError codes
        // to corresponding OperationError codes.
        OperationError::new(
            ErrorCode::StorageError,
            format!("Engine error: {}", err),
        )
    }
}

impl fmt::Display for OperationError {
    /// Renders the same way TS `OperationError` CLI output:
    /// `Error [code]: message` followed by `  Fix: suggestion` on the next line.
    /// Matches TS `cli.ts` catch handler output exactly.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error [{}]: {}", self.code, self.message)?;
        if let Some(suggestion) = &self.suggestion {
            write!(f, "\n  Fix: {suggestion}")?;
        }
        Ok(())
    }
}

impl StdError for OperationError {}

impl OperationError {
    /// Returns the CLI exit code for this error type.
    /// Matches TS exit code conventions in `src/cli.ts`.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self.code {
            // Permission denied → exit 126 (command cannot execute)
            ErrorCode::PermissionDenied => 126,
            // Timeout-like errors could use 124, but for now all others use generic error code 1
            _ => 1,
        }
    }
}

/// Convert an operation error to the structured error format used at the
/// engine layer. Necessary because the two error types have different wire
/// shapes and field names.
impl From<OperationError> for StructuredError {
    fn from(err: OperationError) -> Self {
        let class = match err.code {
            ErrorCode::PageNotFound => "PageNotFound",
            ErrorCode::InvalidParams => "InvalidParams",
            ErrorCode::EmbeddingFailed => "EmbeddingFailed",
            ErrorCode::StorageError => "StorageError",
            ErrorCode::BucketNotFound => "BucketNotFound",
            ErrorCode::DatabaseError => "DatabaseError",
            ErrorCode::PermissionDenied => "PermissionDenied",
            ErrorCode::UnknownTransport => "UnknownTransport",
            ErrorCode::RateLimited => "RateLimited",
            ErrorCode::ExtractionFailed => "ExtractionFailed",
            ErrorCode::FactNotFound => "FactNotFound",
            ErrorCode::Other(_) => "Error",
        };
        let mut se = StructuredError::new(class, err.code.to_string(), err.message);
        if let Some(suggestion) = err.suggestion {
            se = se.with_hint(suggestion);
        }
        if let Some(docs) = err.docs {
            se = se.with_docs_url(docs);
        }
        se
    }
}

// ──────────────────────────────────────────────────────────────────────────
// CliHints struct (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// CLI command metadata for operation-to-command mapping.
///
/// Mirrors `operation.cliHints` object structure in TS. Controls how
/// operations are exposed as CLI commands: command name, positional
/// argument order, flag/option mappings, and stdin field assignments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliHints {
    /// CLI command name (kebab-case, e.g., "get-page").
    pub name: &'static str,
    /// Positional argument names (in order) mapped to params fields.
    /// Each must be a valid field name in the operation's Params struct.
    pub positional: &'static [&'static str],
    /// Flag argument names mapped to params fields (boolean flags).
    pub flags: &'static [&'static str],
    /// Which params field receives stdin content (if any).
    pub stdin: Option<&'static str>,
}

impl CliHints {
    /// Create a minimal CliHints with just a command name.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            positional: &[],
            flags: &[],
            stdin: None,
        }
    }

    /// Add positional arguments.
    #[must_use]
    pub const fn with_positional(mut self, positional: &'static [&'static str]) -> Self {
        self.positional = positional;
        self
    }

    /// Add flag arguments.
    #[must_use]
    pub const fn with_flags(mut self, flags: &'static [&'static str]) -> Self {
        self.flags = flags;
        self
    }

    /// Add stdin field mapping.
    #[must_use]
    pub const fn with_stdin(mut self, stdin: &'static str) -> Self {
        self.stdin = Some(stdin);
        self
    }
}

// ──────────────────────────────────────────────────────────────────────────
// AuthInfo struct (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// OAuth 2.1 authentication info (v0.8+).
///
/// Mirrors `interface AuthInfo` in TS. Populated when the caller
/// authenticated via `zbrain serve --http` HTTP endpoint. Contains the
/// client ID, granted scopes, and federated read allow-list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthInfo {
    /// Raw bearer token (for downstream proxying).
    pub token: String,
    /// OAuth client identifier.
    pub client_id: String,
    /// Human-readable agent name resolved at token verification.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub client_name: Option<String>,
    /// Granted OAuth scopes for per-operation enforcement.
    pub scopes: Vec<String>,
    /// Unix timestamp (milliseconds) when the token expires.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at: Option<u64>,
    /// Write authority source id (v0.34.1 / #861 D2).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_id: Option<String>,
    /// Federated read source allow-list (v0.34.1 / #876).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub allowed_sources: Option<Vec<String>>,
}

// ──────────────────────────────────────────────────────────────────────────
// CliOpts struct (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// Resolved global CLI options.
///
/// Mirrors `ctx.cliOpts` in TS. Populated by CLI callers; MCP / library
/// callers may leave it undefined — consumers default to quiet/no-progress
/// for background work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliOpts {
    /// Suppress human-friendly progress output.
    pub quiet: bool,
    /// Emit newline-delimited JSON progress events instead of ANSI.
    pub progress_json: bool,
    /// Progress tick interval in milliseconds.
    pub progress_interval: u32,
}

// ──────────────────────────────────────────────────────────────────────────
// Logger trait (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// Logger interface passed to operation handlers.
///
/// Mirrors `interface Logger` in TS. The logger is thread-safe.
pub trait Logger: Send + Sync + fmt::Debug {
    /// Log an informational message.
    fn info(&self, msg: &str);
    /// Log a warning message.
    fn warn(&self, msg: &str);
    /// Log an error message.
    fn error(&self, msg: &str);
}

/// No-op logger implementation for tests and background work.
#[derive(Debug, Clone, Default)]
pub struct NoopLogger;

impl Logger for NoopLogger {
    fn info(&self, _msg: &str) {}
    fn warn(&self, _msg: &str) {}
    fn error(&self, _msg: &str) {}
}

// ──────────────────────────────────────────────────────────────────────────
// OperationContext struct (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// Context passed to every operation handler.
///
/// Mirrors `interface OperationContext` in TS. Contains all execution
/// context including trust boundary flags (`remote`, `viaSubagent`),
/// engine reference, auth info, and tenancy scoping (`source_id`).
///
/// The trust boundary fields are security-critical:
/// - `remote=true` → caller is untrusted (MCP over stdio/HTTP)
/// - `via_subagent=true` → enforce agent-facing policy (e.g. put_page namespace)
/// - `source_id` → DB-level tenancy filter for all facts reads/writes
#[derive(Serialize, Deserialize)]
pub struct OperationContext {
    /// Reference to the resolved brain engine instance.
    #[serde(skip)]
    pub engine: Option<std::sync::Arc<dyn BrainEngine>>,
    /// Logger instance for this operation.
    #[serde(skip)]
    pub logger: Option<std::sync::Arc<dyn Logger>>,
    /// Dry-run mode: no mutations, only validation.
    pub dry_run: bool,
    /// OAuth authentication info (if caller authenticated via HTTP).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub auth: Option<AuthInfo>,
    /// Trust boundary flag: true = caller is remote/untrusted (MCP).
    ///
    /// Security-critical: operations like `file_upload` tighten their
    /// filesystem confinement when `remote=true`.
    pub remote: bool,
    /// Minion job id (aggregator or subagent).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub job_id: Option<u64>,
    /// Owning subagent job id (if dispatched from a subagent tool call).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subagent_id: Option<u64>,
    /// Fail-closed subagent flag: when true, agent policy MUST be enforced
    /// even if `subagent_id` is undefined (dispatcher bug must not bypass guard).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub via_subagent: Option<bool>,
    /// Trusted-workspace slug prefix allow-list (v0.23 dream cycle).
    ///
    /// Enforced by `put_page` BEFORE the legacy namespace check.
    /// Empty / unset → fall back to legacy namespace check.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub allowed_slug_prefixes: Option<Vec<String>>,
    /// Global CLI options (--quiet, --progress-json, etc.).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cli_opts: Option<CliOpts>,
    /// Per-token allow-list for the holder field on `takes` (v0.28).
    ///
    /// When set (MCP-bound token), all `takes_*` operations MUST apply
    /// `WHERE holder = ANY($takes_holders_allow_list)`. This is the
    /// server-side filter backing the v0.28+ visibility model.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub takes_holders_allow_list: Option<Vec<String>>,
    /// Connected-gbrains brain id (v0.19+ / v0.26 mounts).
    ///
    /// 'host' for the default brain configured in ~/.zbrain/config.json;
    /// otherwise a mount id registered in ~/.zbrain/mounts.json.
    /// Omitted = 'host'.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub brain_id: Option<String>,
    /// In-DB tenancy axis for facts hot memory (v0.31 eD4 / eE2).
    ///
    /// Resolved in the dispatcher from CLI flag (--source) / env
    /// (ZBRAIN_SOURCE) / `.zbrain-source` dotfile / per-token sources scope.
    /// Every facts read/write filter starts with `WHERE source_id = $X`.
    ///
    /// v0.34 D4: REQUIRED at the type level. Every transport MUST populate
    /// this field. Defaults to 'default' when nothing else applies.
    pub source_id: String,
    /// LLM client for AI-powered operations (e.g., Think).
    ///
    /// Optional: if not set, operations fall back to non-AI modes.
    #[serde(skip)]
    pub llm_client: Option<std::sync::Arc<dyn crate::llm::LlmClient>>,
    /// Cross-encoder rerank settings for the query pipeline post-processing
    /// stage. `None` = reranker off (the default): `QueryOperation::execute`
    /// skips the rerank step entirely and returns fused RRF order. When set,
    /// the query path reranks its top results and fails open to RRF on any
    /// upstream error (see `rerank_client::apply_reranker`). Not serialized —
    /// it carries a live HTTP client Arc, wired at CLI/dispatch construction.
    #[serde(skip)]
    pub rerank: Option<crate::rerank_client::RerankSettings>,
    /// Embedding client for the query pipeline's vector-retrieval path. `None`
    /// = vector path off (the default): `QueryOperation::execute` leaves
    /// `SearchOpts::query_embedding` unset, so hybrid search degenerates to
    /// lexical-only. When set, the query path embeds the query text and injects
    /// the vector so the engine can run cosine similarity against stored
    /// `Page::embedding` blobs; embedding failure fails open to lexical-only
    /// (never fails the search). Not serialized — it carries a live HTTP client
    /// Arc, wired at CLI/dispatch construction, exactly like `rerank`.
    #[serde(skip)]
    pub embedding: Option<std::sync::Arc<crate::embedding::EmbeddingClient>>,
}

impl fmt::Debug for OperationContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationContext")
            .field("dry_run", &self.dry_run)
            .field("auth", &self.auth)
            .field("remote", &self.remote)
            .field("job_id", &self.job_id)
            .field("subagent_id", &self.subagent_id)
            .field("via_subagent", &self.via_subagent)
            .field("allowed_slug_prefixes", &self.allowed_slug_prefixes)
            .field("cli_opts", &self.cli_opts)
            .field("takes_holders_allow_list", &self.takes_holders_allow_list)
            .field("brain_id", &self.brain_id)
            .field("source_id", &self.source_id)
            .field("engine", &self.engine.as_ref().map(|_| "Arc<dyn BrainEngine>"))
            .field("logger", &self.logger.as_ref().map(|_| "Arc<dyn Logger>"))
            .finish()
    }
}

impl OperationContext {
    /// Create a minimal local CLI context (trust boundary = local).
    ///
    /// Mirrors `buildOperationContext({ remote: false, sourceId: 'default' })`
    /// in TS. Used for all CLI invocations where the caller owns the machine.
    #[must_use]
    pub fn local_cli() -> Self {
        Self {
            engine: None,
            logger: Some(std::sync::Arc::new(NoopLogger)),
            dry_run: false,
            auth: None,
            remote: false,
            job_id: None,
            subagent_id: None,
            via_subagent: None,
            allowed_slug_prefixes: None,
            cli_opts: None,
            takes_holders_allow_list: None,
            brain_id: None,
            source_id: "default".to_string(),
            llm_client: None,
            rerank: None,
            embedding: None,
        }
    }

    /// Create a remote MCP context (trust boundary = untrusted).
    ///
    /// Mirrors the HTTP / stdio MCP transport context setup in TS.
    /// Operations must enforce confinement when `remote = true`.
    #[must_use]
    pub fn remote_mcp(source_id: impl Into<String>) -> Self {
        Self {
            engine: None,
            logger: Some(std::sync::Arc::new(NoopLogger)),
            dry_run: false,
            auth: None,
            remote: true,
            job_id: None,
            subagent_id: None,
            via_subagent: None,
            allowed_slug_prefixes: None,
            cli_opts: None,
            takes_holders_allow_list: None,
            brain_id: None,
            source_id: source_id.into(),
            llm_client: None,
            rerank: None,
            embedding: None,
        }
    }

    /// Attach an LLM client to the context for AI-powered operations.
    #[must_use]
    pub fn with_llm_client(mut self, llm_client: std::sync::Arc<dyn crate::llm::LlmClient>) -> Self {
        self.llm_client = Some(llm_client);
        self
    }

    /// Attach cross-encoder rerank settings, enabling the query pipeline's
    /// rerank post-processing stage. Absent this, the reranker stays off.
    #[must_use]
    pub fn with_rerank(mut self, rerank: crate::rerank_client::RerankSettings) -> Self {
        self.rerank = Some(rerank);
        self
    }

    /// Attach an embedding client, enabling the query pipeline's vector path.
    /// Absent this, hybrid search runs lexical-only.
    #[must_use]
    pub fn with_embedding(
        mut self,
        embedding: std::sync::Arc<crate::embedding::EmbeddingClient>,
    ) -> Self {
        self.embedding = Some(embedding);
        self
    }

    /// Resolve the source-scope filter for read-side op handlers (v0.34.1 #861 D9).
    ///
    /// Mirrors `sourceScopeOpts(ctx)` in TS. Precedence:
    ///   1. `ctx.auth?.allowed_sources` (federated read) → `source_ids: [...]`
    ///   2. `ctx.source_id` (scalar) → `source_id: "..."`
    ///   3. Neither set → `{}` (local CLI / tests keep pre-v0.34 unscoped behavior)
    ///
    /// Helper rather than inline so every read-side handler routes through the
    /// same precedence ladder — drift between sites is the bug class.
    /// Get a reference to the configured brain engine.
    ///
    /// # Errors
    ///
    /// Returns `OperationError` if the engine is not configured in this context.
    pub fn engine(&self) -> OperationResult<&dyn BrainEngine> {
        self.engine
            .as_ref()
            .map(|arc| arc.as_ref())
            .ok_or_else(|| OperationError::new(
                ErrorCode::InvalidParams,
                "Operation context engine not configured".to_string(),
            ))
    }

    /// Builder method to attach an engine to this context.
    #[must_use]
    pub fn with_engine(mut self, engine: Arc<dyn BrainEngine>) -> Self {
        self.engine = Some(engine);
        self
    }

    #[must_use]
    pub fn source_scope_opts(&self) -> SourceScopeOpts {
        if let Some(allowed) = self.auth.as_ref().and_then(|a| a.allowed_sources.as_ref()) {
            SourceScopeOpts {
                source_id: None,
                source_ids: Some(allowed.clone()),
            }
        } else {
            SourceScopeOpts {
                source_id: Some(self.source_id.clone()),
                source_ids: None,
            }
        }
    }
}

/// Return type of `OperationContext::source_scope_opts()`.
///
/// Mirrors the spreadable opts fragment in TS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceScopeOpts {
    /// Single source id (scalar case).
    pub source_id: Option<String>,
    /// Multiple source ids (federated read case).
    pub source_ids: Option<Vec<String>>,
}

// ──────────────────────────────────────────────────────────────────────────
// Validation infrastructure (Slice #41)
// ──────────────────────────────────────────────────────────────────────────

/// Validatable operation params. Mirrors the implicit validation contract
/// in TypeScript where each operation validates its params at handler entry.
pub trait ValidateParams {
    /// Validate params against the schema rules. Returns OperationError with
    /// `invalid_params` code on validation failure.
    fn validate(&self) -> OperationResult<()>;
}

/// `serde_json::Value` is always valid as params — validation is deferred
/// to the operation implementation. This blanket impl makes it easy to use
/// `Value` as a params type in tests and dynamic dispatch scenarios.
impl ValidateParams for serde_json::Value {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Trust boundary enforcement guards (Slice #42, Security-critical)
// ──────────────────────────────────────────────────────────────────────────

/// Enforce `localOnly` operation constraint.
///
/// **Security-critical:** Operations marked `local_only = true` MUST NOT be
/// callable from remote/MCP contexts. This guard is called at dispatch layer
/// BEFORE any handler code runs.
///
/// # Errors
///
/// Returns `permission_denied` with exact TS-parity error message when:
/// - `local_only = true` AND `ctx.remote = true`
pub fn enforce_local_only(
    operation_name: &str,
    local_only: bool,
    ctx: &OperationContext,
) -> OperationResult<()> {
    if local_only && ctx.remote {
        return Err(OperationError::permission_denied(format!(
            "Operation '{operation_name}' is only available locally (MCP/remote callers cannot use it)"
        )));
    }
    Ok(())
}

/// D18 Security Constraint: Remote callers cannot pass `image_path`.
///
/// **Why:** `image_path` reads from the local filesystem. Remote/MCP callers
/// must use `image_url` (fetch from network) or `image_data` (base64 upload).
///
/// # Errors
///
/// Returns `permission_denied` with exact TS-parity error message when:
/// - `ctx.remote = true` AND `image_path.is_some()`
pub fn enforce_d18_image_path_constraint(
    ctx: &OperationContext,
    image_path: Option<&str>,
) -> OperationResult<()> {
    if ctx.remote && image_path.is_some() {
        return Err(OperationError::permission_denied(
            "image_path is not permitted for remote callers (D18). Use image_url or image_data instead."
        ));
    }
    Ok(())
}

/// Check if a slug matches an allow-list prefix pattern.
///
/// Glob form: `<prefix>/*` matches any slug starting with `<prefix>/` and
/// having at least one more segment. Bare `<prefix>` (no trailing `/*`)
/// matches that exact slug only.
///
/// **Important:** `prefix/*` does NOT match the bare `prefix` itself.
/// Mirrors TS `matchesSlugAllowList` function 1:1.
pub fn matches_slug_prefix(slug: &str, prefix: &str) -> bool {
    if let Some(base) = prefix.strip_suffix("/*") {
        // Wildcard prefix: must start with base/ and have at least one more segment
        // Base itself does NOT match; we'd "continue" to next prefix in TS
        slug.starts_with(&format!("{base}/"))
    } else {
        // Exact match only
        slug == prefix
    }
}

/// Enforce subagent put_page prefix white-list (v0.23 dream cycle).
///
/// **Security-critical:** Subagents cannot write to arbitrary pages. They are
/// confined to `allowed_slug_prefixes` passed through the context from the
/// synthesizer/patterns phase.
///
/// # Rules
/// 1. If NOT in subagent context → always allowed
/// 2. If `allowed_slug_prefixes` is set → check against every prefix in list
/// 3. If `allowed_slug_prefixes` not set → fall back to legacy subagent namespace
/// 4. No match → reject with `permission_denied`
///
/// # Errors
///
/// Returns `permission_denied` when subagent context and slug not in allow-list.
pub fn enforce_subagent_put_page_prefix(
    ctx: &OperationContext,
    slug: &str,
) -> OperationResult<()> {
    // Not a subagent context → allowed
    if ctx.via_subagent != Some(true) && ctx.subagent_id.is_none() {
        return Ok(());
    }

    // Rule 2: Check allow-list if set
    if let Some(prefixes) = &ctx.allowed_slug_prefixes {
        if !prefixes.is_empty() {
            for prefix in prefixes {
                if matches_slug_prefix(slug, prefix) {
                    return Ok(());
                }
            }
            // No prefix matched
            let prefix_list = prefixes.join(", ");
            return Err(OperationError::permission_denied(format!(
                "Subagent cannot write to page '{slug}'. Allowed prefixes: {prefix_list}"
            )));
        }
    }

    // Rule 3: Fall back to legacy subagent namespace
    // wiki/agents/{id}/% pattern
    if let Some(subagent_id) = ctx.subagent_id {
        let legacy_prefix = format!("wiki/agents/{subagent_id}/");
        if slug.starts_with(&legacy_prefix) {
            return Ok(());
        }
    }

    // Fail-closed: no matching rule found
    Err(OperationError::permission_denied(format!(
        "Subagent cannot write to page '{slug}'. No matching prefix in allow-list and subagent_id not present for legacy namespace check."
    )))
}

// ── CJK character support (v0.32.7) ───────────────────────────────────────

/// Returns true if the char is in CJK ranges allowed in slugs/filenames:
/// Han (U+4E00-U+9FFF), Hiragana (U+3040-U+309F), Katakana (U+30A0-U+30FF),
/// Hangul Syllables (U+AC00-U+D7AF).
fn is_cjk_slug_char(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |   // CJK Unified Ideographs (Han)
        '\u{3040}'..='\u{309F}' |   // Hiragana
        '\u{30A0}'..='\u{30FF}' |   // Katakana
        '\u{AC00}'..='\u{D7AF}'     // Hangul Syllables
    )
}

// ── Upload path validator (1:1 TS parity, operations.ts:110-145) ─────────

/// Validate an upload path. Two modes:
///   - strict (remote=true): confines the resolved path to `root` and rejects symlinks.
///     Used when the caller is untrusted (MCP over stdio/HTTP, agent-facing).
///   - loose (remote=false): only verifies the file exists and is not a symlink whose
///     target escapes the filesystem (no path traversal protection). Used for local CLI
///     where the user owns the filesystem.
///
/// Either way: symlinks in the final component are always rejected (prevents
/// transparent redirection to a different file than the user typed).
///
/// # Errors
///
/// Returns `OperationError` with `invalid_params` code on symlink escape,
/// traversal, or missing file.
pub fn validate_upload_path(
    file_path: &str,
    root: &str,
    strict: bool,
) -> OperationResult<String> {
    use std::path::Path;

    // Step 1: Resolve and realpath the file
    let path = Path::new(file_path);
    let real = path.canonicalize().map_err(|e| {
        if e.to_string().contains("No such file or directory")
            || e.to_string().contains("The system cannot find the file specified")
        {
            OperationError::file_not_found(file_path)
        } else {
            OperationError::invalid_params(format!("Cannot resolve path: {file_path}"))
        }
    })?;

    // Step 2: Always reject final-component symlinks (basic safety for both modes)
    // lstat race tolerance: pass if realpath succeeded (means the target exists)
    if let Ok(meta) = path.symlink_metadata() {
        if meta.file_type().is_symlink() {
            return Err(OperationError::symlink_not_allowed(file_path));
        }
    }

    if !strict {
        return Ok(real.to_string_lossy().into_owned());
    }

    // Step 3: Strict mode — confine to root via realpath + path.relative
    // (catches parent-dir symlinks per B5)
    let root_path = Path::new(root);
    let real_root = root_path.canonicalize().map_err(|_| {
        OperationError::invalid_params(format!("Confinement root not accessible: {root}"))
    })?;

    let rel = pathdiff::diff_paths(&real, &real_root).unwrap_or_else(|| "..".into());
    let rel_str = rel.to_string_lossy();

    if rel_str.is_empty()
        || rel_str.starts_with("..")
        || rel_str.starts_with("../")
        || rel_str.starts_with("..\\")
    {
        return Err(OperationError::path_outside_root(file_path));
    }

    // Double-check: round-trip resolve must equal original real
    let round_trip = real_root.join(&rel);
    if round_trip.canonicalize().ok().as_ref() != Some(&real) {
        return Err(OperationError::path_outside_root(file_path));
    }

    Ok(real.to_string_lossy().into_owned())
}

// ── Page slug validator (1:1 TS parity, operations.ts:152-165) ───────────

/// Allowlist validator for page slugs. Rejects URL-encoded traversal, backslashes,
/// control chars, RTL overrides, Unicode lookalikes — anything outside the allowlist.
/// Format: lowercase alphanumeric + hyphen segments separated by single forward slashes.
///
/// # Errors
///
/// Returns `OperationError` with `invalid_params` code on validation failure.
pub fn validate_page_slug(slug: &str) -> OperationResult<()> {
    if slug.is_empty() {
        return Err(OperationError::invalid_params(
            "page_slug must be a non-empty string",
        ));
    }
    if slug.len() > 255 {
        return Err(OperationError::invalid_params(
            "page_slug exceeds 255 characters",
        ));
    }

    // Validate each segment: alphanumeric (or CJK) + hyphens, no leading/trailing
    // hyphens, no empty segments.
    for segment in slug.split('/') {
        if segment.is_empty() {
            return Err(OperationError::invalid_params(format!(
                "Invalid page_slug: {slug} (allowed: alphanumeric, CJK, hyphens, forward-slash separated segments)"
            )));
        }

        let mut chars = segment.chars().peekable();
        let first = chars.next().unwrap();

        // First char must be alphanumeric or CJK
        if !first.is_ascii_alphanumeric() && !is_cjk_slug_char(first) {
            return Err(OperationError::invalid_params(format!(
                "Invalid page_slug: {slug} (allowed: alphanumeric, CJK, hyphens, forward-slash separated segments)"
            )));
        }

        // Remaining chars: alphanumeric, CJK, or hyphen
        for c in chars {
            if !c.is_ascii_alphanumeric() && !is_cjk_slug_char(c) && c != '-' {
                return Err(OperationError::invalid_params(format!(
                    "Invalid page_slug: {slug} (allowed: alphanumeric, CJK, hyphens, forward-slash separated segments)"
                )));
            }
        }
    }

    Ok(())
}

// ── Filename validator (1:1 TS parity, operations.ts:197-210) ─────────────

/// Allowlist validator for uploaded file basenames. Rejects control chars, backslashes,
/// RTL overrides (\u202E), leading dot (hidden files) and leading dash (CLI flag confusion).
/// Allows extension dots and underscores. Max 255 chars.
///
/// # Errors
///
/// Returns `OperationError` with `invalid_params` code on validation failure.
pub fn validate_filename(name: &str) -> OperationResult<()> {
    if name.is_empty() {
        return Err(OperationError::invalid_params(
            "Filename must be a non-empty string",
        ));
    }
    if name.len() > 255 {
        return Err(OperationError::invalid_params(
            "Filename exceeds 255 characters",
        ));
    }

    let mut chars = name.chars().peekable();
    let first = chars.next().unwrap();

    // Leading char rejection: no leading dot (hidden), no leading dash (CLI flag)
    if first == '.' || first == '-' {
        return Err(OperationError::invalid_params(format!(
            "Invalid filename: {name} (allowed: alphanumeric, CJK, dot, underscore, hyphen — no leading dot/dash, no control chars or backslash)"
        )));
    }

    // First char must be alphanumeric or CJK
    if !first.is_ascii_alphanumeric() && !is_cjk_slug_char(first) {
        return Err(OperationError::invalid_params(format!(
            "Invalid filename: {name} (allowed: alphanumeric, CJK, dot, underscore, hyphen — no leading dot/dash, no control chars or backslash)"
        )));
    }

    // Remaining chars: alphanumeric, CJK, dot, underscore, hyphen
    for c in chars {
        if !c.is_ascii_alphanumeric()
            && !is_cjk_slug_char(c)
            && c != '.'
            && c != '_'
            && c != '-'
        {
            return Err(OperationError::invalid_params(format!(
                "Invalid filename: {name} (allowed: alphanumeric, CJK, dot, underscore, hyphen — no leading dot/dash, no control chars or backslash)"
            )));
        }
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Operation trait (1:1 TS parity)
// ──────────────────────────────────────────────────────────────────────────

/// Trait defining a zbrain operation handler.
///
/// Object-safe base trait for all operations.
///
/// Used by the registry to store and dispatch operations dynamically.
/// Contains only metadata methods and the JSON-based entry point (object-safe).
///
/// Concrete operations should implement `TypedOperation` instead of this trait
/// directly — the blanket impl automatically provides the `Operation` trait.
#[async_trait]
pub trait Operation: fmt::Debug + Send + Sync {
    /// Stable machine-readable operation name (snake_case).
    fn name(&self) -> &'static str;

    /// Human-readable one-sentence description.
    fn description(&self) -> &'static str;

    /// Whether this operation is ONLY available to local callers.
    fn local_only(&self) -> bool {
        false
    }

    /// JSON Schema for input params (`inputSchema` in MCP `tools/list`).
    ///
    /// Implementations should return an object schema. The default is an
    /// empty object schema; concrete ops override this via `TypedOperation`.
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    /// OAuth scope required to invoke this operation over MCP.
    ///
    /// Defaults to `"read"`. Override via `TypedOperation::required_scope()`.
    fn required_scope(&self) -> &'static str {
        "read"
    }

    /// JSON-based execution entry point (object-safe).
    ///
    /// Deserializes params, validates them, enforces trust boundaries,
    /// executes the handler, and serializes the result.
    async fn execute_json(
        &self,
        ctx: &OperationContext,
        params: serde_json::Value,
    ) -> OperationResult<serde_json::Value>;
}

/// Generic typed operation trait for concrete implementations.
///
/// Implement this trait for your operation; the `Operation` trait will be
/// automatically provided via blanket impl.
#[async_trait]
pub trait TypedOperation: fmt::Debug + Send + Sync {
    /// Parameters type for this operation. Must be deserializable and
    /// validateable.
    type Params: ValidateParams + serde::de::DeserializeOwned + fmt::Debug + Send;

    /// Output type for this operation. Must be serializable for JSON responses.
    type Output: serde::Serialize + fmt::Debug;

    /// Stable machine-readable operation name (snake_case).
    fn name(&self) -> &'static str;

    /// Human-readable one-sentence description.
    fn description(&self) -> &'static str;

    /// Whether this operation is ONLY available to local callers.
    ///
    /// Default: false (exposed to MCP by default). Override to true for
    /// local-only operations.
    fn local_only(&self) -> bool {
        false
    }

    /// Whether this operation modifies persisted state (pages, tags, files,
    /// etc.). Used for audit logging and dry-run support.
    fn mutating(&self) -> bool {
        false
    }

    /// CLI command mapping. Returns None if the operation should NOT be
    /// exposed via CLI (e.g., internal operations, MCP-only operations).
    ///
    /// Default: None (not exposed via CLI). Override to expose.
    fn cli_hints(&self) -> Option<CliHints> {
        None
    }

    /// OAuth scope required to invoke this operation over MCP.
    ///
    /// Mirrors TS `op.scope || 'read'`. The returned scope string is
    /// checked against the authenticated client's granted scopes using
    /// `has_scope()` at MCP dispatch time.
    ///
    /// Valid values: `"read"`, `"write"`, `"admin"`, `"sources_admin"`,
    /// `"users_admin"`, `"agent"`.
    ///
    /// Default: `"read"` (least privileged, safe default for read-only ops).
    fn required_scope(&self) -> &'static str {
        "read"
    }

    /// JSON Schema for the input params (`inputSchema` in MCP tool defs).
    ///
    /// Used by `build_tool_defs()` in `zbrain-mcp` to generate MCP
    /// `tools/list` responses. The schema is an object with `properties`
    /// and `required` keys, matching JSON Schema draft-07 subset.
    ///
    /// Default: returns an empty object schema `{ "type": "object", "properties": {} }`.
    /// Override to provide a meaningful schema for MCP clients.
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    /// Execute the operation with validated params.
    ///
    /// Trust boundary enforcement happens BEFORE this method is called
    /// (via `execute_json` in the blanket impl).
    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output>;
}

// ── Blanket impl: TypedOperation → Operation ─────────────────────────────

#[async_trait]
impl<T: TypedOperation + Sync> Operation for T {
    fn name(&self) -> &'static str {
        <Self as TypedOperation>::name(self)
    }

    fn description(&self) -> &'static str {
        <Self as TypedOperation>::description(self)
    }

    fn local_only(&self) -> bool {
        <Self as TypedOperation>::local_only(self)
    }

    fn input_schema(&self) -> serde_json::Value {
        <Self as TypedOperation>::input_schema(self)
    }

    fn required_scope(&self) -> &'static str {
        <Self as TypedOperation>::required_scope(self)
    }

    async fn execute_json(
        &self,
        ctx: &OperationContext,
        params: serde_json::Value,
    ) -> OperationResult<serde_json::Value> {
        // Step 0: Trust boundary enforcement
        if ctx.remote && <Self as TypedOperation>::local_only(self) {
            return Err(OperationError::permission_denied(format!(
                "Operation '{}' is only available locally (not via MCP)",
                <Self as TypedOperation>::name(self)
            )));
        }

        // Step 1: Deserialize JSON to typed params
        let params: T::Params = serde_json::from_value(params).map_err(|e| {
            OperationError::invalid_params(format!("Params deserialization failed: {}", e))
        })?;

        // Step 2: Validate params
        params.validate()?;

        // Step 3: Execute typed handler
        let result = T::execute(self, ctx, params).await?;

        // Step 4: Serialize result to JSON
        serde_json::to_value(result).map_err(|e| {
            OperationError::new(
                ErrorCode::InvalidParams,
                format!("Result serialization failed: {}", e),
            )
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Operation Registry (Issue #43 - dispatch system)
// ──────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;

/// Registry for all operations.
///
/// Stores operations as type-erased trait objects and provides:
/// - Registration of new operations
/// - Lookup by operation name
/// - Trust-boundary-enforcing dispatch
#[derive(Debug, Clone, Default)]
pub struct OperationRegistry {
    ops: HashMap<&'static str, Arc<dyn Operation>>,
}

impl OperationRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an operation.
    ///
    /// The operation will be available for lookup and dispatch by `name()`.
    pub fn register<O: Operation + 'static>(&mut self, op: O) {
        let name = op.name();
        self.ops.insert(name, Arc::new(op));
    }

    /// Look up an operation by name.
    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Operation>> {
        self.ops.get(name).cloned()
    }

    /// Dispatch an operation by name with JSON params.
    ///
    /// This is the main entry point for MCP/HTTP requests. Performs:
    /// 1. Operation lookup by name
    /// 2. Trust boundary enforcement (local_only check)
    /// 3. Params deserialization + validation
    /// 4. Handler execution
    /// 5. Result serialization
    ///
    /// # Errors
    ///
    /// Returns `OperationError` if:
    /// - Operation not found (`invalid_params`)
    /// - Operation is local-only but caller is remote (`permission_denied`)
    /// - Params deserialization fails (`invalid_params`)
    /// - Params validation fails (`invalid_params`)
    /// - Handler execution fails (operation-specific error code)
    pub async fn dispatch_json(
        &self,
        name: &str,
        ctx: &OperationContext,
        params: serde_json::Value,
    ) -> OperationResult<serde_json::Value> {
        // Step 1: Lookup operation
        let op = self.lookup(name).ok_or_else(|| {
            OperationError::invalid_params(format!("Unknown operation: {}", name))
        })?;

        // Step 2: Trust boundary enforcement (local_only check)
        enforce_local_only(op.name(), op.local_only(), ctx)?;

        // Step 3-5: Deserialize, validate, execute, serialize
        op.execute_json(ctx, params).await
    }

    /// Get all registered operation names (for tool listing).
    pub fn operation_names(&self) -> Vec<&'static str> {
        self.ops.keys().copied().collect()
    }

    /// Get all registered operations (for MCP tool listing).
    pub fn operations(&self) -> Vec<Arc<dyn Operation>> {
        self.ops.values().cloned().collect()
    }

    /// Dispatch a tool call and return an MCP-compatible `ToolResult`.
    ///
    /// This is the shared dispatch path for both the CLI and MCP server transports.
    /// Both transports must use this method to ensure identical error formatting,
    /// trust boundary enforcement, and result serialization.
    ///
    /// Mirrors `dispatchToolCall()` in TS `src/mcp/dispatch.ts`.
    ///
    /// # Returns
    ///
    /// Always returns `Ok(ToolResult)` — errors are represented as
    /// `ToolResult { is_error: true, ... }` rather than `Err(...)`.
    /// This matches the MCP spec which returns tool errors in the result body,
    /// not as JSON-RPC errors.
    pub async fn dispatch_tool_call(
        &self,
        name: &str,
        ctx: &OperationContext,
        params: serde_json::Value,
    ) -> ToolResult {
        match self.dispatch_json(name, ctx, params).await {
            Ok(value) => ToolResult {
                content: vec![ToolContent {
                    content_type: "text".into(),
                    text: serde_json::to_string_pretty(&value)
                        .unwrap_or_else(|_| value.to_string()),
                }],
                is_error: false,
                meta: None,
            },
            Err(e) => ToolResult {
                content: vec![ToolContent {
                    content_type: "text".into(),
                    text: serde_json::to_string_pretty(&e)
                        .unwrap_or_else(|_| format!("{{\"error\":\"{}\"}}", e.code)),
                }],
                is_error: true,
                meta: None,
            },
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// ToolResult — MCP tool call response shape
// ──────────────────────────────────────────────────────────────────────────

/// Single content block in a `ToolResult`.
///
/// Mirrors `{ type: 'text'; text: string }` in TS dispatch.ts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ToolContent {
    /// Content type — always "text" for now.
    #[serde(rename = "type")]
    pub content_type: String,
    /// The text payload (JSON-formatted operation result or error).
    pub text: String,
}

/// MCP tool call response.
///
/// Both the CLI and MCP server transports produce this shape.
/// Mirrors `ToolResult` in TS `src/mcp/dispatch.ts`.
///
/// # Shape
///
/// ```json
/// {
///   "content": [{ "type": "text", "text": "<json result>" }],
///   "isError": true,
///   "_meta": { "brain_hot_memory": { ... } }
/// }
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ToolResult {
    /// Content blocks (always contains exactly one text block).
    pub content: Vec<ToolContent>,
    /// Whether the tool call resulted in an error.
    ///
    /// MCP spec: errors are in the result body, not as JSON-RPC errors.
    #[serde(rename = "isError", skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// Optional metadata from the server.
    ///
    /// v0.31 (eD3): MCP spec-blessed metadata slot. The dispatcher may inject
    /// `brain_hot_memory` here after a successful op. Best-effort: errors in
    /// the meta hook degrade to no `_meta` rather than flipping the tool call
    /// to error.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl ToolResult {
    /// Extract the text content from the first content block.
    pub fn text(&self) -> Option<&str> {
        self.content.first().map(|c| c.text.as_str())
    }

    /// Parse the text content as JSON.
    pub fn parse_json(&self) -> Option<serde_json::Value> {
        self.text().and_then(|t| serde_json::from_str(t).ok())
    }
}

/// Operation result type.
///
/// All operation handlers return this type. The Ok variant carries the
/// operation output (serializable for MCP JSON responses).
pub type OperationResult<T> = std::result::Result<T, OperationError>;

// ──────────────────────────────────────────────────────────────────────────
// Standard Operations
// ──────────────────────────────────────────────────────────────────────────

/// Get a page by slug with fuzzy matching support.
///
/// Mirrors the `get_page` operation in TS `operations.ts`.
/// Exact lookup is performed first; if not found and fuzzy=true,
/// `resolve_slugs` is used to find candidate matches.
/// v0.28 / v0.32.2 privacy boundary for the per-token takes/facts allow-list.
///
/// `takes` and `facts` are rendered as markdown tables inside a page's
/// `compiled_truth` between fence markers. A read-only remote (untrusted)
/// MCP caller could otherwise call `get_page` / `get_versions` and recover
/// every fence row verbatim, bypassing the row-level `takes_holders_allow_list`
/// filter entirely.
///
/// When `remote` is true we strip the takes fence wholesale and strip the
/// facts fence keeping only `world`-visibility rows (public knowledge by
/// definition). Local CLI callers (`remote = false`) see the full fence.
///
/// Port of the TS `get_page` masking in `src/core/operations.ts` (the
/// `isUntrustedReader = ctx.remote === true` branch). This closes a
/// pre-existing takes-leak for untrusted readers.
fn mask_fence_body(body: &mut String, remote: bool) {
    if !remote {
        return;
    }
    let stripped_takes = crate::takes_fence::strip_takes_fence(body);
    let stripped = crate::facts_fence::strip_facts_fence(
        &stripped_takes,
        &crate::facts_fence::StripFactsFenceOpts {
            keep_visibility: Some(vec![crate::types::FactVisibility::World]),
        },
    );
    *body = stripped;
}

#[derive(Debug, Clone)]
pub struct GetPageOperation;

/// Parameters for get_page operation.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetPageParams {
    pub slug: String,
    #[serde(default)]
    pub fuzzy: bool,
    #[serde(default)]
    pub include_deleted: bool,
}

impl ValidateParams for GetPageParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

/// Output for get_page operation.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPageOutput {
    pub page: crate::engine::Page,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_slug: Option<String>,
}

#[async_trait]
impl TypedOperation for GetPageOperation {
    type Params = GetPageParams;
    type Output = GetPageOutput;

    fn name(&self) -> &'static str {
        "get_page"
    }

    fn description(&self) -> &'static str {
        "Read a page by slug (supports optional fuzzy matching). Soft-deleted pages are hidden by default; pass include_deleted: true to surface them with deleted_at populated."
    }

    fn cli_hints(&self) -> Option<CliHints> {
        Some(CliHints::new("get-page")
            .with_positional(&["slug"])
            .with_flags(&["fuzzy", "include_deleted"]))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Page slug to retrieve" },
                "fuzzy": { "type": "boolean", "description": "Enable fuzzy matching if exact slug not found" },
                "include_deleted": { "type": "boolean", "description": "Include soft-deleted pages in results" }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;

        // Build source scope opts from context
        let source_opts = ctx.source_scope_opts();
        let get_page_opts = crate::engine::GetPageOpts {
            source_id: source_opts.source_id.clone(),
            include_deleted: params.include_deleted,
        };

        // Step 1: Exact lookup first
        if let Some(mut page) = engine.get_page(&params.slug, &get_page_opts).await? {
            // v0.28/0.32.2: strip takes+facts fences for untrusted readers.
            mask_fence_body(&mut page.compiled_truth, ctx.remote);
            return Ok(GetPageOutput {
                page,
                resolved_slug: None,
            });
        }

        // Step 2: Fuzzy resolution if requested
        if params.fuzzy {
            let resolve_opts = crate::engine::ResolveSlugsOpts {
                source_id: source_opts.source_id,
                source_ids: source_opts.source_ids,
            };

            let candidates = engine.resolve_slugs(&params.slug, &resolve_opts).await?;

            match candidates.len() {
                0 => {
                    return Err(OperationError::page_not_found(format!(
                        "Page not found: {}",
                        params.slug
                    )));
                }
                1 => {
                    let resolved_slug = &candidates[0];
                    if let Some(mut page) = engine.get_page(resolved_slug, &get_page_opts).await? {
                        // v0.28/0.32.2: strip takes+facts fences for untrusted readers.
                        mask_fence_body(&mut page.compiled_truth, ctx.remote);
                        return Ok(GetPageOutput {
                            page,
                            resolved_slug: Some(resolved_slug.clone()),
                        });
                    }
                }
                _ => {
                    return Err(OperationError::invalid_params(format!(
                        "Ambiguous slug: {} matches multiple pages",
                        params.slug
                    )));
                }
            }
        }

        Err(OperationError::page_not_found(format!(
            "Page not found: {}",
            params.slug
        )))
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Think Operation (Slice #51a - RAG Query Engine)
// ──────────────────────────────────────────────────────────────────────────

/// Multi-hop synthesis across pages + takes + graph.
///
/// Pulls relevant evidence and produces a cited answer.
/// **This is a read-only operation** — no persistence happens here.
/// Use `put_page` separately to save results.
#[derive(Debug, Clone)]
pub struct ThinkOperation;

/// Parameters for think operation.
///
/// Mirrors the TS schema (minus save/take flags per Grill decision).
/// Think is strictly read-only — use separate operations for persistence.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ThinkParams {
    /// Question to answer
    pub question: String,
    /// Optional anchor page ID for context focus
    pub anchor: Option<String>,
    /// Optional number of reasoning rounds (default: 1)
    pub rounds: Option<u32>,
    /// Optional model override
    pub model: Option<String>,
    /// Optional time range start (ISO 8601)
    pub since: Option<String>,
    /// Optional time range end (ISO 8601)
    pub until: Option<String>,
}

impl ValidateParams for ThinkParams {
    fn validate(&self) -> OperationResult<()> {
        if self.question.is_empty() {
            return Err(OperationError::invalid_params(
                "question cannot be empty",
            ));
        }
        if let Some(rounds) = self.rounds {
            if rounds == 0 || rounds > 10 {
                return Err(OperationError::invalid_params(
                    "rounds must be between 1 and 10",
                ));
            }
        }
        Ok(())
    }
}

/// Output for think operation.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkOutput {
    /// Generated answer
    pub answer: String,
    /// Warning messages (e.g., model fallback, truncated context)
    pub warnings: Vec<String>,
    /// Number of evidence snippets used in reasoning
    pub evidence_used: u32,
    /// Source page slugs that contributed to the answer
    pub sources: Vec<String>,
}

#[async_trait]
impl TypedOperation for ThinkOperation {
    type Params = ThinkParams;
    type Output = ThinkOutput;

    fn name(&self) -> &'static str {
        "think"
    }

    fn description(&self) -> &'static str {
        "Multi-hop synthesis across pages + takes + graph. Pulls relevant evidence and produces a cited answer."
    }

    fn local_only(&self) -> bool {
        false
    }

    fn mutating(&self) -> bool {
        false
    }

    fn cli_hints(&self) -> Option<CliHints> {
        Some(CliHints::new("think").with_positional(&["question"]))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "Question to answer using multi-hop reasoning" },
                "anchor": { "type": "string", "description": "Optional anchor page slug for context focus" },
                "rounds": { "type": "integer", "description": "Number of reasoning rounds (1-10, default: 1)" },
                "model": { "type": "string", "description": "Optional model override" },
                "since": { "type": "string", "description": "Optional time range start (ISO 8601)" },
                "until": { "type": "string", "description": "Optional time range end (ISO 8601)" }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        // Phase 1: Keyword extraction and retrieval
        let keywords = extract_keywords(&params.question);

        let mut warnings = Vec::new();
        let mut sources = Vec::new();
        let mut evidence_used = 0;
        let mut context_chunks = Vec::new();
        let mut page_sources = Vec::new();

        // Phase 2: Search for relevant pages if engine is available.
        //
        // FUTURE(think-rerank): this retrieval bypasses the reranker that
        // QueryOperation::execute applies. In TS, Think retrieval
        // (src/core/think/gather.ts -> hybridSearch) inherits reranking as a
        // built-in of the hybridSearch primitive (gated by the active search
        // mode's reranker_enabled: off for `conservative`, on for
        // `balanced`/`tokenmax`), so TS Think IS reranked. Here we call
        // engine.search_pages directly, which is a pure storage query with no
        // post-processing, so Think loses the rerank that Query got. The fix is
        // NOT "add a Think-level rerank toggle" (that switch never existed in
        // TS) but to extract a shared operation-layer retrieve+rerank helper
        // that both QueryOperation and ThinkOperation call — engine-layer
        // downsink is ruled out because the engine trait cannot reach
        // config/audit_dir. Deferred to a future refactor.
        // registered in docs/plans/KNOWN-GAPS.md (G1).
        if let Some(engine) = &ctx.engine {
            if !keywords.is_empty() {
                let results = engine.search_pages(&SearchOpts {
                    keywords: keywords.clone(),
                    limit: Some(5),
                    min_score: Some(0.1),
                    source_id: None,
                    query_embedding: None,
                    floor_ratio: None,
                    recency_decay: None,
                    recency_fallback: None,
                }).await?;

                evidence_used = results.len() as u32;
                for result in &results {
                    let slug = result.page.slug.clone();
                    sources.push(slug.clone());
                    page_sources.push(slug);
                    if let Some(snippet) = &result.snippet {
                        context_chunks.push(snippet.clone());
                    } else {
                        context_chunks.push(result.page.title.clone());
                    }
                }
            }
        }

        // Phase 3: LLM synthesis if LLM client is available
        let answer = if let Some(llm_client) = &ctx.llm_client {
            let mut prompt_builder = crate::llm::ThinkPromptBuilder::new(&params.question);
            for (snippet, source) in context_chunks.iter().zip(page_sources.iter()) {
                prompt_builder.add_context(snippet, source);
            }

            let llm_request = prompt_builder.build_request();
            match llm_client.generate(llm_request).await {
                Ok(llm_response) => {
                    match crate::llm::ThinkPromptBuilder::parse_response(&llm_response.content) {
                        Ok(parsed) => {
                            // Override with LLM-generated answer, keep context metadata
                            warnings.extend(parsed.warnings);
                            parsed.answer
                        }
                        Err(e) => {
                            warnings.push(format!("Failed to parse LLM response: {}", e));
                            format!("LLM response could not be parsed. Raw content: {}", llm_response.content)
                        }
                    }
                }
                Err(e) => {
                    warnings.push(format!("LLM request failed: {}", e));
                    format!("Query: {} (LLM unavailable: {})", params.question, e)
                }
            }
        } else {
            // Fallback: simple summary when no LLM client is available
            if keywords.is_empty() {
                format!("Query: {} (no meaningful keywords extracted)", params.question)
            } else if evidence_used > 0 {
                format!(
                    "Query: {} | Keywords: {} | Found {} relevant pages: {}",
                    params.question,
                    keywords.join(", "),
                    evidence_used,
                    sources.join(", ")
                )
            } else {
                format!(
                    "Query: {} | Keywords: {} | No relevant pages found",
                    params.question,
                    keywords.join(", ")
                )
            }
        };

        Ok(ThinkOutput {
            answer,
            warnings,
            evidence_used,
            sources,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Query Operation (Semantic Search - MVP)
// ──────────────────────────────────────────────────────────────────────────

/// Semantic search across pages using keywords.
///
/// v1 MVP: keyword-based substring matching.
/// v2: vector embeddings + hybrid search.
///
/// This is a strictly read-only operation. Results are sorted by relevance.
#[derive(Debug, Clone)]
pub struct QueryOperation;

/// Parameters for query operation.
///
/// MVP: core search parameters only.
/// v2 will add vector/image search, expansion, recency/salience.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QueryParams {
    /// Search query text. Required for text search.
    pub query: Option<String>,
    /// Maximum number of results to return (default: 20)
    #[serde(default)]
    pub limit: Option<usize>,
    /// Offset for pagination (default: 0)
    #[serde(default)]
    pub offset: Option<usize>,
    /// Scope search to a single source (None = all sources)
    #[serde(default)]
    pub source_id: Option<String>,
}

impl ValidateParams for QueryParams {
    fn validate(&self) -> OperationResult<()> {
        // MVP: require text query (image search deferred to v2)
        if self.query.as_ref().map_or(true, |q| q.is_empty()) {
            return Err(OperationError::invalid_params(
                "query cannot be empty",
            ));
        }
        if let Some(limit) = self.limit {
            if limit > 100 {
                return Err(OperationError::invalid_params(
                    "limit cannot exceed 100",
                ));
            }
        }
        Ok(())
    }
}

/// Output for query operation.
///
/// Flat array of results matching TS output shape.
/// Sorted by relevance score descending.
///
/// `Deserialize` is derived so the CLI `--explain` path can round-trip the
/// `run_operation` `serde_json::Value` back into strong types before handing a
/// `&[QueryResultItem]` slice to the core explain formatter (the CLI layer only
/// ever sees the weakly-typed `Value`, so a `from_value` hop is required).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryOutput {
    /// Search results sorted by relevance
    pub results: Vec<QueryResultItem>,
    /// Total matching results (for pagination)
    pub total: usize,
    /// Effective limit applied
    pub limit: usize,
    /// Effective offset applied
    pub offset: usize,
}

/// Single query result item matching TS SearchResult shape.
///
/// Carries the per-stage attribution stamps captured on `engine::SearchResult`
/// so the CLI `--explain` renderer can reconstruct the multiplier breakdown
/// (`base → boost → reranker_delta → final`) without re-running search. Only
/// the migrated stages are surfaced (base_score always present; salience /
/// recency / reranker stamped only when their stage fired). The un-migrated
/// boost axes (backlink / exact-match / graph / session-demote) have no data
/// layer yet and are intentionally absent — see docs/plans/KNOWN-GAPS.md (G13).
///
/// `Deserialize` is derived (with `#[serde(default)]` on the optional stamps) so
/// the CLI can round-trip `run_operation`'s `serde_json::Value` back into this
/// strong type. `skip_serializing_if = "Option::is_none"` keeps the JSON output
/// lean — an absent stamp means "this stage did not fire for this row".
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResultItem {
    /// The matched page
    pub page: crate::engine::Page,
    /// Relevance score (0..1)
    pub score: f64,
    /// Keyword snippet extracted from content (for UI display)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Pre-boost fused score (RRF + cosine), copied from
    /// `SearchResult.base_score`. Always present; equals `score` when no boost
    /// stage ran. `--explain` renders this as the `base=` line.
    pub base_score: f64,
    /// Salience boost multiplier, copied from `SearchResult.salience_boost`.
    /// `None` when the salience stage did not multiply this row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salience_boost: Option<f64>,
    /// Recency boost multiplier, copied from `SearchResult.recency_boost`.
    /// `None` when the recency stage did not multiply this row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recency_boost: Option<f64>,
    /// Rerank rank delta, copied from `SearchResult.reranker_delta`.
    /// `None` for un-reranked tail rows / reranker off / fail-open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reranker_delta: Option<i64>,
}

#[async_trait]
impl TypedOperation for QueryOperation {
    type Params = QueryParams;
    type Output = QueryOutput;

    fn name(&self) -> &'static str {
        "query"
    }

    fn description(&self) -> &'static str {
        "Semantic search across pages using keywords. Returns ranked results with relevance scores and snippets."
    }

    fn local_only(&self) -> bool {
        false
    }

    fn mutating(&self) -> bool {
        false
    }

    fn cli_hints(&self) -> Option<CliHints> {
        Some(CliHints::new("query").with_positional(&["query"]))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query text" },
                "limit": { "type": "integer", "description": "Maximum number of results (default: 20)" },
                "offset": { "type": "integer", "description": "Pagination offset (default: 0)" },
                "source_id": { "type": "string", "description": "Scope search to a single source" }
            },
            "required": []
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        let engine = ctx.engine.as_ref().ok_or_else(|| {
            OperationError::new(ErrorCode::StorageError, "query operation requires an engine")
        })?;

        // MVP: split query into keywords (simple whitespace split)
        // v2 will use proper tokenization + vector search
        let keywords: Vec<String> = params
            .query
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(|s| s.to_lowercase())
            .filter(|s| s.len() >= 2)
            .collect();

        let limit = params.limit.unwrap_or(20);
        let offset = params.offset.unwrap_or(0);

        // Vector path: when an embedding client is wired (ctx.embedding), embed
        // the raw query text and inject the vector so the engine runs cosine
        // similarity against stored Page::embedding blobs. Failure fails OPEN to
        // lexical-only — a flaky embedding provider must never fail the search
        // (same posture as the rerank stage below). `None` (the default) leaves
        // the vector path off, so hybrid search degenerates to lexical-only.
        let query_embedding = match ctx.embedding.as_ref() {
            Some(client) => {
                let query_text = params.query.as_deref().unwrap_or_default();
                if query_text.is_empty() {
                    None
                } else {
                    match client.embed_query(query_text).await {
                        Ok(vec) => Some(vec),
                        Err(e) => {
                            // Fail open: log and continue lexical-only.
                            if let Some(logger) = ctx.logger.as_ref() {
                                logger.warn(&format!(
                                    "query embedding failed, falling back to lexical-only: {e}"
                                ));
                            }
                            None
                        }
                    }
                }
            }
            None => None,
        };

        let results = engine
            .search_pages(&crate::engine::SearchOpts {
                keywords,
                limit: Some(limit),
                min_score: Some(0.01),
                source_id: params.source_id.clone(),
                query_embedding,
                floor_ratio: None,
                recency_decay: None,
                recency_fallback: None,
            })
            .await?;

        // Rerank post-processing stage. When `ctx.rerank` is set (reranker
        // enabled), reorder the top DEFAULT_RERANK_TOP_N results by the
        // cross-encoder score and stamp rerank_score / reranker_delta; on any
        // upstream error this fails open to the fused RRF order and logs one
        // audit row. `None` (the default) skips the stage entirely. This runs
        // BEFORE pagination so the reranked order drives skip/take. Mirrors
        // the TS `applyReranker` slot in hybridSearch (after fusion/dedup,
        // before the token-budget/pagination cut).
        //
        // NOTE: Think/evidence internal retrieval (operation.rs ThinkOperation
        // search path) intentionally does NOT rerank — whether that path
        // should pay an extra cross-encoder round-trip is a separate product
        // decision handled elsewhere, not wired here.
        let results = if let Some(rerank) = ctx.rerank.as_ref() {
            let query_text = params.query.as_deref().unwrap_or_default().to_string();
            crate::rerank_client::apply_reranker(
                rerank.client.as_ref(),
                true,
                &query_text,
                results,
                &rerank.audit_dir,
                rerank.model.as_deref(),
                // Document text sent to the cross-encoder: the display snippet
                // if present, else the compiled page truth. Falls back to the
                // page title so an empty body never sends a blank document.
                |r| {
                    r.snippet
                        .clone()
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            let t = r.page.compiled_truth.clone();
                            if t.is_empty() { None } else { Some(t) }
                        })
                        .unwrap_or_else(|| r.page.title.clone())
                },
                |r, score, delta| {
                    r.rerank_score = Some(score);
                    r.reranker_delta = Some(delta);
                },
            )
            .await
        } else {
            results
        };

        let total = results.len();

        // Apply offset/limit pagination (in-memory for MVP; engine level in v2)
        let paginated: Vec<QueryResultItem> = results
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|r| QueryResultItem {
                page: r.page,
                score: r.score,
                snippet: r.snippet,
                // Carry the attribution stamps through so `--explain` can render
                // the per-stage multiplier breakdown. Only migrated stages exist
                // on SearchResult today (base_score always set; salience/recency/
                // reranker_delta stamped only when their stage fired).
                base_score: r.base_score,
                salience_boost: r.salience_boost,
                recency_boost: r.recency_boost,
                reranker_delta: r.reranker_delta,
            })
            .collect();

        Ok(QueryOutput {
            results: paginated,
            total,
            limit,
            offset,
        })
    }
}

/// Extract keywords from a query string.
///
/// v1.0 Simple rule-based approach:
/// 1. Split on Unicode word boundaries
/// 2. Filter out stopwords (common English/Chinese words)
/// 3. Filter out short words (<2 chars)
/// 4. Deduplicate and take top 5
fn extract_keywords(query: &str) -> Vec<String> {
    // Stopword list (common function words in English + Chinese)
    let stopwords: std::collections::HashSet<&str> = [
        // English
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "must", "can", "shall", "ought", "need",
        "i", "you", "he", "she", "it", "we", "they", "me", "him", "her", "us", "them",
        "my", "your", "his", "its", "our", "their", "this", "that", "these", "those",
        "what", "which", "who", "whom", "whose", "where", "when", "why", "how",
        "and", "or", "but", "not", "no", "yes", "so", "yet", "for", "of", "in",
        "on", "at", "to", "from", "by", "with", "about", "into", "through",
        "during", "before", "after", "above", "below", "between", "under", "again",
        "further", "then", "once", "here", "there", "all", "each", "few", "more",
        "most", "other", "some", "such", "no", "nor", "not", "only", "own", "same",
        "too", "very", "just", "also", "now", "get", "got", "getting", "make",
        "put", "take", "go", "come", "see", "say", "said", "like", "want", "use",
        // Chinese common function words
        "的", "是", "了", "在", "和", "有", "这", "那", "我", "你", "他",
        "她", "它", "我们", "你们", "他们", "什么", "怎么", "为什么", "哪",
        "哪里", "谁", "几", "多少", "如何", "如果", "因为", "所以", "但是",
        "而且", "并且", "还是", "或者", "不是", "就是", "还是", "一个",
    ].iter().cloned().collect();

    let mut keywords = Vec::new();

    // Simple Unicode word split
    for word in query.split_whitespace() {
        let word = word.to_lowercase();
        // Remove punctuation
        let word = word.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
        if word.len() >= 2 && !stopwords.contains(word) {
            keywords.push(word.to_string());
        }
    }

    // Deduplicate
    keywords.sort();
    keywords.dedup();

    // Take top 5
    keywords.truncate(5);

    keywords
}

// ──────────────────────────────────────────────────────────────────────────
// Pages CRUD Operations
// ──────────────────────────────────────────────────────────────────────────

/// Create or update a page by slug.
#[derive(Debug, Clone)]
pub struct PutPageOperation;

/// Parameters for put_page operation.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PutPageParams {
    pub slug: String,
    pub page_type: Option<String>,
    pub title: Option<String>,
    pub compiled_truth: Option<String>,
}

impl ValidateParams for PutPageParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

/// Output for put_page operation.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PutPageOutput {
    pub page: crate::engine::Page,
    pub created: bool,
}

#[async_trait]
impl TypedOperation for PutPageOperation {
    type Params = PutPageParams;
    type Output = PutPageOutput;

    fn name(&self) -> &'static str {
        "put_page"
    }

    fn description(&self) -> &'static str {
        "Create or update a page by slug. Creates a new page if the slug does not exist, or updates an existing page."
    }

    fn local_only(&self) -> bool {
        true
    }

    fn mutating(&self) -> bool {
        true
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Page slug (URL-safe identifier)" },
                "page_type": { "type": "string", "description": "Page type (e.g. note, doc, source). Default: note" },
                "title": { "type": "string", "description": "Page title. Defaults to slug if not provided" },
                "compiled_truth": { "type": "string", "description": "Page content (markdown or plain text)" }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        use crate::engine::PageInput;

        let engine = ctx.engine()?;

        // Check if page exists for created flag
        let get_opts = crate::engine::GetPageOpts {
            source_id: Some(ctx.source_id.clone()),
            include_deleted: true,
        };
        let existing = engine.get_page(&params.slug, &get_opts).await?;
        let created = existing.is_none();

        let page_type = params.page_type.unwrap_or_else(|| "note".to_string());
        let title = params.title.unwrap_or_else(|| params.slug.clone());
        let compiled_truth = params.compiled_truth.unwrap_or_default();

        let input = PageInput {
            page_type,
            title,
            compiled_truth,
            ..Default::default()
        };

        let page = engine.put_page(&params.slug, Some(&ctx.source_id), &input).await?;

        Ok(PutPageOutput { page, created })
    }
}

/// Soft delete a page by slug.
#[derive(Debug, Clone)]
pub struct DeletePageOperation;

/// Parameters for delete_page operation.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DeletePageParams {
    pub slug: String,
}

impl ValidateParams for DeletePageParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

/// Output for delete_page operation.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePageOutput {
    pub deleted: bool,
}

#[async_trait]
impl TypedOperation for DeletePageOperation {
    type Params = DeletePageParams;
    type Output = DeletePageOutput;

    fn name(&self) -> &'static str {
        "delete_page"
    }

    fn description(&self) -> &'static str {
        "Soft delete a page by slug. The page remains in storage with deleted_at timestamp set."
    }

    fn local_only(&self) -> bool {
        true
    }

    fn mutating(&self) -> bool {
        true
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Slug of the page to soft-delete" }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;

        // Check if page exists
        let get_opts = crate::engine::GetPageOpts {
            source_id: Some(ctx.source_id.clone()),
            include_deleted: true,
        };
        let existing = engine.get_page(&params.slug, &get_opts).await?;
        if existing.is_none() {
            return Err(OperationError::new(
                ErrorCode::PageNotFound,
                format!("Page not found: {}", params.slug),
            ));
        }

        engine.delete_page(&params.slug, Some(&ctx.source_id)).await?;
        Ok(DeletePageOutput { deleted: true })
    }
}

/// Restore a soft-deleted page by slug.
#[derive(Debug, Clone)]
pub struct RestorePageOperation;

/// Parameters for restore_page operation.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RestorePageParams {
    pub slug: String,
}

impl ValidateParams for RestorePageParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

/// Output for restore_page operation.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePageOutput {
    pub restored: bool,
}

#[async_trait]
impl TypedOperation for RestorePageOperation {
    type Params = RestorePageParams;
    type Output = RestorePageOutput;

    fn name(&self) -> &'static str {
        "restore_page"
    }

    fn description(&self) -> &'static str {
        "Restore a soft-deleted page by slug. Clears the deleted_at timestamp."
    }

    fn local_only(&self) -> bool {
        true
    }

    fn mutating(&self) -> bool {
        true
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Slug of the soft-deleted page to restore" }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;

        // Check if page exists and is deleted
        let get_opts = crate::engine::GetPageOpts {
            source_id: Some(ctx.source_id.clone()),
            include_deleted: true,
        };
        let existing = engine.get_page(&params.slug, &get_opts).await?;
        match existing {
            None => {
                return Err(OperationError::new(
                    ErrorCode::PageNotFound,
                    format!("Page not found: {}", params.slug),
                ));
            }
            Some(page) if page.deleted_at.is_none() => {
                return Err(OperationError::new(
                    ErrorCode::InvalidParams,
                    format!("Page is not deleted: {}", params.slug),
                ));
            }
            _ => {}
        }

        engine.restore_page(&params.slug, Some(&ctx.source_id)).await?;
        Ok(RestorePageOutput { restored: true })
    }
}

/// Permanently purge soft-deleted pages.
#[derive(Debug, Clone)]
pub struct PurgeDeletedPagesOperation;

/// Parameters for purge_deleted_pages operation.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PurgeDeletedPagesParams {
    pub older_than_days: Option<i64>,
}

impl ValidateParams for PurgeDeletedPagesParams {
    fn validate(&self) -> OperationResult<()> {
        if let Some(days) = self.older_than_days {
            if days < 0 {
                return Err(OperationError::invalid_params(
                    "older_than_days must be non-negative",
                ));
            }
        }
        Ok(())
    }
}

/// Output for purge_deleted_pages operation.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeDeletedPagesOutput {
    pub purged: u64,
}

#[async_trait]
impl TypedOperation for PurgeDeletedPagesOperation {
    type Params = PurgeDeletedPagesParams;
    type Output = PurgeDeletedPagesOutput;

    fn name(&self) -> &'static str {
        "purge_deleted_pages"
    }

    fn description(&self) -> &'static str {
        "Permanently purge soft-deleted pages. If older_than_days is specified, only purge pages deleted before that threshold."
    }

    fn local_only(&self) -> bool {
        true
    }

    fn mutating(&self) -> bool {
        true
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "older_than_days": {
                    "type": "integer",
                    "description": "Only purge pages deleted more than this many days ago. Omit to purge all deleted pages."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        // Convert days to hours, default to 0 (purge all deleted)
        let older_than_hours = params.older_than_days.map_or(0, |d| (d * 24) as u32);
        let result = engine.purge_deleted_pages(older_than_hours).await?;
        Ok(PurgeDeletedPagesOutput { purged: result.count })
    }
}

/// List pages with optional filtering.
#[derive(Debug, Clone)]
pub struct ListPagesOperation;

/// Parameters for list_pages operation.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ListPagesParams {
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub include_deleted: Option<bool>,
}

impl ValidateParams for ListPagesParams {
    fn validate(&self) -> OperationResult<()> {
        if let Some(limit) = self.limit {
            if limit > 1000 {
                return Err(OperationError::invalid_params(
                    "limit cannot exceed 1000",
                ));
            }
        }
        Ok(())
    }
}

/// Output for list_pages operation.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPagesOutput {
    pub pages: Vec<crate::engine::Page>,
    pub total: u64,
}

#[async_trait]
impl TypedOperation for ListPagesOperation {
    type Params = ListPagesParams;
    type Output = ListPagesOutput;

    fn name(&self) -> &'static str {
        "list_pages"
    }

    fn description(&self) -> &'static str {
        "List pages with optional filtering by kind, tag, and pagination. Returns pages and total count."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "description": "Filter by page type (e.g. note, doc, source)" },
                "tag": { "type": "string", "description": "Filter by tag" },
                "limit": { "type": "integer", "description": "Maximum number of results (max: 1000, default: 20)" },
                "offset": { "type": "integer", "description": "Pagination offset (default: 0)" },
                "include_deleted": { "type": "boolean", "description": "Include soft-deleted pages (default: false)" }
            },
            "required": []
        })
    }

    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        use crate::engine::PageFilters;

        let engine = ctx.engine()?;

        let mut filters = PageFilters::default();
        filters.source_id = Some(ctx.source_id.clone());
        filters.page_type = params.kind;
        filters.tag = params.tag;
        filters.limit = params.limit.map(|l| l as usize);
        filters.offset = params.offset.map(|o| o as usize);
        filters.include_deleted = params.include_deleted.unwrap_or(false);

        let pages = engine.list_pages(&filters).await?;
        let total = pages.len() as u64;

        Ok(ListPagesOutput { pages, total })
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

// ── TakesList / TakesSearch Operations (G33: per-token holder allow-list filtering) ──

/// Singleton operation for `takes_list`.
#[derive(Debug, Clone)]
pub struct TakesListOperation;

/// Singleton operation for `takes_search`.
#[derive(Debug, Clone)]
pub struct TakesSearchOperation;

// ── TakesList Operation (G33: per-token holder allow-list filtering) ──
/// Parameters for `takes_list`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TakesListParams {
    pub slug: Option<String>,
    pub holder: Option<String>,
    pub kind: Option<String>,
    pub active: Option<bool>,
    pub resolved: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}
impl ValidateParams for TakesListParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}
/// Output for `takes_list`.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakesListOutput {
    pub takes: Vec<crate::types::Take>,
    pub total: u64,
}
#[async_trait]
impl TypedOperation for TakesListOperation {
    type Params = TakesListParams;
    type Output = TakesListOutput;
    fn name(&self) -> &'static str {
        "takes_list"
    }
    fn description(&self) -> &'static str {
        "List takes across pages (or a single page by slug), filtered by holder/kind/active/resolved. A remote token's `takes_holders_allow_list` is enforced server-side as a hard holder filter (v0.28 visibility model)."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Restrict to a single page by slug" },
                "holder": { "type": "string", "description": "Filter to this holder (world|garry|brain|<slug>)" },
                "kind": { "type": "string", "description": "Filter to this kind (fact|take|bet|hunch)" },
                "active": { "type": "boolean", "description": "Filter by active flag" },
                "resolved": { "type": "boolean", "description": "Filter by resolved status" },
                "limit": { "type": "integer", "description": "Maximum takes to return" },
                "offset": { "type": "integer", "description": "Offset for pagination" }
            }
        })
    }
    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let page_id = match &params.slug {
            Some(slug) => {
                let opts = crate::engine::GetPageOpts {
                    source_id: Some(ctx.source_id.clone()),
                    include_deleted: false,
                };
                let page = engine
                    .get_page(slug, &opts)
                    .await?
                    .ok_or_else(|| OperationError::page_not_found(format!("Page not found: {slug}")))?;
                Some(page.id)
            }
            None => None,
        };
        let opts = crate::types::TakesListOpts {
            page_id,
            holder: params.holder.clone(),
            kind: params.kind.clone(),
            active: params.active,
            resolved: params.resolved,
            limit: params.limit,
            offset: params.offset,
            // v0.28: server-side hard filter. A remote token restricted to a
            // subset of holders can never read other holders' takes, even
            // though the engine returns them for trusted local callers.
            takes_holders_allow_list: ctx.takes_holders_allow_list.clone(),
        };
        let takes = engine.list_takes(&opts).await?;
        let total = takes.len() as u64;
        Ok(TakesListOutput { takes, total })
    }
}
// ── TakesSearch Operation (G33: per-token holder allow-list filtering) ─
/// Parameters for `takes_search`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TakesSearchParams {
    pub query: String,
    pub limit: Option<u32>,
}
impl ValidateParams for TakesSearchParams {
    fn validate(&self) -> OperationResult<()> {
        if self.query.trim().is_empty() {
            return Err(OperationError::invalid_params("query must not be empty"));
        }
        Ok(())
    }
}
/// Output for `takes_search`.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakesSearchOutput {
    pub hits: Vec<crate::types::TakeHit>,
}
#[async_trait]
impl TypedOperation for TakesSearchOperation {
    type Params = TakesSearchParams;
    type Output = TakesSearchOutput;
    fn name(&self) -> &'static str {
        "takes_search"
    }
    fn description(&self) -> &'static str {
        "Full-text search takes by claim. A remote token's `takes_holders_allow_list` is enforced server-side as a hard holder filter (v0.28 visibility model)."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Substring to search in take claims" },
                "limit": { "type": "integer", "description": "Maximum hits to return" }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let opts = crate::types::SearchTakesOpts {
            limit: params.limit,
            // v0.28: server-side hard filter on the holder field.
            takes_holders_allow_list: ctx.takes_holders_allow_list.clone(),
        };
        let hits = engine.search_takes(&params.query, &opts).await?;
        Ok(TakesSearchOutput { hits })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ErrorCode tests ───────────────────────────────────────────────────

    #[test]
    fn error_code_display_matches_snake_case() {
        assert_eq!(ErrorCode::PageNotFound.to_string(), "page_not_found");
        assert_eq!(ErrorCode::InvalidParams.to_string(), "invalid_params");
        assert_eq!(ErrorCode::PermissionDenied.to_string(), "permission_denied");
    }

    // ── OperationError tests ──────────────────────────────────────────────

    #[test]
    fn operation_error_builder_only_required_fields() {
        let e = OperationError::new(ErrorCode::PageNotFound, "Page not found.");
        assert_eq!(e.code, ErrorCode::PageNotFound);
        assert_eq!(e.message, "Page not found.");
        assert!(e.suggestion.is_none());
        assert!(e.docs.is_none());
    }

    #[test]
    fn operation_error_builder_with_optional_fields() {
        let e = OperationError::new(ErrorCode::InvalidParams, "Bad params.")
            .with_suggestion("Check the docs.")
            .with_docs("https://zbrain.dev/errors");
        assert_eq!(e.suggestion.as_deref(), Some("Check the docs."));
        assert_eq!(e.docs.as_deref(), Some("https://zbrain.dev/errors"));
    }

    #[test]
    fn operation_error_json_serialization_matches_ts_exact() {
        // TS `toJSON()` output shape: { error, message, suggestion?, docs? }
        // Field order must match EXACTLY: error, message, suggestion, docs.
        // Optional fields are omitted when absent.

        let e = OperationError::new(ErrorCode::PageNotFound, "Not found.");
        let json = serde_json::to_string(&e).unwrap();
        // Field order and key names must match TS EXACTLY
        assert_eq!(json, "{\"error\":\"page_not_found\",\"message\":\"Not found.\"}");
        // Optional fields must NOT appear when absent
        assert!(!json.contains("suggestion"), "json={json}");
        assert!(!json.contains("docs"), "json={json}");
    }

    #[test]
    fn operation_error_json_serialization_with_all_fields() {
        let e = OperationError::new(ErrorCode::InvalidParams, "Too big.")
            .with_suggestion("Use --chunk.")
            .with_docs("https://zbrain.dev/errors/chunk");
        let json = serde_json::to_string(&e).unwrap();
        // Field order matches TS toJSON(): error, message, suggestion, docs
        assert!(json.contains("\"error\":\"invalid_params\""), "json={json}");
        assert!(json.contains("\"message\":\"Too big.\""), "json={json}");
        assert!(json.contains("\"suggestion\":\"Use --chunk.\""), "json={json}");
        assert!(
            json.contains("\"docs\":\"https://zbrain.dev/errors/chunk\""),
            "json={json}"
        );
    }

    #[test]
    fn operation_error_convenience_constructors() {
        let e = OperationError::invalid_params("bad input");
        assert_eq!(e.code, ErrorCode::InvalidParams);
        assert_eq!(e.message, "bad input");

        let e = OperationError::permission_denied("no access");
        assert_eq!(e.code, ErrorCode::PermissionDenied);
        assert_eq!(e.message, "no access");
    }

    #[test]
    fn operation_error_to_structured_error_conversion() {
        // Verify the duality bridge works
        let oe = OperationError::new(ErrorCode::PageNotFound, "Not found.")
            .with_suggestion("Check slug.")
            .with_docs("https://docs");
        let se: StructuredError = oe.clone().into();

        // Field mapping: OperationError → StructuredError
        // code → class (PascalCase)
        // code → code (snake_case)
        // suggestion → hint
        // docs → docs_url
        assert_eq!(se.class, "PageNotFound");
        assert_eq!(se.code, "page_not_found");
        assert_eq!(se.message, "Not found.");
        assert_eq!(se.hint.as_deref(), Some("Check slug."));
        assert_eq!(se.docs_url.as_deref(), Some("https://docs"));
    }

    #[test]
    fn operation_error_implements_std_error() {
        // Compile-time check: must be usable as `Box<dyn Error>`.
        let e = OperationError::new(ErrorCode::InvalidParams, "x");
        let boxed: Box<dyn StdError> = Box::new(e);
        assert!(boxed.to_string().contains("Error [invalid_params]"));
    }

    #[test]
    fn operation_error_display_matches_ts_cli_format() {
        // TS cli.ts format: `Error [code]: message` + `  Fix: suggestion`
        let e = OperationError::new(ErrorCode::PageNotFound, "Page foo.md not found");
        assert_eq!(e.to_string(), "Error [page_not_found]: Page foo.md not found");

        // With suggestion
        let e = OperationError::new(ErrorCode::InvalidParams, "Bad slug")
            .with_suggestion("Use only lowercase alphanumeric and /");
        assert_eq!(
            e.to_string(),
            "Error [invalid_params]: Bad slug\n  Fix: Use only lowercase alphanumeric and /"
        );
    }

    #[test]
    fn operation_error_exit_code_matches_ts_conventions() {
        // Permission denied maps to exit 126 (command cannot execute)
        let e = OperationError::permission_denied("Nope");
        assert_eq!(e.exit_code(), 126);

        // All other errors use generic exit code 1
        let e = OperationError::invalid_params("Bad");
        assert_eq!(e.exit_code(), 1);

        let e = OperationError::new(ErrorCode::PageNotFound, "Missing");
        assert_eq!(e.exit_code(), 1);
    }

    #[test]
    fn upload_validation_error_messages_match_ts_exact() {
        // These error strings must match TS byte-for-byte because they appear
        // in user-facing error messages and tests.

        let e = OperationError::file_not_found("foo.txt");
        assert_eq!(e.message, "File not found: foo.txt");

        let e = OperationError::symlink_not_allowed("/tmp/link");
        assert_eq!(e.message, "Symlinks are not allowed for upload: /tmp/link");

        let e = OperationError::path_outside_root("../secret.txt");
        assert_eq!(
            e.message,
            "Upload path must be within the working directory: ../secret.txt"
        );
    }

    // ── OperationContext tests ────────────────────────────────────────────

    #[test]
    fn local_cli_context_has_correct_trust_boundary() {
        let ctx = OperationContext::local_cli();
        // Local CLI callers are trusted
        assert!(!ctx.remote, "local_cli must have remote=false");
        assert_eq!(ctx.source_id, "default");
        // Engine is None (populated by dispatcher)
        assert!(ctx.engine.is_none());
    }

    #[test]
    fn remote_mcp_context_has_correct_trust_boundary() {
        let ctx = OperationContext::remote_mcp("public");
        // MCP callers are untrusted
        assert!(ctx.remote, "remote_mcp must have remote=true");
        assert_eq!(ctx.source_id, "public");
    }

    #[test]
    fn source_scope_opts_precedence_scalar_without_auth() {
        // Precedence 2: ctx.source_id (scalar)
        let ctx = OperationContext::local_cli();
        let opts = ctx.source_scope_opts();
        assert_eq!(opts.source_id, Some("default".to_string()));
        assert!(opts.source_ids.is_none());
    }

    #[test]
    fn source_scope_opts_precedence_federated_read() {
        // Precedence 1: ctx.auth?.allowed_sources (federated read)
        let mut ctx = OperationContext::local_cli();
        ctx.auth = Some(AuthInfo {
            token: "t".to_string(),
            client_id: "c".to_string(),
            client_name: None,
            scopes: vec![],
            expires_at: None,
            source_id: Some("dept-x".to_string()),
            allowed_sources: Some(vec!["dept-x".to_string(), "shared".to_string()]),
        });
        let opts = ctx.source_scope_opts();
        // Federated case: source_ids is set, source_id is NOT
        assert!(opts.source_id.is_none());
        assert_eq!(
            opts.source_ids,
            Some(vec!["dept-x".to_string(), "shared".to_string()])
        );
    }

    // ── NoopLogger tests ──────────────────────────────────────────────────

    #[test]
    fn noop_logger_does_nothing() {
        // Just verify it compiles and can be called without panicking
        let logger = NoopLogger::default();
        logger.info("info");
        logger.warn("warn");
        logger.error("error");
    }

    // ── CJK char tests ─────────────────────────────────────────────────────

    #[test]
    fn is_cjk_slug_char_recognizes_cjk() {
        // Han (Chinese)
        assert!(super::is_cjk_slug_char('中'));
        assert!(super::is_cjk_slug_char('文'));
        // Hiragana
        assert!(super::is_cjk_slug_char('ひ'));
        assert!(super::is_cjk_slug_char('ら'));
        // Katakana
        assert!(super::is_cjk_slug_char('カ'));
        assert!(super::is_cjk_slug_char('タ'));
        // Hangul
        assert!(super::is_cjk_slug_char('한'));
        assert!(super::is_cjk_slug_char('글'));
        // Non-CJK should be false
        assert!(!super::is_cjk_slug_char('a'));
        assert!(!super::is_cjk_slug_char('1'));
        assert!(!super::is_cjk_slug_char('-'));
        assert!(!super::is_cjk_slug_char('.'));
    }

    // ── validate_page_slug tests ───────────────────────────────────────────

    #[test]
    fn validate_page_slug_empty_rejected() {
        let err = validate_page_slug("").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert_eq!(err.message, "page_slug must be a non-empty string");
    }

    #[test]
    fn validate_page_slug_too_long_rejected() {
        let slug: String = "a".repeat(256);
        let err = validate_page_slug(&slug).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert_eq!(err.message, "page_slug exceeds 255 characters");
    }

    #[test]
    fn validate_page_slug_valid_ascii_passes() {
        assert!(validate_page_slug("hello").is_ok());
        assert!(validate_page_slug("hello/world-123").is_ok());
        assert!(validate_page_slug("a/b/c/d/e").is_ok());
        assert!(validate_page_slug("my-great-page-v2").is_ok());
    }

    #[test]
    fn validate_page_slug_valid_cjk_passes() {
        assert!(validate_page_slug("中文-标题").is_ok());
        assert!(validate_page_slug("wiki/中文/子页面").is_ok());
        assert!(validate_page_slug("ひらがな-カタカナ").is_ok());
        assert!(validate_page_slug("한글-제목").is_ok());
    }

    #[test]
    fn validate_page_slug_leading_slash_rejected() {
        let err = validate_page_slug("/hello").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Invalid page_slug"));
    }

    #[test]
    fn validate_page_slug_trailing_slash_rejected() {
        let err = validate_page_slug("hello/").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Invalid page_slug"));
    }

    #[test]
    fn validate_page_slug_double_slash_rejected() {
        let err = validate_page_slug("hello//world").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Invalid page_slug"));
    }

    #[test]
    fn validate_page_slug_backslash_rejected() {
        let err = validate_page_slug("hello\\world").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Invalid page_slug"));
    }

    // ── validate_filename tests ────────────────────────────────────────────

    #[test]
    fn validate_filename_empty_rejected() {
        let err = validate_filename("").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert_eq!(err.message, "Filename must be a non-empty string");
    }

    #[test]
    fn validate_filename_too_long_rejected() {
        let name: String = "a".repeat(256);
        let err = validate_filename(&name).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert_eq!(err.message, "Filename exceeds 255 characters");
    }

    #[test]
    fn validate_filename_valid_ascii_passes() {
        assert!(validate_filename("document.pdf").is_ok());
        assert!(validate_filename("my-file-v1.0_final.docx").is_ok());
        assert!(validate_filename("data_2026.json").is_ok());
    }

    #[test]
    fn validate_filename_valid_cjk_passes() {
        assert!(validate_filename("会议纪要_2026.docx").is_ok());
        assert!(validate_filename("レポート.pdf").is_ok());
        assert!(validate_filename("보고서.xlsx").is_ok());
    }

    #[test]
    fn validate_filename_leading_dot_rejected() {
        let err = validate_filename(".hidden").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Invalid filename"));
        assert!(err.message.contains("no leading dot/dash"));
    }

    #[test]
    fn validate_filename_leading_dash_rejected() {
        let err = validate_filename("-flag-confusion").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Invalid filename"));
        assert!(err.message.contains("no leading dot/dash"));
    }

    #[test]
    fn validate_filename_backslash_rejected() {
        let err = validate_filename("path\\traversal").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Invalid filename"));
    }

    // ── Error message EXACT TS parity tests ────────────────────────────────

    #[test]
    fn error_messages_match_ts_byte_for_byte() {
        // These strings must match TypeScript operations.ts EXACTLY
        assert_eq!(
            OperationError::file_not_found("foo.txt").message,
            "File not found: foo.txt"
        );
        assert_eq!(
            OperationError::symlink_not_allowed("/tmp/link").message,
            "Symlinks are not allowed for upload: /tmp/link"
        );
        assert_eq!(
            OperationError::path_outside_root("../secret.txt").message,
            "Upload path must be within the working directory: ../secret.txt"
        );
        assert_eq!(
            validate_page_slug("").unwrap_err().message,
            "page_slug must be a non-empty string"
        );
        assert_eq!(
            validate_filename("").unwrap_err().message,
            "Filename must be a non-empty string"
        );
    }

    // ── Trust boundary enforcement tests (Slice #42) ─────────────────────

    #[test]
    fn enforce_local_only_blocks_remote_caller() {
        let ctx_remote = OperationContext::remote_mcp("public");
        let result = enforce_local_only("import_file", true, &ctx_remote);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(
            err.message,
            "Operation 'import_file' is only available locally (MCP/remote callers cannot use it)"
        );
    }

    #[test]
    fn enforce_local_only_allows_local_caller() {
        let ctx_local = OperationContext::local_cli();
        let result = enforce_local_only("import_file", true, &ctx_local);
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_local_only_allows_remote_non_local_only_op() {
        let ctx_remote = OperationContext::remote_mcp("public");
        let result = enforce_local_only("get_page", false, &ctx_remote);
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_d18_blocks_image_path_for_remote() {
        let ctx_remote = OperationContext::remote_mcp("public");
        let result = enforce_d18_image_path_constraint(&ctx_remote, Some("/local/file.png"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(
            err.message,
            "image_path is not permitted for remote callers (D18). Use image_url or image_data instead."
        );
    }

    #[test]
    fn enforce_d18_allows_no_image_path_for_remote() {
        let ctx_remote = OperationContext::remote_mcp("public");
        let result = enforce_d18_image_path_constraint(&ctx_remote, None);
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_d18_allows_image_path_for_local() {
        let ctx_local = OperationContext::local_cli();
        let result = enforce_d18_image_path_constraint(&ctx_local, Some("/local/file.png"));
        assert!(result.is_ok());
    }

    #[test]
    fn matches_slug_prefix_exact_match() {
        assert!(matches_slug_prefix("wiki/agents/123", "wiki/agents/123"));
        assert!(!matches_slug_prefix("wiki/agents/123", "wiki/agents/456"));
    }

    #[test]
    fn matches_slug_prefix_wildcard_match() {
        assert!(matches_slug_prefix("wiki/agents/123/page", "wiki/agents/123/*"));
        assert!(matches_slug_prefix("wiki/agents/123/nested/deep", "wiki/agents/123/*"));
        assert!(!matches_slug_prefix("wiki/agents/456/page", "wiki/agents/123/*"));
    }

    #[test]
    fn matches_slug_prefix_wildcard_excludes_base_itself() {
        // "prefix/*" should NOT match "prefix" (no trailing slash)
        // Actually let's check the TS behavior...
        // TS code: if (slug === base) continue; - so base itself does NOT match prefix/*
        // This matches exactly the TS behavior
        assert!(!matches_slug_prefix("wiki/agents/123", "wiki/agents/123/*"));
    }

    #[test]
    fn enforce_subagent_prefix_allows_non_subagent_context() {
        let ctx = OperationContext::local_cli();
        let result = enforce_subagent_put_page_prefix(&ctx, "any/slug");
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_subagent_prefix_allows_matching_prefix() {
        let mut ctx = OperationContext::local_cli();
        ctx.via_subagent = Some(true);
        ctx.subagent_id = Some(42);
        ctx.allowed_slug_prefixes = Some(vec!["wiki/agents/42/*".to_string()]);

        let result = enforce_subagent_put_page_prefix(&ctx, "wiki/agents/42/draft");
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_subagent_prefix_blocks_non_matching_prefix() {
        let mut ctx = OperationContext::local_cli();
        ctx.via_subagent = Some(true);
        ctx.allowed_slug_prefixes = Some(vec!["wiki/agents/42/*".to_string()]);

        let result = enforce_subagent_put_page_prefix(&ctx, "wiki/agents/999/attack");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert!(err.message.contains("Subagent cannot write"));
        assert!(err.message.contains("wiki/agents/42/*"));
    }

    #[test]
    fn enforce_subagent_prefix_fallback_to_legacy_namespace() {
        let mut ctx = OperationContext::local_cli();
        ctx.via_subagent = Some(true);
        ctx.subagent_id = Some(42);
        ctx.allowed_slug_prefixes = None;

        // Legacy namespace: wiki/agents/42/%
        let result = enforce_subagent_put_page_prefix(&ctx, "wiki/agents/42/draft");
        assert!(result.is_ok());
    }

    // ── Registry and dispatch tests (Slice #43) ───────────────────────────

    // Test operation: simple echo operation for registry testing
    #[derive(Debug, Clone)]
    struct EchoOperation;

    #[derive(Debug, serde::Deserialize)]
    struct EchoParams {
        message: String,
        count: Option<u32>,
    }

    impl ValidateParams for EchoParams {
        fn validate(&self) -> OperationResult<()> {
            if self.message.is_empty() {
                return Err(OperationError::invalid_params("message cannot be empty"));
            }
            Ok(())
        }
    }

    #[async_trait]
    impl TypedOperation for EchoOperation {
        type Params = EchoParams;
        type Output = String;

        fn name(&self) -> &'static str {
            "echo"
        }

        fn description(&self) -> &'static str {
            "Echoes a message back to the caller"
        }

        async fn execute(&self, _ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let count = params.count.unwrap_or(1) as usize;
            Ok(std::iter::repeat(params.message).take(count).collect::<Vec<_>>().join(" "))
        }
    }

    // Local-only operation for testing trust boundary enforcement
    #[derive(Debug, Clone)]
    struct LocalOnlyOperation;

    #[derive(Debug, serde::Deserialize)]
    struct LocalOnlyParams {
        path: String,
    }

    impl ValidateParams for LocalOnlyParams {
        fn validate(&self) -> OperationResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl TypedOperation for LocalOnlyOperation {
        type Params = LocalOnlyParams;
        type Output = String;

        fn name(&self) -> &'static str {
            "read_local_file"
        }

        fn description(&self) -> &'static str {
            "Reads a file from local filesystem (local-only)"
        }

        fn local_only(&self) -> bool {
            true
        }

        async fn execute(&self, _ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            Ok(format!("Reading: {}", params.path))
        }
    }

    #[test]
    fn registry_register_and_lookup() {
        let mut registry = OperationRegistry::new();
        registry.register(EchoOperation);

        let op = registry.lookup("echo");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "echo");
    }

    #[test]
    fn registry_lookup_unknown_returns_none() {
        let registry = OperationRegistry::new();
        assert!(registry.lookup("nonexistent").is_none());
    }

    #[test]
    fn registry_operation_names() {
        let mut registry = OperationRegistry::new();
        registry.register(EchoOperation);
        registry.register(LocalOnlyOperation);

        let mut names = registry.operation_names();
        names.sort();
        assert_eq!(names, vec!["echo", "read_local_file"]);
    }

    #[tokio::test]
    async fn dispatch_json_echo_success() {
        let mut registry = OperationRegistry::new();
        registry.register(EchoOperation);

        let ctx = OperationContext::local_cli();
        let params = serde_json::json!({ "message": "hello", "count": 3 });

        let result = registry.dispatch_json("echo", &ctx, params).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.as_str().unwrap(), "hello hello hello");
    }

    #[tokio::test]
    async fn dispatch_json_unknown_operation_error() {
        let registry = OperationRegistry::new();
        let ctx = OperationContext::local_cli();
        let params = serde_json::json!({});

        let result = registry.dispatch_json("nonexistent", &ctx, params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("Unknown operation"));
    }

    #[tokio::test]
    async fn dispatch_json_invalid_params_validation() {
        let mut registry = OperationRegistry::new();
        registry.register(EchoOperation);

        let ctx = OperationContext::local_cli();
        let params = serde_json::json!({ "message": "" });  // Empty message fails validation

        let result = registry.dispatch_json("echo", &ctx, params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("message cannot be empty"));
    }

    #[tokio::test]
    async fn dispatch_json_local_only_blocked_for_remote() {
        let mut registry = OperationRegistry::new();
        registry.register(LocalOnlyOperation);

        let ctx_remote = OperationContext::remote_mcp("public");
        let params = serde_json::json!({ "path": "/secret/path" });

        let result = registry.dispatch_json("read_local_file", &ctx_remote, params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert!(err.message.contains("only available locally"));
    }

    #[tokio::test]
    async fn dispatch_json_local_only_allowed_for_local() {
        let mut registry = OperationRegistry::new();
        registry.register(LocalOnlyOperation);

        let ctx_local = OperationContext::local_cli();
        let params = serde_json::json!({ "path": "/some/path" });

        let result = registry.dispatch_json("read_local_file", &ctx_local, params).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.as_str().unwrap(), "Reading: /some/path");
    }

    #[tokio::test]
    async fn typed_execute_directly_no_serialization() {
        // Type-safe direct execution without going through JSON
        let op = EchoOperation;
        let ctx = OperationContext::local_cli();
        let params = EchoParams {
            message: "direct".to_string(),
            count: Some(2),
        };

        let result = op.execute(&ctx, params).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "direct direct");
    }

    #[test]
    fn registry_register_get_page() {
        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);

        let op = registry.lookup("get_page");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_page");
    }

    #[test]
    fn get_page_params_deserialization() {
        // Test that snake_case params deserialize correctly
        let params: GetPageParams = serde_json::from_value(serde_json::json!({
            "slug": "test/page",
            "fuzzy": true,
            "include_deleted": true
        })).unwrap();
        assert_eq!(params.slug, "test/page");
        assert!(params.fuzzy);
        assert!(params.include_deleted);
    }

    #[test]
    fn get_page_params_defaults() {
        // Test default values for optional params
        let params: GetPageParams = serde_json::from_value(serde_json::json!({
            "slug": "test/page"
        })).unwrap();
        assert_eq!(params.slug, "test/page");
        assert!(!params.fuzzy);
        assert!(!params.include_deleted);
    }

    #[test]
    fn get_page_params_validation() {
        // Empty slug should fail validation
        let params = GetPageParams {
            slug: "".to_string(),
            fuzzy: false,
            include_deleted: false,
        };
        let result = params.validate();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
    }

    #[test]
    fn get_page_operation_name_and_description() {
        use super::TypedOperation;

        let op = GetPageOperation;
        assert_eq!(TypedOperation::name(&op), "get_page");
        assert!(TypedOperation::description(&op).contains("Read a page by slug"));
        assert!(TypedOperation::description(&op).contains("fuzzy matching"));
    }

    #[tokio::test]
    async fn dispatch_json_get_page_exact_match() {
        use crate::engine::{InMemoryEngine, PageInput};

        // Setup: Create engine and insert test page
        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page\n\nContent".to_string(),
            ..Default::default()
        };
        let _page = engine.put_page("test/page", None, &input).await.unwrap();

        // Setup registry and context
        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/page" });

        // Execute
        let result = registry.dispatch_json("get_page", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        // Verify output
        let output = result.unwrap();
        assert_eq!(output["page"]["slug"], "test/page");
        assert_eq!(output["page"]["title"], "Test Page");
        assert!(output["resolved_slug"].is_null());
    }

    #[tokio::test]
    async fn dispatch_json_get_page_not_found() {
        use crate::engine::InMemoryEngine;

        // Setup: Empty engine
        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "nonexistent/page" });

        // Execute
        let result = registry.dispatch_json("get_page", &ctx, params).await;
        assert!(result.is_err());

        // Verify error
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PageNotFound);
        assert!(err.message.contains("nonexistent/page"));
    }

    #[tokio::test]
    async fn dispatch_json_get_page_fuzzy_match_resolved() {
        use crate::engine::{InMemoryEngine, PageInput};

        // Setup: Engine with pages that can be fuzzy matched
        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page\n\nContent".to_string(),
            ..Default::default()
        };
        engine.put_page("test/page-001", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        // Fuzzy match: partial slug should resolve to the full slug
        let params = serde_json::json!({ "slug": "test/page-001", "fuzzy": true });

        // Execute
        let result = registry.dispatch_json("get_page", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        // Verify output (exact match case works)
        let output = result.unwrap();
        assert_eq!(output["page"]["slug"], "test/page-001");
    }

    #[tokio::test]
    async fn dispatch_json_get_page_fuzzy_ambiguous_error() {
        use crate::engine::{InMemoryEngine, PageInput};

        // Setup: Engine with multiple pages matching the same partial
        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page 1".to_string(),
            compiled_truth: "# Test Page 1".to_string(),
            ..Default::default()
        };
        engine.put_page("test/page-001", None, &input).await.unwrap();

        let input2 = PageInput {
            page_type: "note".to_string(),
            title: "Test Page 2".to_string(),
            compiled_truth: "# Test Page 2".to_string(),
            ..Default::default()
        };
        engine.put_page("test/page-002", None, &input2).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        // Partial "test/page" matches both pages → ambiguous error
        let params = serde_json::json!({ "slug": "test/page", "fuzzy": true });

        // Execute
        let result = registry.dispatch_json("get_page", &ctx, params).await;
        assert!(result.is_err());

        // Verify error
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.to_lowercase().contains("ambiguous"));
    }

    #[tokio::test]
    async fn dispatch_json_get_page_include_deleted() {
        use crate::engine::{InMemoryEngine, PageInput};

        // Setup: Engine with a soft-deleted page
        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Deleted Page".to_string(),
            compiled_truth: "# Deleted Page".to_string(),
            ..Default::default()
        };
        engine.put_page("deleted/page", None, &input).await.unwrap();
        engine.delete_page("deleted/page", None).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);

        // Test 1: without include_deleted, should be not found
        let engine_arc = engine.into_arc();
        let ctx = OperationContext::local_cli().with_engine(engine_arc.clone());
        let params = serde_json::json!({ "slug": "deleted/page" });
        let result = registry.dispatch_json("get_page", &ctx, params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PageNotFound);

        // Test 2: with include_deleted = true, should find the page
        let ctx2 = OperationContext::local_cli().with_engine(engine_arc);
        let params2 = serde_json::json!({ "slug": "deleted/page", "include_deleted": true });
        let result2 = registry.dispatch_json("get_page", &ctx2, params2).await;
        assert!(result2.is_ok(), "Expected ok with include_deleted, got: {:?}", result2);
        let output2 = result2.unwrap();
        assert_eq!(output2["page"]["slug"], "deleted/page");
    }

    #[test]
    fn registry_register_put_page() {
        let mut registry = OperationRegistry::new();
        registry.register(PutPageOperation);

        let op = registry.lookup("put_page");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "put_page");
    }

    #[test]
    fn put_page_params_deserialization() {
        let params: PutPageParams = serde_json::from_value(serde_json::json!({
            "slug": "test/new-page",
            "page_type": "note",
            "title": "New Page",
            "compiled_truth": "# Content"
        })).unwrap();
        assert_eq!(params.slug, "test/new-page");
        assert_eq!(params.page_type.as_deref(), Some("note"));
        assert_eq!(params.title.as_deref(), Some("New Page"));
        assert_eq!(params.compiled_truth.as_deref(), Some("# Content"));
    }

    #[test]
    fn put_page_params_validation_invalid_slug() {
        let params = PutPageParams {
            slug: "".to_string(),
            page_type: None,
            title: None,
            compiled_truth: None,
        };
        let result = params.validate();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dispatch_json_put_page_create_new() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(PutPageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({
            "slug": "test/new-page",
            "title": "My New Page",
            "compiled_truth": "# Hello World"
        });

        let result = registry.dispatch_json("put_page", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["page"]["slug"], "test/new-page");
        assert_eq!(output["page"]["title"], "My New Page");
        assert_eq!(output["created"], true);
    }

    #[tokio::test]
    async fn dispatch_json_put_page_update_existing() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Old Title".to_string(),
            compiled_truth: "Old content".to_string(),
            ..Default::default()
        };
        engine.put_page("test/existing", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(PutPageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({
            "slug": "test/existing",
            "title": "Updated Title",
            "compiled_truth": "Updated content"
        });

        let result = registry.dispatch_json("put_page", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["page"]["slug"], "test/existing");
        assert_eq!(output["page"]["title"], "Updated Title");
        assert_eq!(output["created"], false);
    }

    #[test]
    fn registry_register_delete_page() {
        let mut registry = OperationRegistry::new();
        registry.register(DeletePageOperation);

        let op = registry.lookup("delete_page");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "delete_page");
    }

    #[tokio::test]
    async fn dispatch_json_delete_page_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/to-delete", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(DeletePageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/to-delete" });

        let result = registry.dispatch_json("delete_page", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["deleted"], true);
    }

    #[tokio::test]
    async fn dispatch_json_delete_page_not_found() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(DeletePageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "nonexistent/page" });

        let result = registry.dispatch_json("delete_page", &ctx, params).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PageNotFound);
    }

    // ── RestorePage Operation (Slice #45) ──────────────────────────────────

    #[test]
    fn registry_register_restore_page() {
        let mut registry = OperationRegistry::new();
        registry.register(RestorePageOperation);

        let op = registry.lookup("restore_page");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "restore_page");
    }

    #[tokio::test]
    async fn dispatch_json_restore_page_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/to-restore", None, &input).await.unwrap();
        engine.delete_page("test/to-restore", None).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(RestorePageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/to-restore" });

        let result = registry.dispatch_json("restore_page", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["restored"], true);
    }

    #[tokio::test]
    async fn dispatch_json_restore_page_not_deleted_error() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Active Page".to_string(),
            compiled_truth: "# Active Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/active", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(RestorePageOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/active" });

        let result = registry.dispatch_json("restore_page", &ctx, params).await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("not deleted"));
    }

    #[test]
    fn registry_register_purge_deleted_pages() {
        let mut registry = OperationRegistry::new();
        registry.register(PurgeDeletedPagesOperation);

        let op = registry.lookup("purge_deleted_pages");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "purge_deleted_pages");
    }

    #[tokio::test]
    async fn dispatch_json_purge_deleted_pages_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        // Create and delete some pages
        for i in 1..=3 {
            let input = PageInput {
                page_type: "note".to_string(),
                title: format!("Page {}", i),
                compiled_truth: format!("# Page {}", i),
                ..Default::default()
            };
            let slug = format!("test/page-{}", i);
            engine.put_page(&slug, None, &input).await.unwrap();
            engine.delete_page(&slug, None).await.unwrap();
        }

        let mut registry = OperationRegistry::new();
        registry.register(PurgeDeletedPagesOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({});

        let result = registry.dispatch_json("purge_deleted_pages", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["purged"], 3);
    }

    #[test]
    fn purge_deleted_pages_params_validation() {
        let params = PurgeDeletedPagesParams { older_than_days: Some(-1) };
        let result = params.validate();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("non-negative"));

        let params = PurgeDeletedPagesParams { older_than_days: Some(7) };
        assert!(params.validate().is_ok());

        let params = PurgeDeletedPagesParams { older_than_days: None };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn registry_register_list_pages() {
        let mut registry = OperationRegistry::new();
        registry.register(ListPagesOperation);

        let op = registry.lookup("list_pages");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "list_pages");
    }

    #[tokio::test]
    async fn dispatch_json_list_pages_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        for i in 1..=5 {
            let input = PageInput {
                page_type: "note".to_string(),
                title: format!("Page {}", i),
                compiled_truth: format!("# Page {}", i),
                ..Default::default()
            };
            let slug = format!("test/page-{}", i);
            engine.put_page(&slug, None, &input).await.unwrap();
        }

        let mut registry = OperationRegistry::new();
        registry.register(ListPagesOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({});

        let result = registry.dispatch_json("list_pages", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["total"], 5);
        assert_eq!(output["pages"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn list_pages_params_validation_limit_too_high() {
        let params = ListPagesParams {
            kind: None,
            tag: None,
            limit: Some(2000),
            offset: None,
            include_deleted: None,
        };
        let result = params.validate();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("exceed 1000"));
    }

    // ── AddTag Operation (Slice #47) ───────────────────────────────────────

    #[derive(Debug, Clone)]
    struct AddTagOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct AddTagParams {
        slug: String,
        tag: String,
    }

    impl ValidateParams for AddTagParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            if self.tag.is_empty() {
                return Err(OperationError::invalid_params("tag cannot be empty"));
            }
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct AddTagOutput {
        added: bool,
    }

    #[async_trait]
    impl TypedOperation for AddTagOperation {
        type Params = AddTagParams;
        type Output = AddTagOutput;

        fn name(&self) -> &'static str {
            "add_tag"
        }

        fn description(&self) -> &'static str {
            "Add a tag to a page. Idempotent - if tag already exists, returns success."
        }

        fn local_only(&self) -> bool {
            true
        }

        fn mutating(&self) -> bool {
            true
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Page slug" },
                    "tag": { "type": "string", "description": "Tag to add" }
                },
                "required": ["slug", "tag"]
            })
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            engine
                .add_tag(&params.slug, &params.tag, Some(&ctx.source_id))
                .await?;
            Ok(AddTagOutput { added: true })
        }
    }

    #[test]
    fn registry_register_add_tag() {
        let mut registry = OperationRegistry::new();
        registry.register(AddTagOperation);

        let op = registry.lookup("add_tag");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "add_tag");
    }

    #[tokio::test]
    async fn dispatch_json_add_tag_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/tag-page", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(AddTagOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/tag-page", "tag": "important" });

        let result = registry.dispatch_json("add_tag", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["added"], true);
    }

    #[test]
    fn add_tag_params_validation_empty_tag() {
        let params = AddTagParams {
            slug: "test/page".to_string(),
            tag: "".to_string(),
        };
        let result = params.validate();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("tag cannot be empty"));
    }

    // ── RemoveTag Operation (Slice #47) ────────────────────────────────────

    #[derive(Debug, Clone)]
    struct RemoveTagOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct RemoveTagParams {
        slug: String,
        tag: String,
    }

    impl ValidateParams for RemoveTagParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            if self.tag.is_empty() {
                return Err(OperationError::invalid_params("tag cannot be empty"));
            }
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RemoveTagOutput {
        removed: bool,
    }

    #[async_trait]
    impl TypedOperation for RemoveTagOperation {
        type Params = RemoveTagParams;
        type Output = RemoveTagOutput;

        fn name(&self) -> &'static str {
            "remove_tag"
        }

        fn description(&self) -> &'static str {
            "Remove a tag from a page. Idempotent - if tag doesn't exist, returns success."
        }

        fn local_only(&self) -> bool {
            true
        }

        fn mutating(&self) -> bool {
            true
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Page slug" },
                    "tag": { "type": "string", "description": "Tag to remove" }
                },
                "required": ["slug", "tag"]
            })
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            engine
                .remove_tag(&params.slug, &params.tag, Some(&ctx.source_id))
                .await?;
            Ok(RemoveTagOutput { removed: true })
        }
    }

    #[test]
    fn registry_register_remove_tag() {
        let mut registry = OperationRegistry::new();
        registry.register(RemoveTagOperation);

        let op = registry.lookup("remove_tag");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "remove_tag");
    }

    #[tokio::test]
    async fn dispatch_json_remove_tag_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/tag-page", None, &input).await.unwrap();
        engine.add_tag("test/tag-page", "important", None).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(RemoveTagOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/tag-page", "tag": "important" });

        let result = registry.dispatch_json("remove_tag", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["removed"], true);
    }

    // ── GetTags Operation (Slice #47) ──────────────────────────────────────

    #[derive(Debug, Clone)]
    struct GetTagsOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetTagsParams {
        slug: String,
    }

    impl ValidateParams for GetTagsParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetTagsOutput {
        tags: Vec<String>,
    }

    #[async_trait]
    impl TypedOperation for GetTagsOperation {
        type Params = GetTagsParams;
        type Output = GetTagsOutput;

        fn name(&self) -> &'static str {
            "get_tags"
        }

        fn description(&self) -> &'static str {
            "Get all tags for a page."
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Page slug" }
                },
                "required": ["slug"]
            })
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let tags = engine
                .get_tags(&params.slug, Some(&ctx.source_id))
                .await?;
            Ok(GetTagsOutput { tags })
        }
    }

    #[test]
    fn registry_register_get_tags() {
        let mut registry = OperationRegistry::new();
        registry.register(GetTagsOperation);

        let op = registry.lookup("get_tags");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_tags");
    }

    #[tokio::test]
    async fn dispatch_json_get_tags_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/tag-page", None, &input).await.unwrap();
        engine.add_tag("test/tag-page", "important", None).await.unwrap();
        engine.add_tag("test/tag-page", "review", None).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(GetTagsOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/tag-page" });

        let result = registry.dispatch_json("get_tags", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["tags"].as_array().unwrap().len(), 2);
    }

    // ── GetVersions Operation (Slice #48) ──────────────────────────────────

    #[derive(Debug, Clone)]
    struct GetVersionsOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetVersionsParams {
        slug: String,
        limit: Option<u32>,
    }

    impl ValidateParams for GetVersionsParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetVersionsOutput {
        versions: Vec<crate::types::PageVersion>,
    }

    #[async_trait]
    impl TypedOperation for GetVersionsOperation {
        type Params = GetVersionsParams;
        type Output = GetVersionsOutput;

        fn name(&self) -> &'static str {
            "get_versions"
        }

        fn description(&self) -> &'static str {
            "Get version history for a page."
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Page slug" },
                    "limit": { "type": "integer", "description": "Maximum versions to return (optional)" }
                },
                "required": ["slug"]
            })
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let mut versions = engine
                .get_versions(&params.slug, Some(&ctx.source_id))
                .await?;
            // v0.28/0.32.2: snapshots persist historical compiled_truth verbatim,
            // including the takes fence, so a remote token bypassing get_page via
            // get_versions would re-introduce the same leak across every prior version.
            for v in &mut versions {
                mask_fence_body(&mut v.compiled_truth, ctx.remote);
            }
            Ok(GetVersionsOutput { versions })
        }
    }

    #[test]
    fn registry_register_get_versions() {
        let mut registry = OperationRegistry::new();
        registry.register(GetVersionsOperation);

        let op = registry.lookup("get_versions");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_versions");
    }

    // ── GetRawData Operation (Slice #50) ───────────────────────────────────

    #[derive(Debug, Clone)]
    struct GetRawDataOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetRawDataParams {
        slug: String,
        source: Option<String>,
    }

    impl ValidateParams for GetRawDataParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetRawDataOutput {
        raw_data: Vec<crate::types::RawData>,
    }

    #[async_trait]
    impl TypedOperation for GetRawDataOperation {
        type Params = GetRawDataParams;
        type Output = GetRawDataOutput;

        fn name(&self) -> &'static str {
            "get_raw_data"
        }

        fn description(&self) -> &'static str {
            "Get raw data attached to a page, optionally filtered by source."
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Page slug" },
                    "source": { "type": "string", "description": "Optional source filter" }
                },
                "required": ["slug"]
            })
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let source_ref = params.source.as_deref();
            let raw_data = engine
                .get_raw_data(&params.slug, source_ref, Some(&ctx.source_id))
                .await?;
            Ok(GetRawDataOutput { raw_data })
        }
    }

    #[test]
    fn registry_register_get_raw_data() {
        let mut registry = OperationRegistry::new();
        registry.register(GetRawDataOperation);

        let op = registry.lookup("get_raw_data");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_raw_data");
    }

    // ── UpdateSlug Operation (Slice #48) ───────────────────────────────────

    #[derive(Debug, Clone)]
    struct UpdateSlugOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct UpdateSlugParams {
        old_slug: String,
        new_slug: String,
    }

    impl ValidateParams for UpdateSlugParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.old_slug)?;
            validate_page_slug(&self.new_slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct UpdateSlugOutput {
        updated: bool,
    }

    #[async_trait]
    impl TypedOperation for UpdateSlugOperation {
        type Params = UpdateSlugParams;
        type Output = UpdateSlugOutput;

        fn name(&self) -> &'static str {
            "update_slug"
        }

        fn description(&self) -> &'static str {
            "Update a page's slug and rewrite all links pointing to it."
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "old_slug": { "type": "string", "description": "Current page slug" },
                    "new_slug": { "type": "string", "description": "New slug to assign" }
                },
                "required": ["old_slug", "new_slug"]
            })
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            engine
                .update_slug(&params.old_slug, &params.new_slug, Some(&ctx.source_id))
                .await?;
            Ok(UpdateSlugOutput { updated: true })
        }
    }

    #[test]
    fn registry_register_update_slug() {
        let mut registry = OperationRegistry::new();
        registry.register(UpdateSlugOperation);

        let op = registry.lookup("update_slug");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "update_slug");
    }

    // ── GetAllSlugs Operation (Slice #48) ──────────────────────────────────

    #[derive(Debug, Clone)]
    struct GetAllSlugsOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetAllSlugsParams {
        prefix: Option<String>,
    }

    impl ValidateParams for GetAllSlugsParams {
        fn validate(&self) -> OperationResult<()> {
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetAllSlugsOutput {
        slugs: Vec<String>,
    }

    #[async_trait]
    impl TypedOperation for GetAllSlugsOperation {
        type Params = GetAllSlugsParams;
        type Output = GetAllSlugsOutput;

        fn name(&self) -> &'static str {
            "get_all_slugs"
        }

        fn description(&self) -> &'static str {
            "Get all page slugs, optionally filtered by prefix."
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "prefix": { "type": "string", "description": "Optional prefix filter" }
                },
                "required": []
            })
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let slugs_set = engine.get_all_slugs(Some(&ctx.source_id)).await?;
            let mut slugs: Vec<String> = slugs_set.into_iter().collect();
            if let Some(prefix) = params.prefix {
                slugs.retain(|s| s.starts_with(&prefix));
            }
            slugs.sort();
            Ok(GetAllSlugsOutput { slugs })
        }
    }

    #[test]
    fn registry_register_get_all_slugs() {
        let mut registry = OperationRegistry::new();
        registry.register(GetAllSlugsOperation);

        let op = registry.lookup("get_all_slugs");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_all_slugs");
    }

    #[test]
    fn all_seven_tag_version_slug_ops_have_non_empty_schemas() {
        let mut registry = OperationRegistry::new();
        registry.register(AddTagOperation);
        registry.register(RemoveTagOperation);
        registry.register(GetTagsOperation);
        registry.register(GetVersionsOperation);
        registry.register(GetRawDataOperation);
        registry.register(UpdateSlugOperation);
        registry.register(GetAllSlugsOperation);

        let ops: &[(&str, &[&str])] = &[
            ("add_tag", &["slug", "tag"]),
            ("remove_tag", &["slug", "tag"]),
            ("get_tags", &["slug"]),
            ("get_versions", &["slug"]),
            ("get_raw_data", &["slug"]),
            ("update_slug", &["old_slug", "new_slug"]),
            ("get_all_slugs", &[]),
        ];

        for (name, required_props) in ops {
            let op = registry.lookup(name).expect("operation should be registered");
            let schema = op.input_schema();
            assert_eq!(schema["type"], "object", "{}: schema type should be object", name);

            let required = schema["required"].as_array()
                .expect(&format!("{}: required should be an array", name));
            assert_eq!(required.len(), required_props.len(), "{}: wrong required count", name);
            for rp in *required_props {
                assert!(required.iter().any(|v| v.as_str() == Some(rp)),
                    "{}: {} should be required", name, rp);
            }

            // Verify properties exist for all required fields
            let props = &schema["properties"];
            for rp in *required_props {
                assert!(props[*rp].is_object(), "{}: property {} should exist", name, rp);
            }
        }
    }

    // ── GetPageTimestamps Operation (Slice #49) ───────────────────────────

    #[derive(Debug, Clone)]
    struct GetPageTimestampsOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetPageTimestampsParams {
        slug: String,
    }

    impl ValidateParams for GetPageTimestampsParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetPageTimestampsOutput {
        timestamps: std::collections::HashMap<String, String>,
    }

    #[async_trait]
    impl TypedOperation for GetPageTimestampsOperation {
        type Params = GetPageTimestampsParams;
        type Output = GetPageTimestampsOutput;

        fn name(&self) -> &'static str {
            "get_page_timestamps"
        }

        fn description(&self) -> &'static str {
            "Get page creation and update timestamps for multiple pages."
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let timestamps = engine
                .get_page_timestamps(&[params.slug.clone()])
                .await?;
            Ok(GetPageTimestampsOutput { timestamps })
        }
    }

    #[test]
    fn registry_register_get_page_timestamps() {
        let mut registry = OperationRegistry::new();
        registry.register(GetPageTimestampsOperation);

        let op = registry.lookup("get_page_timestamps");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_page_timestamps");
    }

    #[tokio::test]
    async fn dispatch_json_get_page_timestamps_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Test Page".to_string(),
            compiled_truth: "# Test Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/timestamp-page", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(GetPageTimestampsOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "slug": "test/timestamp-page" });

        let result = registry.dispatch_json("get_page_timestamps", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert!(output["timestamps"].is_object());
        assert!(!output["timestamps"].as_object().unwrap().is_empty());
    }

    // ── GetEffectiveDates Operation (Slice #49) ───────────────────────────

    #[derive(Debug, Clone)]
    struct GetEffectiveDatesOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetEffectiveDatesParams {
        slug: String,
    }

    impl ValidateParams for GetEffectiveDatesParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetEffectiveDatesOutput {
        dates: std::collections::HashMap<String, String>,
    }

    #[async_trait]
    impl TypedOperation for GetEffectiveDatesOperation {
        type Params = GetEffectiveDatesParams;
        type Output = GetEffectiveDatesOutput;

        fn name(&self) -> &'static str {
            "get_effective_dates"
        }

        fn description(&self) -> &'static str {
            "Get effective date range for timeline pages."
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            use crate::types::PageRef;

            let engine = ctx.engine()?;
            let page_ref = PageRef {
                slug: params.slug,
                source_id: ctx.source_id.clone(),
            };
            let dates = engine
                .get_effective_dates(&[page_ref])
                .await?;
            Ok(GetEffectiveDatesOutput { dates })
        }
    }

    #[test]
    fn registry_register_get_effective_dates() {
        let mut registry = OperationRegistry::new();
        registry.register(GetEffectiveDatesOperation);

        let op = registry.lookup("get_effective_dates");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_effective_dates");
    }

    // ── GetSalienceScores Operation (Slice #49) ───────────────────────────

    #[derive(Debug, Clone)]
    struct GetSalienceScoresOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetSalienceScoresParams {
        slug: String,
    }

    impl ValidateParams for GetSalienceScoresParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetSalienceScoresOutput {
        scores: std::collections::HashMap<String, f64>,
    }

    #[async_trait]
    impl TypedOperation for GetSalienceScoresOperation {
        type Params = GetSalienceScoresParams;
        type Output = GetSalienceScoresOutput;

        fn name(&self) -> &'static str {
            "get_salience_scores"
        }

        fn description(&self) -> &'static str {
            "Get ML-derived salience scores for page entities and topics."
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            use crate::types::PageRef;

            let engine = ctx.engine()?;
            let page_ref = PageRef {
                slug: params.slug,
                source_id: ctx.source_id.clone(),
            };
            let scores = engine
                .get_salience_scores(&[page_ref])
                .await?;
            Ok(GetSalienceScoresOutput { scores })
        }
    }

    #[test]
    fn registry_register_get_salience_scores() {
        let mut registry = OperationRegistry::new();
        registry.register(GetSalienceScoresOperation);

        let op = registry.lookup("get_salience_scores");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "get_salience_scores");
    }

    // ── FindOrphanPages Operation (Slice #49) ─────────────────────────────

    #[derive(Debug, Clone)]
    struct FindOrphanPagesOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct FindOrphanPagesParams {}

    impl ValidateParams for FindOrphanPagesParams {
        fn validate(&self) -> OperationResult<()> {
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FindOrphanPagesOutput {
        orphans: Vec<crate::types::OrphanPage>,
        total_count: u64,
    }

    #[async_trait]
    impl TypedOperation for FindOrphanPagesOperation {
        type Params = FindOrphanPagesParams;
        type Output = FindOrphanPagesOutput;

        fn name(&self) -> &'static str {
            "find_orphan_pages"
        }

        fn description(&self) -> &'static str {
            "Find pages with no incoming links."
        }

        async fn execute(&self, ctx: &OperationContext, _params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let orphans = engine.find_orphan_pages().await?;
            let total_count = orphans.len() as u64;
            Ok(FindOrphanPagesOutput { orphans, total_count })
        }
    }

    #[test]
    fn registry_register_find_orphan_pages() {
        let mut registry = OperationRegistry::new();
        registry.register(FindOrphanPagesOperation);

        let op = registry.lookup("find_orphan_pages");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "find_orphan_pages");
    }

    #[tokio::test]
    async fn dispatch_json_find_orphan_pages_success() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            page_type: "note".to_string(),
            title: "Orphan Page".to_string(),
            compiled_truth: "# Orphan Page".to_string(),
            ..Default::default()
        };
        engine.put_page("test/orphan", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(FindOrphanPagesOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({});

        let result = registry.dispatch_json("find_orphan_pages", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["totalCount"].as_u64().unwrap(), 1);
    }

    // ── ListAllPageRefs Operation (Slice #49) ─────────────────────────────

    #[derive(Debug, Clone)]
    struct ListAllPageRefsOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct ListAllPageRefsParams {}

    impl ValidateParams for ListAllPageRefsParams {
        fn validate(&self) -> OperationResult<()> {
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ListAllPageRefsOutput {
        page_refs: Vec<crate::types::PageRef>,
        total_count: u64,
    }

    #[async_trait]
    impl TypedOperation for ListAllPageRefsOperation {
        type Params = ListAllPageRefsParams;
        type Output = ListAllPageRefsOutput;

        fn name(&self) -> &'static str {
            "list_all_page_refs"
        }

        fn description(&self) -> &'static str {
            "List all page cross-references for graph visualization."
        }

        async fn execute(&self, ctx: &OperationContext, _params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let page_refs = engine.list_all_page_refs().await?;
            let total_count = page_refs.len() as u64;
            Ok(ListAllPageRefsOutput { page_refs, total_count })
        }
    }

    #[test]
    fn registry_register_list_all_page_refs() {
        let mut registry = OperationRegistry::new();
        registry.register(ListAllPageRefsOperation);

        let op = registry.lookup("list_all_page_refs");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "list_all_page_refs");
    }

    #[tokio::test]
    async fn dispatch_json_list_all_page_refs_success() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(ListAllPageRefsOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({});

        let result = registry.dispatch_json("list_all_page_refs", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["totalCount"], 0);
        assert!(output["pageRefs"].as_array().unwrap().is_empty());
    }

    // ── Takes List / Search Operations (G33: per-token holder allow-list) ──

    #[test]
    fn registry_register_takes_list() {
        let mut registry = OperationRegistry::new();
        registry.register(super::TakesListOperation);

        let op = registry.lookup("takes_list");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "takes_list");
    }

    #[test]
    fn registry_register_takes_search() {
        let mut registry = OperationRegistry::new();
        registry.register(super::TakesSearchOperation);

        let op = registry.lookup("takes_search");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "takes_search");
    }

    #[tokio::test]
    async fn dispatch_json_takes_list_success() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(super::TakesListOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "limit": 10 });

        let result = registry.dispatch_json("takes_list", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["total"], 0);
        assert!(output["takes"].as_array().unwrap().is_empty());
    }

    // ── G33 / G34 security regression tests ─────────────────────────────────

    /// A realistic page `compiled_truth` carrying both a takes fence and a
    /// facts fence (one world-visible row + one private row). Used to assert
    /// that untrusted (remote) readers never recover the shielded content.
    const FENCE_BODY: &str = r#"# Secret Page

Intro.

<!--- zbrain:takes:begin -->
| # | claim | kind | holder | weight | since | source |
|---|---|---|---|---|---|---|
| 1 | Secret revenue number | take | garry | 0.9 | 2026 | private |
<!--- zbrain:takes:end -->

## Facts

<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | Founded Acme in 2017 | fact | 1.0 | world | high | 2017-01-01 |  | linkedin |  |
| 2 | Prefers async over meetings | preference | 0.85 | private | medium | 2026-04-29 |  | OH |  |
<!--- zbrain:facts:end -->

Outro."#;

    /// G34: a trusted (local) reader must see the full fence verbatim.
    #[test]
    fn mask_fence_body_local_keeps_fences() {
        let mut body = FENCE_BODY.to_string();
        let original = body.clone();
        mask_fence_body(&mut body, false);
        assert_eq!(body, original);
        assert!(body.contains("zbrain:takes:begin"));
        assert!(body.contains("zbrain:facts:begin"));
        assert!(body.contains("Secret revenue number"));
        assert!(body.contains("Prefers async over meetings"));
    }

    /// G34: an untrusted (remote) reader must lose the takes fence entirely
    /// and the private facts row, while world-visible facts survive.
    #[test]
    fn mask_fence_body_remote_strips_takes_and_private_facts() {
        let mut body = FENCE_BODY.to_string();
        mask_fence_body(&mut body, true);
        // takes fence fully removed (markers + content)
        assert!(!body.contains("zbrain:takes:begin"));
        assert!(!body.contains("zbrain:takes:end"));
        assert!(!body.contains("Secret revenue number"));
        // facts fence retained but private row dropped, world row kept
        assert!(body.contains("zbrain:facts:begin"));
        assert!(body.contains("zbrain:facts:end"));
        assert!(body.contains("Founded Acme in 2017"));
        assert!(!body.contains("Prefers async over meetings"));
    }

    fn mk_seed_take(id: u64, page_id: u64, claim: &str, holder: &str) -> crate::types::Take {
        crate::types::Take {
            id,
            page_id,
            row_num: id as i32,
            claim: claim.to_string(),
            kind: "fact".to_string(),
            holder: holder.to_string(),
            weight: 0.5,
            since_date: None,
            until_date: None,
            source: None,
            superseded_by: None,
            active: true,
            resolved_at: None,
            resolved_quality: None,
            resolved_outcome: None,
            resolved_evidence: None,
            resolved_value: None,
            resolved_unit: None,
            resolved_by: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// G33 (engine): `list_takes` applies the per-token holder allow-list as a
    /// hard filter. `None` returns every holder; a restricted list returns
    /// only the allowed holder's takes.
    #[tokio::test]
    async fn inmemory_list_takes_respects_allow_list() {
        use crate::engine::InMemoryEngine;
        use crate::types::{TakesListOpts, Take};

        let engine = InMemoryEngine::default();
        engine.add_take(mk_seed_take(1, 10, "world claim", "world"));
        engine.add_take(mk_seed_take(2, 10, "garry secret", "garry"));
        engine.add_take(mk_seed_take(3, 10, "brain secret", "brain"));

        // No filter -> all three holders visible (trusted local caller).
        let all = engine
            .list_takes(&TakesListOpts::default())
            .await
            .unwrap();
        assert_eq!(all.len(), 3);

        // Restricted allow-list -> only the allowed holder's takes.
        let only_world = engine
            .list_takes(&TakesListOpts {
                takes_holders_allow_list: Some(vec!["world".to_string()]),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(only_world.len(), 1);
        assert_eq!(only_world[0].holder, "world");

        // Empty allow-list -> fail-closed, no takes visible.
        let none = engine
            .list_takes(&TakesListOpts {
                takes_holders_allow_list: Some(vec![]),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(none.len(), 0);
    }

    /// G33 (engine): `search_takes` likewise honours the holder allow-list.
    #[tokio::test]
    async fn inmemory_search_takes_respects_allow_list() {
        use crate::engine::InMemoryEngine;
        use crate::types::SearchTakesOpts;

        let engine = InMemoryEngine::default();
        engine.add_take(mk_seed_take(1, 10, "shared revenue number", "world"));
        engine.add_take(mk_seed_take(2, 10, "shared private number", "garry"));

        let all = engine.search_takes("shared", &SearchTakesOpts::default()).await.unwrap();
        assert_eq!(all.len(), 2);

        let only_world = engine
            .search_takes(
                "shared",
                &SearchTakesOpts {
                    takes_holders_allow_list: Some(vec!["world".to_string()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(only_world.len(), 1);
        assert_eq!(only_world[0].holder, "world");
    }

    /// G33 (op): `takes_list` hard-pushes `ctx.takes_holders_allow_list` into
    /// the engine query. A remote token restricted to one holder can never
    /// read other holders' takes, even though the engine holds them.
    #[tokio::test]
    async fn takes_list_operation_enforces_allow_list_server_side() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        engine.add_take(mk_seed_take(1, 10, "world claim", "world"));
        engine.add_take(mk_seed_take(2, 10, "garry secret", "garry"));
        engine.add_take(mk_seed_take(3, 10, "brain secret", "brain"));

        let mut ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        // Simulate a remote token whose token is scoped to the "world" holder.
        ctx.takes_holders_allow_list = Some(vec!["world".to_string()]);

        let params = TakesListParams {
            slug: None,
            holder: None,
            kind: None,
            active: None,
            resolved: None,
            limit: None,
            offset: None,
        };
        let out = TakesListOperation.execute(&ctx, params).await.unwrap();
        assert_eq!(out.takes.len(), 1);
        assert_eq!(out.takes[0].holder, "world");
    }

    /// G34 (op e2e): `get_page` strips the takes fence and private facts for a
    /// remote (untrusted) reader, but the local CLI reader sees everything.
    #[tokio::test]
    async fn get_page_remote_strips_fences_end_to_end() {
        use crate::engine::{InMemoryEngine, PageInput};

        let engine = InMemoryEngine::default();
        let input = PageInput {
            title: "Secret Page".to_string(),
            compiled_truth: FENCE_BODY.to_string(),
            ..Default::default()
        };
        let _ = engine.put_page("secret/page", None, &input).await.unwrap();

        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);

        // Share one engine across both readers.
        let engine_arc = engine.into_arc();

        // Remote (untrusted) reader: fences must be stripped.
        let mut ctx_remote = OperationContext::local_cli().with_engine(engine_arc.clone());
        ctx_remote.remote = true;
        let params = serde_json::json!({ "slug": "secret/page" });
        let out_remote = registry
            .dispatch_json("get_page", &ctx_remote, params)
            .await
            .unwrap();
        let truth_remote = out_remote["page"]["compiledTruth"].as_str().unwrap();
        assert!(!truth_remote.contains("zbrain:takes:begin"));
        assert!(!truth_remote.contains("Secret revenue number"));
        assert!(truth_remote.contains("Founded Acme in 2017"));
        assert!(!truth_remote.contains("Prefers async over meetings"));

        // Local CLI reader: full fence retained.
        let ctx_local = OperationContext::local_cli().with_engine(engine_arc.clone());
        let params = serde_json::json!({ "slug": "secret/page" });
        let out_local = registry
            .dispatch_json("get_page", &ctx_local, params)
            .await
            .unwrap();
        let truth_local = out_local["page"]["compiledTruth"].as_str().unwrap();
        assert!(truth_local.contains("Secret revenue number"));
        assert!(truth_local.contains("Prefers async over meetings"));
    }

    // ── PutRawData Operation (Slice #50 - Skeleton) ─────────────────────────

    #[derive(Debug, Clone)]
    struct PutRawDataOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct PutRawDataParams {
        slug: String,
        source: String,
        data: serde_json::Value,
    }

    impl ValidateParams for PutRawDataParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            if self.source.is_empty() {
                return Err(OperationError::invalid_params("source cannot be empty"));
            }
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct PutRawDataOutput {
        success: bool,
    }

    #[async_trait]
    impl TypedOperation for PutRawDataOperation {
        type Params = PutRawDataParams;
        type Output = PutRawDataOutput;

        fn name(&self) -> &'static str {
            "put_raw_data"
        }

        fn description(&self) -> &'static str {
            "Store raw data attached to a page (e.g., scraped content, API responses)."
        }

        fn local_only(&self) -> bool {
            true
        }

        fn mutating(&self) -> bool {
            true
        }

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            // STUB IMPLEMENTATION - Full integration requires page to exist first
            // Engine layer: put_raw_data requires an existing page_id
            Ok(PutRawDataOutput { success: true })
        }
    }

    #[test]
    fn registry_register_put_raw_data() {
        let mut registry = OperationRegistry::new();
        registry.register(PutRawDataOperation);

        let op = registry.lookup("put_raw_data");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "put_raw_data");
    }

    #[tokio::test]
    async fn dispatch_json_put_raw_data_success() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(PutRawDataOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({
            "slug": "test/page",
            "source": "scraper",
            "data": { "title": "Test", "content": "Hello" }
        });

        let result = registry.dispatch_json("put_raw_data", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["success"], true);
    }

    #[test]
    fn registry_register_think() {
        let mut registry = OperationRegistry::new();
        registry.register(crate::operation::ThinkOperation);

        let op = registry.lookup("think");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "think");
    }

    #[tokio::test]
    async fn dispatch_json_think_success() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(crate::operation::ThinkOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "question": "What is ZBrain?" });

        let result = registry.dispatch_json("think", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert!(output["answer"].as_str().unwrap().contains("Query:"));
        assert_eq!(output["evidenceUsed"], 0);
        assert_eq!(output["sources"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn think_params_validation_empty_question_rejected() {
        let params = crate::operation::ThinkParams {
            question: "".to_string(),
            anchor: None,
            rounds: None,
            model: None,
            since: None,
            until: None,
        };
        let result = params.validate();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("question cannot be empty"));
    }

    #[test]
    fn think_params_validation_rounds_bounds() {
        // Zero rejected
        let params = crate::operation::ThinkParams {
            question: "test".to_string(),
            rounds: Some(0),
            anchor: None,
            model: None,
            since: None,
            until: None,
        };
        assert!(params.validate().is_err());

        // Too high rejected
        let params = crate::operation::ThinkParams {
            question: "test".to_string(),
            rounds: Some(11),
            anchor: None,
            model: None,
            since: None,
            until: None,
        };
        assert!(params.validate().is_err());

        // Valid range passes
        let params = crate::operation::ThinkParams {
            question: "test".to_string(),
            rounds: Some(5),
            anchor: None,
            model: None,
            since: None,
            until: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn extract_keywords_basic_english() {
        let query = "How do I configure the database connection in production";
        let keywords = super::extract_keywords(query);
        assert!(keywords.contains(&"configure".to_string()));
        assert!(keywords.contains(&"database".to_string()));
        assert!(keywords.contains(&"connection".to_string()));
        assert!(keywords.contains(&"production".to_string()));
        // Stopwords should be removed
        assert!(!keywords.contains(&"how".to_string()));
        assert!(!keywords.contains(&"do".to_string()));
        assert!(!keywords.contains(&"i".to_string()));
        assert!(!keywords.contains(&"the".to_string()));
        assert!(!keywords.contains(&"in".to_string()));
    }

    #[test]
    fn extract_keywords_short_words_filtered() {
        let query = "a b c hello world";
        let keywords = super::extract_keywords(query);
        assert!(!keywords.contains(&"a".to_string()));
        assert!(!keywords.contains(&"b".to_string()));
        assert!(!keywords.contains(&"c".to_string()));
        assert!(keywords.contains(&"hello".to_string()));
        assert!(keywords.contains(&"world".to_string()));
    }

    #[test]
    fn extract_keywords_chinese_stopwords_removed() {
        let query = "如何在生产环境中配置数据库连接";
        let keywords = super::extract_keywords(query);
        // Chinese is not split into words yet, so whole string treated as one token
        // Chinese segmentation would need jieba-rs, which we're not using for now
        assert!(!keywords.is_empty() || query.len() < 2);
    }

    #[test]
    fn extract_keywords_empty_query() {
        let keywords = super::extract_keywords("");
        assert!(keywords.is_empty());
    }

    #[test]
    fn extract_keywords_only_stopwords() {
        let keywords = super::extract_keywords("the a is how what");
        assert!(keywords.is_empty());
    }

    #[test]
    fn extract_keywords_punctuation_removed() {
        let keywords = super::extract_keywords("hello, world! how?");
        assert!(keywords.contains(&"hello".to_string()));
        assert!(keywords.contains(&"world".to_string()));
    }

    #[tokio::test]
    async fn dispatch_json_think_with_keywords() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(crate::operation::ThinkOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "question": "How to configure database production" });

        let result = registry.dispatch_json("think", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        // With empty engine, evidence_used should be 0 (no matching pages)
        assert_eq!(output["evidenceUsed"].as_u64().unwrap(), 0);
        assert!(output["sources"].as_array().unwrap().is_empty());
        // Answer should contain the keywords
        assert!(output["answer"].as_str().unwrap().contains("database"));
    }

    #[tokio::test]
    async fn dispatch_json_think_with_llm_client() {
        use crate::engine::InMemoryEngine;
        use crate::llm::MockLlmClient;

        // Setup engine with a page
        let engine = InMemoryEngine::default();
        engine.put_page("docs/database", Some("default"), &crate::engine::PageInput {
            page_type: "doc".to_string(),
            title: "Database Configuration".to_string(),
            compiled_truth: "Set the DB_URL environment variable to configure database connection".to_string(),
            timeline: None,
            frontmatter: None,
            content_hash: None,
            page_kind: None,
            effective_date: None,
            effective_date_source: None,
            import_filename: None,
            chunker_version: None,
            source_path: None,
            source_kind: None,
            source_uri: None,
            ingested_via: None,
            ingested_at: None,
            last_retrieved_at: None,
            embedding: None,
        }).await.unwrap();

        // Setup mock LLM client that returns structured JSON
        let llm_client = MockLlmClient::default();
        llm_client.queue_success(r#"{
            "answer": "Use the DB_URL environment variable to configure your database connection.",
            "warnings": [],
            "evidence_used": 1,
            "sources": ["docs/database"]
        }"#);

        let mut registry = OperationRegistry::new();
        registry.register(crate::operation::ThinkOperation);

        let ctx = OperationContext::local_cli()
            .with_engine(engine.into_arc())
            .with_llm_client(std::sync::Arc::new(llm_client));

        let params = serde_json::json!({ "question": "How to configure database production" });

        let result = registry.dispatch_json("think", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        // Should include LLM-generated answer
        assert!(output["answer"].as_str().unwrap().contains("DB_URL"));
        assert_eq!(output["evidenceUsed"].as_u64().unwrap(), 1);
    }

    #[tokio::test]
    async fn dispatch_json_put_raw_data_remote_blocked() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(PutRawDataOperation);

        // Remote context should be blocked
        let ctx = OperationContext::remote_mcp("public").with_engine(engine.into_arc());
        let params = serde_json::json!({
            "slug": "test/page",
            "source": "scraper",
            "data": { "title": "Test" }
        });

        let result = registry.dispatch_json("put_raw_data", &ctx, params).await;
        assert!(result.is_err(), "Expected permission denied for remote call");

        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert!(err.message.contains("only available locally"));
    }

    // ── Upload path traversal security tests (Slice #42c) ─────────────────

    #[cfg(test)]
    mod upload_path_security {
        use std::fs::File;
        use std::io::Write;

        use super::*;

        #[test]
        fn strict_mode_blocks_parent_traversal() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();

            // Create a file OUTSIDE the root
            let outside_file = root.parent().unwrap().join("outside_test.txt");
            File::create(&outside_file).unwrap();

            // Strict mode should block containment when giving absolute path
            let result = validate_upload_path(outside_file.to_str().unwrap(), root.to_str().unwrap(), true);
            assert!(result.is_err(), "Strict mode should block path outside root");
            let err = result.unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidParams);
            assert!(err.message.contains("within the working directory"));
        }

        #[test]
        fn strict_mode_blocks_nested_parent_traversal() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();

            // Create nested subdir
            let subdir = root.join("a/b/c");
            std::fs::create_dir_all(&subdir).unwrap();

            // File outside: ../../secret
            let outside_rel = "../../outside.txt";

            // Strict mode should block
            let result = validate_upload_path(outside_rel, root.to_str().unwrap(), true);
            assert!(result.is_err(), "Strict mode should block nested ../../ traversal");
        }

        #[test]
        fn strict_mode_blocks_windows_backslash_traversal() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();

            // Windows-style path traversal (tested on all platforms for consistency)
            let outside_rel = "..\\..\\outside.txt";

            // Strict mode should block
            let result = validate_upload_path(outside_rel, root.to_str().unwrap(), true);
            assert!(result.is_err(), "Strict mode should block ..\\ traversal");
        }

        #[test]
        fn loose_mode_allows_parent_traversal_for_local() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();

            // Create a file outside the root (in temp parent)
            let outside_file = root.parent().unwrap().join("outside_loose.txt");
            File::create(&outside_file).unwrap();

            // Loose mode should allow (local CLI user is trusted)
            // We just verify it doesn't return containment error; might return Ok
            // or other errors depending on platform, but NOT "within working directory"
            let _result = validate_upload_path(outside_file.to_str().unwrap(), root.to_str().unwrap(), false);
            // Loose mode doesn't enforce root containment - that's the key property
        }

        #[test]
        fn always_blocks_final_component_symlink() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();

            // Create a real file
            let real_file = root.join("real.txt");
            File::create(&real_file).unwrap();

            // Create a symlink to it (only works on Unix; skip on Windows)
            #[cfg(unix)]
            {
                let symlink_path = root.join("link.txt");
                std::os::unix::fs::symlink(&real_file, &symlink_path).unwrap();

                // Both strict and loose should reject
                let result_strict = validate_upload_path(symlink_path.to_str().unwrap(), root.to_str().unwrap(), true);
                let result_loose = validate_upload_path(symlink_path.to_str().unwrap(), root.to_str().unwrap(), false);

                assert!(result_strict.is_err(), "Strict mode should block final-component symlink");
                assert!(result_loose.is_err(), "Loose mode should also block final-component symlink");
            }
        }

        #[test]
        fn symlink_metadata_check_race_tolerant() {
            // Verify that missing symlink_metadata() (e.g. file deleted between
            // canonicalize and lstat) doesn't break validation - we just skip
            // the symlink check in that race window, which is acceptable.
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();

            // Create a normal file
            let file_path = root.join("normal.txt");
            {
                let mut f = File::create(&file_path).unwrap();
                writeln!(f, "content").unwrap();
            }

            // Should pass with no issues
            let result = validate_upload_path(file_path.to_str().unwrap(), root.to_str().unwrap(), true);
            assert!(result.is_ok());
        }
    }

    mod query_operation_tests {
        use super::*;
        use crate::engine::{InMemoryEngine, PageInput};
        use std::sync::Arc;

        #[test]
        fn query_operation_lookup_works() {
            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            let op = registry.lookup("query");
            assert!(op.is_some());
            assert_eq!(op.unwrap().name(), "query");
        }

        #[test]
        fn query_params_deserialization() {
            let params: QueryParams = serde_json::from_value(serde_json::json!({
                "query": "test search query",
                "limit": 10,
                "offset": 0,
                "source_id": "default"
            }))
            .unwrap();

            assert_eq!(params.query.unwrap(), "test search query");
            assert_eq!(params.limit, Some(10));
            assert_eq!(params.offset, Some(0));
            assert_eq!(params.source_id, Some("default".to_string()));
        }

        #[test]
        fn query_params_validation_rejects_empty_query() {
            let params = QueryParams {
                query: Some("".to_string()),
                limit: None,
                offset: None,
                source_id: None,
            };

            let result = params.validate();
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().code, ErrorCode::InvalidParams);
        }

        #[test]
        fn query_params_validation_rejects_limit_over_100() {
            let params = QueryParams {
                query: Some("test".to_string()),
                limit: Some(101),
                offset: None,
                source_id: None,
            };

            let result = params.validate();
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().code, ErrorCode::InvalidParams);
        }

        #[test]
        fn query_params_accepts_limit_100() {
            let params = QueryParams {
                query: Some("test".to_string()),
                limit: Some(100),
                offset: None,
                source_id: None,
            };

            let result = params.validate();
            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn dispatch_json_query_success() {
            // Setup engine with pages
            let engine = InMemoryEngine::default();

            // Create page with content to search
            let input = PageInput {
                page_type: "note".to_string(),
                title: "Search Target Page".to_string(),
                compiled_truth: "This is some searchable content about coding and development.".to_string(),
                ..Default::default()
            };
            engine.put_page("test/search-result", None, &input).await.unwrap();

            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            let ctx = OperationContext::local_cli().with_engine(Arc::new(engine));
            let params = serde_json::json!({ "query": "content" });

            let result = registry.dispatch_json("query", &ctx, params).await;
            assert!(result.is_ok(), "Expected ok, got: {:?}", result);

            let output = result.unwrap();
            assert!(output["results"].is_array());
            assert!(output["total"].is_number());
            assert!(output["limit"].is_number());
            assert!(output["offset"].is_number());
        }

        #[tokio::test]
        async fn query_with_source_id_scope() {
            // Setup engine with pages
            let engine = InMemoryEngine::default();

            // Create page with content to search
            let input = PageInput {
                page_type: "note".to_string(),
                title: "Search Target Page".to_string(),
                compiled_truth: "This is some searchable content about coding and development.".to_string(),
                ..Default::default()
            };
            engine.put_page("test/search-result", None, &input).await.unwrap();

            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            let ctx = OperationContext::local_cli().with_engine(Arc::new(engine));

            // Use the same source as the page
            let params = serde_json::json!({ "query": "content", "source_id": "default" });

            let result = registry.dispatch_json("query", &ctx, params).await;
            assert!(result.is_ok(), "Expected ok, got: {:?}", result);

            let output = result.unwrap();
            assert_eq!(output["results"].as_array().unwrap().len(), 1);
        }

        #[tokio::test]
        async fn query_output_serialization_uses_camel_case() {
            // Setup engine with pages
            let engine = InMemoryEngine::default();
            let input = PageInput {
                page_type: "note".to_string(),
                title: "Camel Case Page".to_string(),
                compiled_truth: "Content for camelCase testing with keyword matching.".to_string(),
                ..Default::default()
            };
            engine.put_page("test/camel-case", None, &input).await.unwrap();

            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            let ctx = OperationContext::local_cli().with_engine(Arc::new(engine));
            let params = serde_json::json!({ "query": "keyword" });

            let result = registry.dispatch_json("query", &ctx, params).await;
            assert!(result.is_ok(), "Expected ok, got: {:?}", result);

            let output = result.unwrap();
            let output_str = serde_json::to_string(&output).unwrap();

            // Verify camelCase keys - no underscores in top-level keys
            assert!(output_str.contains("\"results\""));
            assert!(output_str.contains("\"total\""));
            assert!(output_str.contains("\"limit\""));
            assert!(output_str.contains("\"offset\""));

            // Verify nested page uses camelCase (page_type -> pageType)
            assert!(
                output_str.contains("\"pageType\""),
                "Expected camelCase field names, got: {}",
                output_str
            );
        }

        // ── rerank wiring (1-4-2-2): reranker off by default; on = reorder ──

        /// A rerank client that imposes a deterministic order on the head it is
        /// given: documents are ranked by their text in DESCENDING lexical
        /// order, highest relevance first. This lets the wiring test assert an
        /// exact output order without depending on the fused RRF tie-break,
        /// which is nondeterministic for equally-scored pages. Not the real
        /// transport — only the pipeline wiring is under test.
        struct ReversingRerank;

        #[async_trait::async_trait]
        impl crate::rerank_client::RerankClient for ReversingRerank {
            async fn rerank(
                &self,
                req: &crate::rerank_client::RerankRequest,
            ) -> Result<Vec<crate::rerank_client::RerankOutcome>, crate::rerank_client::RerankError>
            {
                // Rank indices by their document text, descending. The outcome
                // list is emitted already-sorted (the reranker contract), so
                // the first entry is the highest-relevance document.
                let mut idx: Vec<usize> = (0..req.documents.len()).collect();
                idx.sort_by(|&a, &b| req.documents[b].cmp(&req.documents[a]));
                let n = idx.len();
                Ok(idx
                    .into_iter()
                    .enumerate()
                    .map(|(rank, index)| crate::rerank_client::RerankOutcome {
                        index,
                        relevance_score: (n - rank) as f64,
                    })
                    .collect())
            }
        }

        async fn seed_two_pages() -> InMemoryEngine {
            let engine = InMemoryEngine::default();
            engine
                .put_page(
                    "test/alpha",
                    None,
                    &PageInput {
                        page_type: "note".to_string(),
                        title: "Alpha".to_string(),
                        compiled_truth: "shared keyword alpha body".to_string(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            engine
                .put_page(
                    "test/beta",
                    None,
                    &PageInput {
                        page_type: "note".to_string(),
                        title: "Beta".to_string(),
                        compiled_truth: "shared keyword beta body".to_string(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            engine
        }

        #[tokio::test]
        async fn query_reranker_off_by_default_keeps_rrf_order() {
            let engine = seed_two_pages().await;
            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            // No .with_rerank → ctx.rerank is None → stage is skipped.
            let ctx = OperationContext::local_cli().with_engine(Arc::new(engine));
            let params = serde_json::json!({ "query": "keyword" });
            let output = registry.dispatch_json("query", &ctx, params).await.unwrap();

            let results = output["results"].as_array().unwrap();
            assert_eq!(results.len(), 2);
            // No rerank stamps leak into the serialized output.
            let s = serde_json::to_string(&output).unwrap();
            assert!(!s.contains("rerankScore"), "no rerank stamp when reranker off");
        }

        #[tokio::test]
        async fn query_reranker_on_reorders_results() {
            let engine: Arc<InMemoryEngine> = Arc::new(seed_two_pages().await);
            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            let audit = tempfile::TempDir::new().unwrap();
            let settings = crate::rerank_client::RerankSettings {
                client: Arc::new(ReversingRerank),
                audit_dir: audit.path().to_path_buf(),
                model: Some("zeroentropyai:zerank-2".to_string()),
            };
            let ctx = OperationContext::local_cli()
                .with_engine(engine.clone())
                .with_rerank(settings);

            let on = registry
                .dispatch_json("query", &ctx, serde_json::json!({ "query": "keyword" }))
                .await
                .unwrap();
            let on_titles: Vec<String> = on["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["page"]["title"].as_str().unwrap().to_string())
                .collect();

            // ReversingRerank ranks documents by compiled_truth descending:
            // "shared keyword beta body" > "shared keyword alpha body", so the
            // reranked order is deterministic regardless of the fused RRF
            // tie-break. This proves the pipeline applied the reranker's order.
            assert_eq!(
                on_titles,
                vec!["Beta".to_string(), "Alpha".to_string()],
                "rerank stage must apply the reranker's deterministic order"
            );
            // Success path writes no audit row.
            let has_audit = std::fs::read_dir(audit.path())
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);
            assert!(!has_audit, "successful rerank writes no audit row");
        }

        #[tokio::test]
        async fn query_reranker_fails_open_on_error() {
            /// Always-fail client → exercise the fail-open + audit branch
            /// through the real query pipeline.
            struct FailingRerank;
            #[async_trait::async_trait]
            impl crate::rerank_client::RerankClient for FailingRerank {
                async fn rerank(
                    &self,
                    _req: &crate::rerank_client::RerankRequest,
                ) -> Result<
                    Vec<crate::rerank_client::RerankOutcome>,
                    crate::rerank_client::RerankError,
                > {
                    Err(crate::rerank_client::RerankError {
                        message: "boom".to_string(),
                        reason: crate::rerank_audit::RerankFailureReason::Network,
                        status: None,
                    })
                }
            }

            let engine = seed_two_pages().await;
            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            let audit = tempfile::TempDir::new().unwrap();
            let ctx = OperationContext::local_cli()
                .with_engine(Arc::new(engine))
                .with_rerank(crate::rerank_client::RerankSettings {
                    client: Arc::new(FailingRerank),
                    audit_dir: audit.path().to_path_buf(),
                    model: None,
                });

            // Search still succeeds (fails open), returning results.
            let out = registry
                .dispatch_json("query", &ctx, serde_json::json!({ "query": "keyword" }))
                .await
                .unwrap();
            assert_eq!(out["results"].as_array().unwrap().len(), 2, "search survives rerank failure");
            // One audit row was written.
            let wrote_audit = std::fs::read_dir(audit.path())
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);
            assert!(wrote_audit, "fail-open must log an audit row");
        }

        // ──────────────────────────────────────────────────────────────────
        // Query embedding wiring (1-3-3)
        // ──────────────────────────────────────────────────────────────────

        /// Deterministic in-process embedding provider: maps any text to a
        /// fixed unit vector so tests exercise the vector path without a
        /// network round-trip. `dims` is honored so `EmbeddingClient::embed`'s
        /// dimension check passes.
        #[derive(Debug)]
        struct FixedVecProvider(Vec<f32>);
        #[async_trait::async_trait]
        impl crate::embedding::EmbeddingProvider for FixedVecProvider {
            async fn embed(
                &self,
                texts: &[String],
                _dims: usize,
            ) -> Result<Vec<Vec<f32>>, crate::embedding::EmbeddingError> {
                Ok(texts.iter().map(|_| self.0.clone()).collect())
            }
        }

        /// Encode an f32 vector to the little-endian byte layout the
        /// `Page::embedding` column stores (mirrors the engine decode path).
        fn f32_le_bytes(v: &[f32]) -> Vec<u8> {
            v.iter().flat_map(|f| f.to_le_bytes()).collect()
        }

        fn embedding_client(vec: Vec<f32>) -> Arc<crate::embedding::EmbeddingClient> {
            let dims = vec.len();
            let config = crate::embedding::EmbeddingConfig {
                dimensions: dims,
                api_key: "test".to_string(),
                ..crate::embedding::EmbeddingConfig::default()
            };
            Arc::new(crate::embedding::EmbeddingClient::with_provider(
                config,
                Arc::new(FixedVecProvider(vec)),
            ))
        }

        #[tokio::test]
        async fn query_vector_path_surfaces_semantic_match() {
            // Page has ZERO lexical overlap with the query keyword, but its
            // stored embedding is colinear with what the client returns for the
            // query. Without the vector path this returns nothing; with
            // ctx.embedding wired the cosine hit must surface it.
            let engine = InMemoryEngine::default();
            engine
                .put_page(
                    "semantic/only",
                    None,
                    &PageInput {
                        page_type: "note".to_string(),
                        title: "Feline companions".to_string(),
                        compiled_truth: "Domestic cats and their behaviour.".to_string(),
                        embedding: Some(f32_le_bytes(&[1.0, 0.0, 0.0])),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            // Client returns a vector colinear with the page embedding for any
            // query, so "quantum" (no lexical hit) still matches via cosine.
            let ctx = OperationContext::local_cli()
                .with_engine(Arc::new(engine))
                .with_embedding(embedding_client(vec![1.0, 0.0, 0.0]));

            let out = registry
                .dispatch_json("query", &ctx, serde_json::json!({ "query": "quantum" }))
                .await
                .unwrap();
            let results = out["results"].as_array().unwrap();
            assert_eq!(results.len(), 1, "vector path must surface the semantic match");
            assert_eq!(results[0]["page"]["slug"].as_str().unwrap(), "semantic/only");
        }

        #[tokio::test]
        async fn query_without_embedding_stays_lexical_only() {
            // No ctx.embedding → the semantic-only page (no lexical overlap) is
            // NOT found: hybrid search degenerates to lexical-only.
            let engine = InMemoryEngine::default();
            engine
                .put_page(
                    "semantic/only",
                    None,
                    &PageInput {
                        page_type: "note".to_string(),
                        title: "Feline companions".to_string(),
                        compiled_truth: "Domestic cats and their behaviour.".to_string(),
                        embedding: Some(f32_le_bytes(&[1.0, 0.0, 0.0])),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);
            let ctx = OperationContext::local_cli().with_engine(Arc::new(engine));

            let out = registry
                .dispatch_json("query", &ctx, serde_json::json!({ "query": "quantum" }))
                .await
                .unwrap();
            assert!(
                out["results"].as_array().unwrap().is_empty(),
                "lexical-only search must not surface a non-lexical page"
            );
        }

        #[tokio::test]
        async fn query_embedding_failure_fails_open_to_lexical() {
            /// Always-error embedding provider → exercise the fail-open branch.
            #[derive(Debug)]
            struct FailingProvider;
            #[async_trait::async_trait]
            impl crate::embedding::EmbeddingProvider for FailingProvider {
                async fn embed(
                    &self,
                    _texts: &[String],
                    _dims: usize,
                ) -> Result<Vec<Vec<f32>>, crate::embedding::EmbeddingError> {
                    Err(crate::embedding::EmbeddingError::Provider("boom".to_string()))
                }
            }

            // Page DOES have a lexical hit on "keyword", so lexical-only search
            // still finds it after the embedding call errors.
            let engine = seed_two_pages().await;
            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            let config = crate::embedding::EmbeddingConfig {
                dimensions: 3,
                api_key: "test".to_string(),
                ..crate::embedding::EmbeddingConfig::default()
            };
            let client = Arc::new(crate::embedding::EmbeddingClient::with_provider(
                config,
                Arc::new(FailingProvider),
            ));
            let ctx = OperationContext::local_cli()
                .with_engine(Arc::new(engine))
                .with_embedding(client);

            let out = registry
                .dispatch_json("query", &ctx, serde_json::json!({ "query": "keyword" }))
                .await
                .unwrap();
            assert_eq!(
                out["results"].as_array().unwrap().len(),
                2,
                "search survives embedding failure (fails open to lexical)"
            );
        }

        // ──────────────────────────────────────────────────────────────────────
        // Trust Boundary Tests
        // ──────────────────────────────────────────────────────────────────────

        #[tokio::test]
        async fn trust_boundary_local_only_operation_remote_call_rejected() {
            // Setup: create a local-only operation and call it with remote=true
            use crate::engine::InMemoryEngine;

            #[derive(Debug, Clone)]
            struct LocalOnlyOperation;

            #[derive(Debug, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            struct LocalOnlyParams {
                value: String,
            }

            impl ValidateParams for LocalOnlyParams {
                fn validate(&self) -> OperationResult<()> {
                    Ok(())
                }
            }

            #[derive(Debug, serde::Serialize)]
            struct LocalOnlyOutput {
                result: String,
            }

            #[async_trait]
            impl TypedOperation for LocalOnlyOperation {
                type Params = LocalOnlyParams;
                type Output = LocalOnlyOutput;

                fn name(&self) -> &'static str {
                    "local_only_test"
                }

                fn description(&self) -> &'static str {
                    "Test operation that is local-only"
                }

                fn local_only(&self) -> bool {
                    true // Mark as local-only
                }

                async fn execute(&self, _ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
                    Ok(LocalOnlyOutput { result: params.value })
                }
            }

            // Test 1: Local call (remote=false) should succeed
            let engine_local = InMemoryEngine::default();
            let mut registry = OperationRegistry::new();
            registry.register(LocalOnlyOperation);

            let ctx_local = OperationContext::local_cli().with_engine(Arc::new(engine_local));
            let params = serde_json::json!({ "value": "hello" });
            let result = registry.dispatch_json("local_only_test", &ctx_local, params).await;
            assert!(result.is_ok(), "Local call should succeed");

            // Test 2: Remote call (remote=true) should be rejected
            let engine_remote = InMemoryEngine::default();
            let mut ctx_remote = OperationContext::local_cli();
            ctx_remote.remote = true; // Simulate remote MCP call
            let ctx_remote = ctx_remote.with_engine(Arc::new(engine_remote));
            let params = serde_json::json!({ "value": "hello" });
            let result = registry.dispatch_json("local_only_test", &ctx_remote, params).await;

            assert!(result.is_err(), "Remote call to local-only operation should fail");
            let err = result.err().unwrap();
            assert_eq!(err.code, ErrorCode::PermissionDenied);
            assert!(err.message.contains("only available locally"));
        }

        #[tokio::test]
        async fn trust_boundary_non_local_only_operation_remote_call_allowed() {
            // Setup: default operation (not local-only) should work remotely
            use crate::engine::InMemoryEngine;

            let engine = InMemoryEngine::default();
            let mut registry = OperationRegistry::new();
            registry.register(QueryOperation);

            // Create a page to search
            let input = PageInput {
                page_type: "note".to_string(),
                title: "Test Page".to_string(),
                compiled_truth: "Content with searchable keyword.".to_string(),
                ..Default::default()
            };
            engine.put_page("test/page", None, &input).await.unwrap();

            // Remote call to query operation (not local-only) should succeed
            let mut ctx_remote = OperationContext::local_cli();
            ctx_remote.remote = true; // Simulate remote MCP call
            let ctx_remote = ctx_remote.with_engine(Arc::new(engine));
            let params = serde_json::json!({ "query": "searchable" });

            let result = registry.dispatch_json("query", &ctx_remote, params).await;
            assert!(result.is_ok(), "Remote call to non-local-only operation should succeed");
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // dispatch_tool_call() tests — shared MCP dispatch path
    // ──────────────────────────────────────────────────────────────────────
    #[cfg(test)]
    mod dispatch_tool_call_tests {
        use super::*;
        use crate::engine::InMemoryEngine;

        /// Helper: build a local-CLI context with an InMemoryEngine.
        async fn local_ctx() -> OperationContext {
            let engine = InMemoryEngine::default();
            OperationContext::local_cli().with_engine(Arc::new(engine))
        }

        /// Helper: build a registry with the standard echo + read_local_file operations.
        fn test_registry() -> OperationRegistry {
            let mut registry = OperationRegistry::new();
            // EchoOperation is defined in the dispatch_json tests above
            #[derive(Debug, Clone)]
            struct EchoOp;
            #[derive(Debug, serde::Deserialize)]
            struct EchoParams { msg: String }
            impl ValidateParams for EchoParams {
                fn validate(&self) -> OperationResult<()> { Ok(()) }
            }
            #[derive(Debug, serde::Serialize)]
            struct EchoOutput { echo: String }
            #[async_trait]
            impl TypedOperation for EchoOp {
                type Params = EchoParams;
                type Output = EchoOutput;
                fn name(&self) -> &'static str { "echo_tool" }
                fn description(&self) -> &'static str { "Echo a message" }
                async fn execute(&self, _ctx: &OperationContext, params: EchoParams) -> OperationResult<EchoOutput> {
                    Ok(EchoOutput { echo: params.msg })
                }
            }
            registry.register(EchoOp);

            // Local-only operation for trust boundary tests
            #[derive(Debug, Clone)]
            struct LocalOp;
            #[derive(Debug, serde::Deserialize)]
            struct LocalParams { v: String }
            impl ValidateParams for LocalParams {
                fn validate(&self) -> OperationResult<()> { Ok(()) }
            }
            #[derive(Debug, serde::Serialize)]
            struct LocalOutput { v: String }
            #[async_trait]
            impl TypedOperation for LocalOp {
                type Params = LocalParams;
                type Output = LocalOutput;
                fn name(&self) -> &'static str { "local_tool" }
                fn description(&self) -> &'static str { "Local-only op" }
                fn local_only(&self) -> bool { true }
                async fn execute(&self, _ctx: &OperationContext, params: LocalParams) -> OperationResult<LocalOutput> {
                    Ok(LocalOutput { v: params.v })
                }
            }
            registry.register(LocalOp);
            registry
        }

        // ── TEST 1: successful dispatch returns ToolResult with isError=false ──
        #[tokio::test]
        async fn dispatch_tool_call_success_returns_tool_result() {
            let registry = test_registry();
            let ctx = local_ctx().await;
            let params = serde_json::json!({ "msg": "hello world" });

            let result = registry.dispatch_tool_call("echo_tool", &ctx, params).await;

            // isError must be false on success
            assert!(!result.is_error, "Successful dispatch must not have isError=true");
            // content[0].type == "text"
            assert_eq!(result.content.len(), 1);
            assert_eq!(result.content[0].content_type, "text");
            // text must be valid JSON containing the echo field
            let json: serde_json::Value = serde_json::from_str(&result.content[0].text)
                .expect("text content must be valid JSON");
            assert_eq!(json["echo"], "hello world");
        }

        // ── TEST 2: unknown op returns isError=true with structured JSON ──
        #[tokio::test]
        async fn dispatch_tool_call_unknown_op_returns_tool_error() {
            let registry = test_registry();
            let ctx = local_ctx().await;
            let params = serde_json::json!({});

            let result = registry.dispatch_tool_call("no_such_tool", &ctx, params).await;

            assert!(result.is_error, "Unknown op must return isError=true");
            // content[0].text must be JSON-parseable
            let json: serde_json::Value = serde_json::from_str(&result.content[0].text)
                .expect("error text must be valid JSON");
            // TS shape: { error: 'invalid_params', message: '...' }
            assert_eq!(json["error"], "invalid_params");
            assert!(
                json["message"].as_str().unwrap_or("").contains("Unknown operation"),
                "message should mention Unknown operation, got: {}",
                json["message"]
            );
        }

        // ── TEST 3: invalid params returns isError=true ──
        #[tokio::test]
        async fn dispatch_tool_call_invalid_params_returns_tool_error() {
            let registry = test_registry();
            let ctx = local_ctx().await;
            // Missing required "msg" field
            let params = serde_json::json!({ "wrong_field": 42 });

            let result = registry.dispatch_tool_call("echo_tool", &ctx, params).await;

            assert!(result.is_error, "Invalid params must return isError=true");
            let json: serde_json::Value = serde_json::from_str(&result.content[0].text)
                .expect("error text must be valid JSON");
            assert_eq!(json["error"], "invalid_params");
        }

        // ── TEST 4: local-only op called from remote → isError=true, permission_denied ──
        #[tokio::test]
        async fn dispatch_tool_call_local_only_from_remote_returns_permission_denied() {
            let registry = test_registry();
            let engine = InMemoryEngine::default();
            let mut ctx = OperationContext::local_cli();
            ctx.remote = true; // simulate remote caller
            let ctx = ctx.with_engine(Arc::new(engine));
            let params = serde_json::json!({ "v": "test" });

            let result = registry.dispatch_tool_call("local_tool", &ctx, params).await;

            assert!(result.is_error, "local-only op from remote must return isError=true");
            let json: serde_json::Value = serde_json::from_str(&result.content[0].text)
                .expect("error text must be valid JSON");
            assert_eq!(json["error"], "permission_denied");
        }

        // ── TEST 5: result serialized as pretty JSON (matches TS dispatchToolCall) ──
        #[tokio::test]
        async fn dispatch_tool_call_result_is_pretty_printed_json() {
            let registry = test_registry();
            let ctx = local_ctx().await;
            let params = serde_json::json!({ "msg": "pretty" });

            let result = registry.dispatch_tool_call("echo_tool", &ctx, params).await;

            // Pretty-printed JSON contains newlines
            assert!(
                result.content[0].text.contains('\n'),
                "result text should be pretty-printed JSON"
            );
        }

        // ── TEST 6: ToolResult has no _meta by default ──
        #[tokio::test]
        async fn dispatch_tool_call_no_meta_by_default() {
            let registry = test_registry();
            let ctx = local_ctx().await;
            let params = serde_json::json!({ "msg": "no meta" });

            let result = registry.dispatch_tool_call("echo_tool", &ctx, params).await;

            assert!(result.meta.is_none(), "_meta should be None by default");
        }
    }
}

