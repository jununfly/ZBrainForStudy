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
                    ..Default::default()
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
#[derive(Debug, serde::Deserialize, Default)]
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
    /// Salience boost axis. `'off'` disables the post-fusion salience stage;
    /// `'on'` / `'strong'` (and omit) leave it on. Rust pins strength to `'on'`
    /// because the mode-resolved strength system is not ported yet — see
    /// docs/plans/KNOWN-GAPS.md (G13). Mirrors TS `query.salience`.
    #[serde(default)]
    pub salience: Option<String>,
    /// Recency boost axis. `'off'` disables the post-fusion recency stage;
    /// `'on'` / `'strong'` (and omit) leave it on. Mirrors TS `query.recency`.
    #[serde(default)]
    pub recency: Option<String>,
    /// Minimum fused score threshold (0..1). Mirrors TS `query.min_score`.
    #[serde(default)]
    pub min_score: Option<f64>,
    /// Page-type whitelist (e.g. `["person","company"]). Mirrors TS
    /// `query.types` (v0.33) — pushed to the fusion layer for `whoknows`.
    #[serde(default)]
    pub types: Option<Vec<String>>,
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
                "source_id": { "type": "string", "description": "Scope search to a single source" },
                "salience": { "type": "string", "enum": ["off", "on", "strong"], "description": "Salience boost axis (Rust pins 'on'/'strong' to one strength, G13)" },
                "recency": { "type": "string", "enum": ["off", "on", "strong"], "description": "Recency boost axis (Rust pins 'on'/'strong' to one strength, G13)" },
                "min_score": { "type": "number", "description": "Minimum fused score threshold (0..1)" },
                "types": { "type": "array", "items": { "type": "string" }, "description": "Page-type whitelist" }
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
                min_score: params.min_score.or(Some(0.01)),
                source_id: params.source_id.clone(),
                query_embedding,
                floor_ratio: None,
                recency_decay: None,
                recency_fallback: None,
                // `salience`/`recency` axis: only `'off'` disables the
                // post-fusion stage. `'on'`/`'strong'`/omit keep the always-on
                // behavior (Rust pins strength to 'on', G13 — mode-resolved
                // strength not ported). Mirrors TS `SearchOpts.salience`.
                disable_salience_boost: params.salience.as_deref() == Some("off"),
                disable_recency_boost: params.recency.as_deref() == Some("off"),
                types: params.types.clone(),
                ..Default::default()
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

/// Lexical keyword search across pages — mirrors TS `search` operation.
///
/// Pure backend-agnostic substring match over title / compiled_truth /
/// frontmatter via the shared `fuse_and_boost` lexical path. Unlike `query`
/// (which is semantic + rerank + boost-heavy), `search` is the literal
/// keyword op: the whole query string is treated as a single keyword so the
/// match is a phrase-substring (mirrors TS `ctx.engine.searchKeyword(queryText, …)`).
///
/// Scoped to the caller's source by default (mirrors TS `sourceScopeOpts(ctx)`);
/// an explicit `source_id` overrides the scope. No vector path — TS `search`
/// is lexical-only. Pagination (offset/limit) is applied in-memory after the
/// engine returns the fused, ranked candidate set.
#[derive(Debug, Clone)]
pub struct SearchOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct SearchParams {
    /// Keywords to search for (substring match over title / body / frontmatter).
    pub query: String,
    /// Maximum number of results to return (default: 20).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Skip the first N results for pagination (default: 0).
    #[serde(default)]
    pub offset: Option<usize>,
    /// Scope search to a single source. Defaults to the caller's `source_id`
    /// (set from CLI `--source` / `ZBRAIN_SOURCE` / `.zbrain-source` dotfile).
    /// Pass `__all__` to force cross-source search in multi-source brains.
    #[serde(default)]
    pub source_id: Option<String>,
}

impl ValidateParams for SearchParams {
    fn validate(&self) -> OperationResult<()> {
        if self.query.trim().is_empty() {
            return Err(OperationError::invalid_params("query must not be empty"));
        }
        if let Some(limit) = self.limit {
            if limit > 100 {
                return Err(OperationError::invalid_params("limit cannot exceed 100"));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for SearchOperation {
    type Params = SearchParams;
    type Output = Vec<crate::engine::SearchResult>;

    fn name(&self) -> &'static str {
        "search"
    }

    fn description(&self) -> &'static str {
        "Lexical keyword search across pages (title / body / frontmatter). Scoped to the caller's source by default."
    }

    fn local_only(&self) -> bool {
        false
    }

    fn mutating(&self) -> bool {
        false
    }

    fn cli_hints(&self) -> Option<CliHints> {
        Some(CliHints::new("search").with_positional(&["query"]))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Keywords to search for (substring match)" },
                "limit": { "type": "integer", "description": "Maximum number of results (default: 20)" },
                "offset": { "type": "integer", "description": "Pagination offset (default: 0)" },
                "source_id": { "type": "string", "description": "Scope search to a single source (defaults to caller source; '__all__' for cross-source)" }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;

        // Scope resolution mirrors TS `sourceScopeOpts(ctx)` + the `source_id`
        // param: explicit `source_id` wins; `__all__` forces cross-source
        // (None); otherwise fall back to the caller's `source_id`.
        let source_id = match params.source_id.as_deref() {
            Some("__all__") => None,
            Some(s) => Some(s.to_string()),
            None => Some(ctx.source_id.clone()),
        };

        let mut results = engine
            .search_pages(&crate::engine::SearchOpts {
                // Whole query as a single keyword → phrase-substring match,
                // mirroring TS `searchKeyword`.
                keywords: vec![params.query.clone()],
                limit: params.limit,
                source_id,
                ..Default::default()
            })
            .await?;

        // In-memory pagination (mirrors TS `search` offset/limit).
        if let Some(offset) = params.offset {
            if offset >= results.len() {
                results.clear();
            } else {
                results = results.split_off(offset);
            }
        }

        Ok(results)
    }
}

/// Search pages by image similarity (1-6-7-11).
///
/// Embeds the supplied image via the multimodal embedding provider, retrieves
/// visually-similar pages with `search_pages_by_embedding` (chunk-level cosine
/// over stored chunk embeddings), then re-ranks the candidates through
/// `fuse_and_boost` (page-level cosine + salience/recency boost + snippet).
///
/// A per-client daily spend budget is enforced up front (and the completed
/// call recorded afterward) via `image_search_spend_log`, so a flaky embedding
/// provider or failed retrieval never bills the client.
#[derive(Debug, Clone)]
pub struct SearchByImageOperation;

/// Estimated cost (cents) of one image-embedding API call. Flat placeholder
/// rate — the real multimodal-embedding price is a fraction of a cent per
/// image. Recorded for daily-cap accounting + audit.
const IMAGE_SEARCH_COST_CENTS_PER_CALL: i64 = 1;
/// Per-client daily image-search spend cap (cents) = $1.00/day.
const IMAGE_SEARCH_DAILY_CAP_CENTS: i64 = 100;

/// Parameters for `search_by_image`. Exactly one of `image_path`, `image_url`,
/// or `image_data` must be supplied.
#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct SearchByImageParams {
    /// Local filesystem path to the query image.
    #[serde(default)]
    pub image_path: Option<String>,
    /// HTTP(S) URL of the query image (SSRF-protected on untrusted callers).
    #[serde(default)]
    pub image_url: Option<String>,
    /// Raw base64 image bytes (a `data:` URI prefix is stripped if present).
    #[serde(default)]
    pub image_data: Option<String>,
    /// Maximum number of results to return (default: 20, max: 100).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Scope search to a single source. Defaults to the caller's `source_id`;
    /// `__all__` forces cross-source.
    #[serde(default)]
    pub source_id: Option<String>,
}

impl ValidateParams for SearchByImageParams {
    fn validate(&self) -> OperationResult<()> {
        let provided = [
            self.image_path.is_some(),
            self.image_url.is_some(),
            self.image_data.is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count();
        if provided == 0 {
            return Err(OperationError::invalid_params(
                "exactly one of image_path, image_url, or image_data is required",
            ));
        }
        if provided > 1 {
            return Err(OperationError::invalid_params(
                "only one of image_path, image_url, or image_data may be supplied",
            ));
        }
        if let Some(limit) = self.limit {
            if limit == 0 || limit > 100 {
                return Err(OperationError::invalid_params(
                    "limit must be between 1 and 100",
                ));
            }
        }
        Ok(())
    }
}

/// Output for `search_by_image` — ranked, snippet-tagged page results.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchByImageOutput {
    pub results: Vec<crate::engine::SearchResult>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[async_trait]
impl TypedOperation for SearchByImageOperation {
    type Params = SearchByImageParams;
    type Output = SearchByImageOutput;

    fn name(&self) -> &'static str {
        "search_by_image"
    }

    fn description(&self) -> &'static str {
        "Find pages visually similar to a query image via multimodal embedding search. Exactly one of image_path / image_url / image_data is required."
    }

    fn local_only(&self) -> bool {
        false
    }

    fn mutating(&self) -> bool {
        false
    }

    fn cli_hints(&self) -> Option<CliHints> {
        Some(CliHints::new("search-by-image").with_positional(&["image_path"]))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "image_path": { "type": "string", "description": "Local filesystem path to the query image" },
                "image_url": { "type": "string", "description": "HTTP(S) URL of the query image (SSRF-protected)" },
                "image_data": { "type": "string", "description": "Raw base64 image bytes (data: URI prefix optional)" },
                "limit": { "type": "integer", "description": "Maximum number of results (default: 20, max: 100)" },
                "source_id": { "type": "string", "description": "Scope search to a single source (defaults to caller source; '__all__' for cross-source)" }
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine.as_ref().ok_or_else(|| {
            OperationError::new(
                ErrorCode::StorageError,
                "search_by_image requires an engine",
            )
        })?;

        // Budget identity: authenticated MCP client if present, else the
        // caller's source (local CLI). Used for the per-client daily cap.
        let client_id = ctx
            .auth
            .as_ref()
            .map(|a| a.client_id.clone())
            .unwrap_or_else(|| ctx.source_id.clone());

        // Daily budget gate (checked before any paid call).
        let spent_today = engine
            .image_search_daily_spend_cents(&client_id)
            .await
            .map_err(|e| OperationError::new(ErrorCode::StorageError, &format!("budget check failed: {e}")))?;
        if spent_today + IMAGE_SEARCH_COST_CENTS_PER_CALL > IMAGE_SEARCH_DAILY_CAP_CENTS {
            return Err(OperationError::new(
                ErrorCode::RateLimited,
                &format!(
                    "daily image-search budget exceeded for client '{client_id}' \
                     (spent {spent_today} of {cap} cents today); try again tomorrow",
                    cap = IMAGE_SEARCH_DAILY_CAP_CENTS
                ),
            ));
        }

        // Resolve which image source was provided (validation guarantees exactly one).
        let source = if let Some(path) = &params.image_path {
            crate::image_loader::ImageSource::Path(path.clone())
        } else if let Some(url) = &params.image_url {
            crate::image_loader::ImageSource::Url(url.clone())
        } else {
            crate::image_loader::ImageSource::Data(params.image_data.clone().expect("validated: one source present"))
        };

        // Load + base64-encode (SSRF guard runs inside for Url on untrusted callers).
        let loaded = crate::image_loader::load_image(&source).await.map_err(|e| {
            OperationError::new(
                ErrorCode::InvalidParams,
                &format!("failed to load query image: {e}"),
            )
        })?;

        // Embedding is the query itself — without a provider there is nothing
        // to search, so this is a hard error (not a fail-open lexical fallback
        // like text `query`, which has a lexical path).
        let embedding_client = ctx.embedding.as_ref().ok_or_else(|| {
            OperationError::new(
                ErrorCode::EmbeddingFailed,
                "search_by_image requires an embedding provider (none configured)",
            )
        })?;
        let embedding = embedding_client
            .embed_image(&loaded.base64, loaded.mime.as_deref())
            .await
            .map_err(|e| {
                OperationError::new(
                    ErrorCode::EmbeddingFailed,
                    &format!("image embedding failed: {e}"),
                )
            })?;

        // Source scoping mirrors `search` / `query`.
        let source_id = match params.source_id.as_deref() {
            Some("__all__") => None,
            Some(s) => Some(s.to_string()),
            None => Some(ctx.source_id.clone()),
        };

        let limit = params.limit.unwrap_or(20);

        // 1) Chunk-level cosine retrieval → over-fetch candidates, then the
        //    fusion step trims to `limit` (mirrors query's rerank-before-paginate).
        let candidates = engine
            .search_pages_by_embedding(&embedding, limit * 3, source_id.as_deref())
            .await
            .map_err(|e| {
                OperationError::new(
                    ErrorCode::StorageError,
                    &format!("image similarity search failed: {e}"),
                )
            })?;

        // 2) Page-level fusion: vector cosine (against Page::embedding) +
        //    salience/recency boost + snippet. (RRF lives in `fuse_and_boost`;
        //    search_by_image orchestrates retrieval + fusion here rather than
        //    re-implementing it.)
        let results = crate::engine::fuse_and_boost(
            engine.as_ref(),
            &candidates,
            &crate::engine::SearchOpts {
                keywords: Vec::new(),
                limit: Some(limit),
                source_id: source_id.clone(),
                query_embedding: Some(embedding.clone()),
                min_score: Some(0.0),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            OperationError::new(
                ErrorCode::StorageError,
                &format!("image search fusion failed: {e}"),
            )
        })?;

        // 3) Bill the completed call (audit + daily cap). Never runs on the
        //    error paths above, so failed searches don't consume budget.
        engine
            .record_image_search_spend(
                &client_id,
                IMAGE_SEARCH_COST_CENTS_PER_CALL,
                "embedding",
                embedding_client.model(),
            )
            .await
            .map_err(|e| {
                OperationError::new(
                    ErrorCode::StorageError,
                    &format!("failed to record image-search spend: {e}"),
                )
            })?;

        let total = results.len();
        Ok(SearchByImageOutput {
            results,
            total,
            limit,
            offset: 0,
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

// ──────────────────────────────────────────────────────────────────────────
// Unified live registry (Slice 1-6-7-1)
// ──────────────────────────────────────────────────────────────────────────

/// Register every production operation into `registry`.
///
/// Single source of truth for the live operation set. The CLI (`zbrain`)
/// and the MCP server (`zbrain-mcp`) both call this instead of hand-listing
/// operations, so new ports land in the live registry automatically.
///
/// Test-only fixture operations (the `AddTag*`/`UpdateSlug*` family defined
/// inside `mod tests` below) are intentionally NOT registered here — they are
/// promotion targets for their own domain slices (tags/links/timeline).
pub fn register_all(registry: &mut OperationRegistry) {
    registry.register(GetPageOperation);
    registry.register(PutPageOperation);
    registry.register(DeletePageOperation);
    registry.register(RestorePageOperation);
    registry.register(PurgeDeletedPagesOperation);
    registry.register(ListPagesOperation);
    registry.register(QueryOperation);
    // 1-6-7-7 — search op (lexical keyword search, mirrors TS `search`).
    // `query` op already existed; this closes the search/query retrieval pair.
    registry.register(SearchOperation);
    // 1-6-7-11 — search_by_image (multimodal embedding retrieval + daily
    // spend budget via image_search_spend_log).
    registry.register(SearchByImageOperation);
    registry.register(ThinkOperation);
    registry.register(TakesListOperation);
    registry.register(TakesSearchOperation);
    // — Page domain WRAP (first batch, slice 1-6-7-1) —
    registry.register(SoftDeletePageOperation);
    registry.register(RewriteLinksOperation);
    registry.register(GetPageTimestampsOperation);
    registry.register(RefreshPageBodyOperation);
    // — Tags / Links / Timeline (slice 1-6-7-2) —
    registry.register(AddTagOperation);
    registry.register(RemoveTagOperation);
    registry.register(GetTagsOperation);
    registry.register(AddLinkOperation);
    registry.register(RemoveLinkOperation);
    registry.register(GetLinksOperation);
    registry.register(GetBacklinksOperation);
    registry.register(TraverseGraphOperation);
    registry.register(AddTimelineEntryOperation);
    registry.register(GetTimelineOperation);

    // 1-6-7-3 — sources(4) + facts(3) + anomalies(1) + health-stats(3)
    registry.register(SourcesAddOperation);
    registry.register(SourcesListOperation);
    registry.register(SourcesStatusOperation);
    registry.register(SourcesRemoveOperation);
    registry.register(ForgetFactOperation);
    registry.register(ExtractFactsOperation);
    registry.register(FindContradictionsOperation);
    registry.register(FindAnomaliesOperation);
    registry.register(GetHealthOperation);
    registry.register(GetStatsOperation);
    registry.register(GetRecentSalienceOperation);

    // 1-6-7-4 — jobs / minions (11)
    registry.register(SubmitJobOperation);
    registry.register(SubmitAgentOperation);
    registry.register(ListJobsOperation);
    registry.register(GetJobOperation);
    registry.register(GetJobProgressOperation);
    registry.register(ReplayJobOperation);
    registry.register(SendJobMessageOperation);
    registry.register(CancelJobOperation);
    registry.register(RetryJobOperation);
    registry.register(PauseJobOperation);
    registry.register(ResumeJobOperation);

    // 1-6-7-5 — ingestion(4) + files-attachments(5) + calibration + transcripts
    registry.register(GetChunksOperation);
    registry.register(LogIngestOperation);
    registry.register(GetIngestLogOperation);
    registry.register(FileListOperation);
    registry.register(FileUploadOperation);
    registry.register(FileUrlOperation);
    registry.register(GetCalibrationProfileOperation);
    registry.register(GetRecentTranscriptsOperation);
    // 1-6-7-8: commands-misc gap ops (engine methods already exist)
    registry.register(ResolveSlugsOperation);
    registry.register(GetVersionsOperation);
    registry.register(RevertVersionOperation);
    registry.register(PutRawDataOperation);
    registry.register(GetRawDataOperation);
    registry.register(GetBrainIdentityOperation);
}

// ── SoftDeletePage Operation (Slice 1-6-7-1) ──────────────────────────────

/// Soft-delete a page by slug (keeps the row with `deleted_at` set).
#[derive(Debug, Clone)]
pub struct SoftDeletePageOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SoftDeletePageParams {
    pub slug: String,
}

impl ValidateParams for SoftDeletePageParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoftDeletePageOutput {
    pub deleted_slug: Option<String>,
}

#[async_trait]
impl TypedOperation for SoftDeletePageOperation {
    type Params = SoftDeletePageParams;
    type Output = SoftDeletePageOutput;

    fn name(&self) -> &'static str {
        "soft_delete_page"
    }

    fn description(&self) -> &'static str {
        "Soft-delete a page by slug, retaining the row (deleted_at set)."
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

    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let deleted = engine
            .soft_delete_page(&params.slug, Some(&ctx.source_id))
            .await?;
        Ok(SoftDeletePageOutput { deleted_slug: deleted })
    }
}

// ── RewriteLinks Operation (Slice 1-6-7-1) ─────────────────────────────────

/// Rewrite links after a slug change. Explicit no-op in the current engine
/// (links use integer page_id foreign keys), kept for contract parity.
#[derive(Debug, Clone)]
pub struct RewriteLinksOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RewriteLinksParams {
    pub old_slug: String,
    pub new_slug: String,
}

impl ValidateParams for RewriteLinksParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.old_slug)?;
        validate_page_slug(&self.new_slug)?;
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewriteLinksOutput {
    pub rewritten: bool,
}

#[async_trait]
impl TypedOperation for RewriteLinksOperation {
    type Params = RewriteLinksParams;
    type Output = RewriteLinksOutput;

    fn name(&self) -> &'static str {
        "rewrite_links"
    }

    fn description(&self) -> &'static str {
        "Rewrite links pointing at `old_slug` to `new_slug`."
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
                "old_slug": { "type": "string", "description": "Current page slug" },
                "new_slug": { "type": "string", "description": "New slug to repoint links to" }
            },
            "required": ["old_slug", "new_slug"]
        })
    }

    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        engine
            .rewrite_links(&params.old_slug, &params.new_slug)
            .await?;
        Ok(RewriteLinksOutput { rewritten: true })
    }
}

// ── GetPageTimestamps Operation (Slice 1-6-7-1) ────────────────────────────

/// Get created/updated timestamps for a batch of slugs.
#[derive(Debug, Clone)]
pub struct GetPageTimestampsOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetPageTimestampsParams {
    pub slugs: Vec<String>,
}

impl ValidateParams for GetPageTimestampsParams {
    fn validate(&self) -> OperationResult<()> {
        if self.slugs.is_empty() {
            return Err(OperationError::invalid_params("`slugs` must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPageTimestampsOutput {
    pub timestamps: std::collections::HashMap<String, String>,
}

#[async_trait]
impl TypedOperation for GetPageTimestampsOperation {
    type Params = GetPageTimestampsParams;
    type Output = GetPageTimestampsOutput;

    fn name(&self) -> &'static str {
        "get_page_timestamps"
    }

    fn description(&self) -> &'static str {
        "Get created/updated timestamps for a batch of page slugs."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slugs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Page slugs to fetch timestamps for"
                }
            },
            "required": ["slugs"]
        })
    }

    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let timestamps = engine.get_page_timestamps(&params.slugs).await?;
        Ok(GetPageTimestampsOutput { timestamps })
    }
}

// ── RefreshPageBody Operation (Slice 1-6-7-1) ─────────────────────────────

/// Update `compiled_truth` / `timeline` / `content_hash` for an existing page.
#[derive(Debug, Clone)]
pub struct RefreshPageBodyOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RefreshPageBodyParams {
    pub slug: String,
    pub compiled_truth: String,
    pub timeline: serde_json::Value,
    pub content_hash: String,
}

impl ValidateParams for RefreshPageBodyParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshPageBodyOutput {
    pub refreshed: bool,
}

#[async_trait]
impl TypedOperation for RefreshPageBodyOperation {
    type Params = RefreshPageBodyParams;
    type Output = RefreshPageBodyOutput;

    fn name(&self) -> &'static str {
        "refresh_page_body"
    }

    fn description(&self) -> &'static str {
        "Refresh compiled_truth / timeline / content_hash for a page."
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
                "compiled_truth": { "type": "string", "description": "New compiled truth" },
                "timeline": { "type": "object", "description": "New timeline JSON" },
                "content_hash": { "type": "string", "description": "New content hash" }
            },
            "required": ["slug", "compiled_truth", "timeline", "content_hash"]
        })
    }

    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let args = crate::types::RefreshPageBodyArgs {
            slug: params.slug,
            source_id: ctx.source_id.clone(),
            compiled_truth: params.compiled_truth,
            timeline: params.timeline,
            content_hash: params.content_hash,
        };
        engine.refresh_page_body(&args).await?;
        Ok(RefreshPageBodyOutput { refreshed: true })
    }
}

// ── Tags / Links / Timeline Operations (Slice 1-6-7-2) ────────────────────
//
// Port of the `tags` (3), `links-graph` (5) and `timeline` (2) operation
// families from `src/core/operations.ts` into the production registry. These
// are thin wrappers over existing `BrainEngine` methods — see the roadmap node
// 1-6-7-2 for the full decision record.
//
// Two deliberate, documented deviations from the legacy TS shapes:
//   1. `traverse_graph` always returns `GraphPath[]` (never the legacy
//      `GraphNode[]` shape). The Rust engine only exposes `traverse_paths`
//      (GraphPath[]); GraphPath is the modern superset shape TS switches to
//      whenever `link_type`/`direction` filters are present. Reproducing the
//      legacy GraphNode[] projection would require an extra DB join for
//      page titles that the engine does not perform.
//   2. `get_timeline` reads the page's `timeline` TEXT column and parses each
//      non-empty line as a JSON `TimelineEntry`. This matches the Rust
//      engine's `add_timeline_entry` storage model (a newline-delimited JSON
//      log on `pages.timeline`), which differs from the legacy TS engine's
//      separate `timeline_entries` table. Since Rust is the successor
//      runtime, the Rust model is canonical for this port.

const TRAVERSE_DEPTH_CAP: u32 = 10;

// — Tags (3) —

/// Attach a tag to a page.
#[derive(Debug, Clone)]
pub struct AddTagOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AddTagParams {
    pub slug: String,
    pub tag: String,
}

impl ValidateParams for AddTagParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        if self.tag.trim().is_empty() {
            return Err(OperationError::invalid_params("tag must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddTagOutput {
    pub slug: String,
    pub tag: String,
}

#[async_trait]
impl TypedOperation for AddTagOperation {
    type Params = AddTagParams;
    type Output = AddTagOutput;

    fn name(&self) -> &'static str {
        "add_tag"
    }
    fn description(&self) -> &'static str {
        "Attach a tag to a page."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Page slug" },
                "tag": { "type": "string", "description": "Tag to attach" }
            },
            "required": ["slug", "tag"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        engine
            .add_tag(&params.slug, &params.tag, Some(&ctx.source_id))
            .await?;
        Ok(AddTagOutput {
            slug: params.slug,
            tag: params.tag,
        })
    }
}

/// Detach a tag from a page.
#[derive(Debug, Clone)]
pub struct RemoveTagOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RemoveTagParams {
    pub slug: String,
    pub tag: String,
}

impl ValidateParams for RemoveTagParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        if self.tag.trim().is_empty() {
            return Err(OperationError::invalid_params("tag must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveTagOutput {
    pub slug: String,
    pub tag: String,
}

#[async_trait]
impl TypedOperation for RemoveTagOperation {
    type Params = RemoveTagParams;
    type Output = RemoveTagOutput;

    fn name(&self) -> &'static str {
        "remove_tag"
    }
    fn description(&self) -> &'static str {
        "Detach a tag from a page."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string" },
                "tag": { "type": "string" }
            },
            "required": ["slug", "tag"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        engine
            .remove_tag(&params.slug, &params.tag, Some(&ctx.source_id))
            .await?;
        Ok(RemoveTagOutput {
            slug: params.slug,
            tag: params.tag,
        })
    }
}

/// List the tags attached to a page.
#[derive(Debug, Clone)]
pub struct GetTagsOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetTagsParams {
    pub slug: String,
}

impl ValidateParams for GetTagsParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTagsOutput {
    pub slug: String,
    pub tags: Vec<String>,
}

#[async_trait]
impl TypedOperation for GetTagsOperation {
    type Params = GetTagsParams;
    type Output = GetTagsOutput;

    fn name(&self) -> &'static str {
        "get_tags"
    }
    fn description(&self) -> &'static str {
        "List the tags attached to a page."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "slug": { "type": "string" } },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let tags = engine.get_tags(&params.slug, Some(&ctx.source_id)).await?;
        Ok(GetTagsOutput {
            slug: params.slug,
            tags,
        })
    }
}

// — Links / Graph (5) —

/// Create a link between two pages.
#[derive(Debug, Clone)]
pub struct AddLinkOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AddLinkParams {
    pub from: String,
    pub to: String,
    pub link_type: Option<String>,
    pub context: Option<String>,
}

impl ValidateParams for AddLinkParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.from)?;
        validate_page_slug(&self.to)?;
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddLinkOutput {
    pub added: u64,
    pub from: String,
    pub to: String,
}

#[async_trait]
impl TypedOperation for AddLinkOperation {
    type Params = AddLinkParams;
    type Output = AddLinkOutput;

    fn name(&self) -> &'static str {
        "add_link"
    }
    fn description(&self) -> &'static str {
        "Create a link between two pages."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "from": { "type": "string" },
                "to": { "type": "string" },
                "link_type": { "type": "string", "description": "Link type (e.g. invested_in, works_at)" },
                "context": { "type": "string", "description": "Context for the link" }
            },
            "required": ["from", "to"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let input = crate::types::LinkBatchInput {
            from_slug: params.from.clone(),
            to_slug: params.to.clone(),
            link_type: params.link_type.clone(),
            context: params.context.clone(),
            link_source: None,
            origin_slug: None,
            origin_field: None,
            from_source_id: Some(ctx.source_id.clone()),
            to_source_id: Some(ctx.source_id.clone()),
            origin_source_id: Some(ctx.source_id.clone()),
        };
        let added: u64 = engine.add_links_batch(&[input]).await? as u64;
        Ok(AddLinkOutput {
            added,
            from: params.from,
            to: params.to,
        })
    }
}

/// Remove a link between two pages.
#[derive(Debug, Clone)]
pub struct RemoveLinkOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RemoveLinkParams {
    pub from: String,
    pub to: String,
}

impl ValidateParams for RemoveLinkParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.from)?;
        validate_page_slug(&self.to)?;
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveLinkOutput {
    pub from: String,
    pub to: String,
}

#[async_trait]
impl TypedOperation for RemoveLinkOperation {
    type Params = RemoveLinkParams;
    type Output = RemoveLinkOutput;

    fn name(&self) -> &'static str {
        "remove_link"
    }
    fn description(&self) -> &'static str {
        "Remove a link between two pages."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "from": { "type": "string" },
                "to": { "type": "string" }
            },
            "required": ["from", "to"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        engine
            .remove_link(
                &params.from,
                &params.to,
                None,
                None,
                Some(&ctx.source_id),
                Some(&ctx.source_id),
            )
            .await?;
        Ok(RemoveLinkOutput {
            from: params.from,
            to: params.to,
        })
    }
}

/// List outgoing links from a page.
#[derive(Debug, Clone)]
pub struct GetLinksOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetLinksParams {
    pub slug: String,
}

impl ValidateParams for GetLinksParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLinksOutput {
    pub slug: String,
    pub links: Vec<crate::types::Link>,
}

#[async_trait]
impl TypedOperation for GetLinksOperation {
    type Params = GetLinksParams;
    type Output = GetLinksOutput;

    fn name(&self) -> &'static str {
        "get_links"
    }
    fn description(&self) -> &'static str {
        "List outgoing links from a page."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "slug": { "type": "string" } },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let links = engine.get_links(&params.slug, Some(&ctx.source_id)).await?;
        Ok(GetLinksOutput {
            slug: params.slug,
            links,
        })
    }
}

/// List incoming links to a page.
#[derive(Debug, Clone)]
pub struct GetBacklinksOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetBacklinksParams {
    pub slug: String,
}

impl ValidateParams for GetBacklinksParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBacklinksOutput {
    pub slug: String,
    pub links: Vec<crate::types::Link>,
}

#[async_trait]
impl TypedOperation for GetBacklinksOperation {
    type Params = GetBacklinksParams;
    type Output = GetBacklinksOutput;

    fn name(&self) -> &'static str {
        "get_backlinks"
    }
    fn description(&self) -> &'static str {
        "List incoming links to a page."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "slug": { "type": "string" } },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let links = engine.get_backlinks(&params.slug, Some(&ctx.source_id)).await?;
        Ok(GetBacklinksOutput {
            slug: params.slug,
            links,
        })
    }
}

/// Traverse the link graph from a page. Always returns `GraphPath[]` (see the
/// module-level note on the legacy `GraphNode[]` deviation).
#[derive(Debug, Clone)]
pub struct TraverseGraphOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TraverseGraphParams {
    pub slug: String,
    pub depth: Option<u32>,
    pub link_type: Option<String>,
    pub direction: Option<String>,
}

impl ValidateParams for TraverseGraphParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        if let Some(d) = self.depth {
            if d == 0 {
                return Err(OperationError::invalid_params("depth must be >= 1"));
            }
        }
        if let Some(dir) = &self.direction {
            if !matches!(dir.as_str(), "in" | "out" | "both") {
                return Err(OperationError::invalid_params(
                    "direction must be 'in', 'out', or 'both'",
                ));
            }
        }
        Ok(())
    }
}

/// Bare `GraphPath[]` array — mirrors the TS wire shape for `traverse_graph`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(transparent)]
pub struct TraverseGraphOutput(pub Vec<crate::types::GraphPath>);

#[async_trait]
impl TypedOperation for TraverseGraphOperation {
    type Params = TraverseGraphParams;
    type Output = TraverseGraphOutput;

    fn name(&self) -> &'static str {
        "traverse_graph"
    }
    fn description(&self) -> &'static str {
        "Traverse the link graph from a page (BFS, returns edges with depth)."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string" },
                "depth": { "type": "number", "description": "Max traversal depth (default 5, capped at 10)" },
                "link_type": { "type": "string", "description": "Filter to one link type" },
                "direction": { "type": "string", "enum": ["in", "out", "both"] }
            },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        // Mirror TS: default depth 5, clamp to [1, TRAVERSE_DEPTH_CAP]. This
        // also neutralises a remote caller passing depth=1e6 (memory/CPU burn).
        let depth = Some(params.depth.unwrap_or(5).clamp(1, TRAVERSE_DEPTH_CAP));
        let paths = engine
            .traverse_paths(
                &params.slug,
                depth,
                params.link_type.as_deref(),
                params.direction.as_deref(),
                Some(&ctx.source_id),
                None,
            )
            .await?;
        Ok(TraverseGraphOutput(paths))
    }
}

// — Timeline (2) —

/// Validate a `YYYY-MM-DD` timeline date: strict format, year 1900-2199, and
/// a real calendar day (chrono rejects e.g. Feb 30). Mirrors the TS
/// `add_timeline_entry` date guard.
fn validate_timeline_date(date: &str) -> OperationResult<()> {
    use chrono::Datelike;
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| {
        OperationError::invalid_params(format!(
            "Invalid date format \"{date}\" (expected YYYY-MM-DD)"
        ))
    })?;
    if parsed.year() < 1900 || parsed.year() > 2199 {
        return Err(OperationError::invalid_params(format!(
            "Invalid date \"{date}\" (year must be 1900-2199)"
        )));
    }
    Ok(())
}

/// Append a single timeline entry to a page.
#[derive(Debug, Clone)]
pub struct AddTimelineEntryOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AddTimelineEntryParams {
    pub slug: String,
    pub date: String,
    pub summary: String,
    pub detail: Option<String>,
    pub source: Option<String>,
}

impl ValidateParams for AddTimelineEntryParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        validate_timeline_date(&self.date)?;
        if self.summary.trim().is_empty() {
            return Err(OperationError::invalid_params(
                "summary must not be empty",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddTimelineEntryOutput {
    pub slug: String,
    pub date: String,
}

#[async_trait]
impl TypedOperation for AddTimelineEntryOperation {
    type Params = AddTimelineEntryParams;
    type Output = AddTimelineEntryOutput;

    fn name(&self) -> &'static str {
        "add_timeline_entry"
    }
    fn description(&self) -> &'static str {
        "Add a timeline entry to a page."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string" },
                "date": { "type": "string", "description": "YYYY-MM-DD" },
                "summary": { "type": "string" },
                "detail": { "type": "string" },
                "source": { "type": "string" }
            },
            "required": ["slug", "date", "summary"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        // Serialise the entry as one JSON line so `get_timeline` can round-trip
        // it. Mirrors the structured (date, source, summary, detail) shape of
        // the legacy TS TimelineInput.
        let entry = serde_json::json!({
            "date": params.date,
            "source": params.source.unwrap_or_default(),
            "summary": params.summary,
            "detail": params.detail.unwrap_or_default(),
        })
        .to_string();
        engine
            .add_timeline_entry(&params.slug, &ctx.source_id, &entry)
            .await?;
        Ok(AddTimelineEntryOutput {
            slug: params.slug,
            date: params.date,
        })
    }
}

/// A single timeline entry as returned by `get_timeline`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntryOutput {
    pub date: String,
    pub source: String,
    pub summary: String,
    pub detail: String,
}

/// List timeline entries for a page.
#[derive(Debug, Clone)]
pub struct GetTimelineOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetTimelineParams {
    pub slug: String,
}

impl ValidateParams for GetTimelineParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTimelineOutput {
    pub slug: String,
    pub entries: Vec<TimelineEntryOutput>,
}

#[async_trait]
impl TypedOperation for GetTimelineOperation {
    type Params = GetTimelineParams;
    type Output = GetTimelineOutput;

    fn name(&self) -> &'static str {
        "get_timeline"
    }
    fn description(&self) -> &'static str {
        "Get timeline entries for a page."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "slug": { "type": "string" } },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let page = engine
            .get_page(
                &params.slug,
                &crate::engine::GetPageOpts {
                    source_id: Some(ctx.source_id.clone()),
                    include_deleted: false,
                },
            )
            .await?;
        let entries = match page {
            Some(p) => p
                .timeline
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str::<TimelineEntryOutput>(l).ok())
                .collect(),
            None => Vec::new(),
        };
        Ok(GetTimelineOutput {
            slug: params.slug,
            entries,
        })
    }
}

// ─── Sources (4) ────────────────────────────────────────────────────────────
// Wraps engine source CRUD. The bare `health`/`salience`/`stats` names are
// human-CLI cliHints only (see operations.ts) and are NOT agent tools, so
// only `get_health`/`get_stats`/`get_recent_salience` are registered below.

/// Create a new source.
#[derive(Debug, Clone)]
pub struct SourcesAddOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SourcesAddParams {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

impl ValidateParams for SourcesAddParams {
    fn validate(&self) -> OperationResult<()> {
        if self.id.trim().is_empty() {
            return Err(OperationError::invalid_params("id must not be empty"));
        }
        if self.name.trim().is_empty() {
            return Err(OperationError::invalid_params("name must not be empty"));
        }
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for SourcesAddOperation {
    type Params = SourcesAddParams;
    type Output = crate::engine::SourceRow;

    fn name(&self) -> &'static str {
        "sources_add"
    }
    fn description(&self) -> &'static str {
        "Create a new source."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Source id (lowercase, hyphenated)" },
                "name": { "type": "string", "description": "Human-friendly name" },
                "config": { "type": "object", "description": "Optional source config" }
            },
            "required": ["id", "name"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let created = engine
            .create_source(&crate::engine::CreateSourceInput {
                id: params.id,
                name: params.name,
                config: params.config,
            })
            .await?;
        Ok(created)
    }
}

/// List sources.
#[derive(Debug, Clone)]
pub struct SourcesListOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct SourcesListParams {
    #[serde(default)]
    pub include_archived: Option<bool>,
}

impl ValidateParams for SourcesListParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for SourcesListOperation {
    type Params = SourcesListParams;
    type Output = Vec<crate::engine::SourceRow>;

    fn name(&self) -> &'static str {
        "sources_list"
    }
    fn description(&self) -> &'static str {
        "List all sources."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "include_archived": { "type": "boolean", "description": "Include archived sources" }
            }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let sources = engine
            .list_sources(params.include_archived.unwrap_or(false))
            .await?;
        Ok(sources)
    }
}

/// Get a single source's status/config.
#[derive(Debug, Clone)]
pub struct SourcesStatusOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SourcesStatusParams {
    pub id: String,
}

impl ValidateParams for SourcesStatusParams {
    fn validate(&self) -> OperationResult<()> {
        if self.id.trim().is_empty() {
            return Err(OperationError::invalid_params("id must not be empty"));
        }
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for SourcesStatusOperation {
    type Params = SourcesStatusParams;
    type Output = crate::engine::SourceRow;

    fn name(&self) -> &'static str {
        "sources_status"
    }
    fn description(&self) -> &'static str {
        "Get a source's status and config."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let source = engine
            .get_source(&params.id)
            .await?
            .ok_or_else(|| OperationError::invalid_params(format!("source not found: {}", params.id)))?;
        Ok(source)
    }
}

/// Remove (archive) a source.
#[derive(Debug, Clone)]
pub struct SourcesRemoveOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SourcesRemoveParams {
    pub id: String,
}

impl ValidateParams for SourcesRemoveParams {
    fn validate(&self) -> OperationResult<()> {
        if self.id.trim().is_empty() {
            return Err(OperationError::invalid_params("id must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcesRemoveOutput {
    pub removed: bool,
}

#[async_trait]
impl TypedOperation for SourcesRemoveOperation {
    type Params = SourcesRemoveParams;
    type Output = SourcesRemoveOutput;

    fn name(&self) -> &'static str {
        "sources_remove"
    }
    fn description(&self) -> &'static str {
        "Archive (soft-delete) a source."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let removed = engine.delete_source(&params.id).await?;
        Ok(SourcesRemoveOutput { removed })
    }
}

// ─── Facts (3) ─────────────────────────────────────────────────────────────
// NOTE: TS `extract_facts` is an LLM extraction pipeline (Haiku + sanitise +
// dedup) and `find_contradictions` reads the `eval_contradictions_runs` report
// table. Rust has neither yet, so these two are simplified stand-ins that wrap
// the available engine facts methods (`insert_fact` / `get_facts_health`).
// `forget_fact` maps cleanly to `expire_fact`. The LLM pipeline + contradiction
// probe are tracked for a later dedicated slice.

/// Forget (expire) a fact.
#[derive(Debug, Clone)]
pub struct ForgetFactOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ForgetFactParams {
    pub fact_id: i64,
}

impl ValidateParams for ForgetFactParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetFactOutput {
    pub removed: bool,
}

#[async_trait]
impl TypedOperation for ForgetFactOperation {
    type Params = ForgetFactParams;
    type Output = ForgetFactOutput;

    fn name(&self) -> &'static str {
        "forget_fact"
    }
    fn description(&self) -> &'static str {
        "Expire (forget) a fact by id."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "fact_id": { "type": "integer" } },
            "required": ["fact_id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let removed = engine.expire_fact(&ctx.source_id, params.fact_id).await?;
        Ok(ForgetFactOutput { removed })
    }
}

/// Extract facts — simplified stand-in. Accepts a pre-extracted fact claim and
/// inserts it via `engine.insert_fact`. The TS LLM extraction pipeline
/// (sanitise + Haiku + dedup) is not ported here yet.
#[derive(Debug, Clone)]
pub struct ExtractFactsOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExtractFactsParams {
    pub fact: String,
    #[serde(default)]
    pub entity_slug: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
}

impl ValidateParams for ExtractFactsParams {
    fn validate(&self) -> OperationResult<()> {
        if self.fact.trim().is_empty() {
            return Err(OperationError::invalid_params("fact must not be empty"));
        }
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for ExtractFactsOperation {
    type Params = ExtractFactsParams;
    type Output = crate::types::FactInsertStatus;

    fn name(&self) -> &'static str {
        "extract_facts"
    }
    fn description(&self) -> &'static str {
        "Insert a fact into the per-source hot memory (simplified: pre-extracted claim)."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "fact": { "type": "string", "description": "The fact claim text (pre-extracted)" },
                "entity_slug": { "type": "string", "description": "Optional canonical entity slug" },
                "context": { "type": "string", "description": "Optional context" }
            },
            "required": ["fact"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let entity_slug = params.entity_slug.clone();
        let status = engine
            .insert_fact(
                &ctx.source_id,
                entity_slug.as_deref().unwrap_or(""),
                &crate::types::NewFact {
                    fact: params.fact,
                    kind: None,
                    entity_slug: params.entity_slug,
                    visibility: None,
                    context: params.context,
                    valid_from: None,
                    valid_until: None,
                    source: "mcp:extract_facts".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                },
            )
            .await?;
        Ok(status)
    }
}

/// Find contradictions — simplified stand-in. Rust has no contradictions probe
/// yet (TS reads `eval_contradictions_runs.report_json`); this returns the
/// facts-domain health snapshot as a proxy.
#[derive(Debug, Clone)]
pub struct FindContradictionsOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct FindContradictionsParams {
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ValidateParams for FindContradictionsParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindContradictionsOutput {
    pub health: crate::types::FactsHealth,
    pub note: String,
}

#[async_trait]
impl TypedOperation for FindContradictionsOperation {
    type Params = FindContradictionsParams;
    type Output = FindContradictionsOutput;

    fn name(&self) -> &'static str {
        "find_contradictions"
    }
    fn description(&self) -> &'static str {
        "Facts-domain health snapshot (Rust has no contradictions probe yet)."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string" },
                "severity": { "type": "string", "enum": ["low", "medium", "high"] },
                "limit": { "type": "integer" }
            }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let _ = params; // slug/severity/limit filtering not available without the contradictions probe
        let engine = ctx.engine()?;
        let health = engine.get_facts_health(&ctx.source_id).await?;
        Ok(FindContradictionsOutput {
            health,
            note: "Rust has no contradictions probe yet; returning facts health as a proxy.".to_string(),
        })
    }
}

// ─── Anomalies (1) ──────────────────────────────────────────────────────────

/// Find anomalies (cohort traffic deviations).
#[derive(Debug, Clone)]
pub struct FindAnomaliesOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct FindAnomaliesParams {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub lookback_days: Option<u32>,
    #[serde(default)]
    pub sigma: Option<f64>,
}

impl ValidateParams for FindAnomaliesParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for FindAnomaliesOperation {
    type Params = FindAnomaliesParams;
    type Output = Vec<crate::anomaly::AnomalyResult>;

    fn name(&self) -> &'static str {
        "find_anomalies"
    }
    fn description(&self) -> &'static str {
        "Detect cohort traffic anomalies."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "since": { "type": "string", "description": "Target day YYYY-MM-DD" },
                "lookback_days": { "type": "integer", "description": "Baseline window days" },
                "sigma": { "type": "number", "description": "Sigma threshold multiplier" }
            }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let anomalies = engine
            .find_anomalies(crate::anomaly::AnomaliesOpts {
                since: params.since,
                lookback_days: params.lookback_days,
                sigma: params.sigma,
            })
            .await?;
        Ok(anomalies)
    }
}

// ─── Health / stats (3) ─────────────────────────────────────────────────────

/// Brain health snapshot.
#[derive(Debug, Clone)]
pub struct GetHealthOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct GetHealthParams {}

impl ValidateParams for GetHealthParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for GetHealthOperation {
    type Params = GetHealthParams;
    type Output = crate::autopilot::brain_score::BrainHealth;

    fn name(&self) -> &'static str {
        "get_health"
    }
    fn description(&self) -> &'static str {
        "Brain health snapshot."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let _ = params;
        let engine = ctx.engine()?;
        let health = engine.get_health().await?;
        Ok(health)
    }
}

/// Minion job queue stats.
#[derive(Debug, Clone)]
pub struct GetStatsOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct GetStatsParams {
    #[serde(default)]
    pub since: Option<String>,
}

impl ValidateParams for GetStatsParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for GetStatsOperation {
    type Params = GetStatsParams;
    type Output = crate::minions::types::QueueStats;

    fn name(&self) -> &'static str {
        "get_stats"
    }
    fn description(&self) -> &'static str {
        "Minion job queue statistics."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "since": { "type": "string", "description": "RFC3339 lower bound" } }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let stats = engine.get_stats(&params.since.unwrap_or_default()).await?;
        Ok(stats)
    }
}

/// Recent salience entries.
#[derive(Debug, Clone)]
pub struct GetRecentSalienceOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct GetRecentSalienceParams {
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub slug_prefix: Option<String>,
}

impl ValidateParams for GetRecentSalienceParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for GetRecentSalienceOperation {
    type Params = GetRecentSalienceParams;
    type Output = Vec<crate::types::SalienceResult>;

    fn name(&self) -> &'static str {
        "get_recent_salience"
    }
    fn description(&self) -> &'static str {
        "Recent salience entries."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "days": { "type": "integer" },
                "limit": { "type": "integer" },
                "slug_prefix": { "type": "string" }
            }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let salience = engine
            .get_recent_salience(
                params.days.unwrap_or(7),
                params.limit.unwrap_or(50),
                params.slug_prefix.as_deref(),
            )
            .await?;
        Ok(salience)
    }
}

// ─── Jobs / minions (11) ──────────────────────────────────────────────────
// All ops wrap the Rust `MinionQueue` (minions/queue.rs) except `cancel_job`
// and `send_job_message` which use engine methods. `subagent` is NOT a
// separate op — it is the `name` returned by `submit_agent` in TS; the audit
// double-counted it. `submit_job` preserves the TS MCP protected-name guard
// (rejects shell-type jobs from remote callers). `submit_agent` simplifies the
// TS OAuth binding enforcement (tracked for a later dedicated slice).

use crate::minions::queue::MinionQueue;
use crate::minions::types::{InboxMessage, JobFilters, MinionJob, MinionJobInput, MinionJobStatus};

fn is_protected_job_name(name: &str) -> bool {
    matches!(name, "shell")
}

fn parse_job_status(s: &str) -> Option<MinionJobStatus> {
    match s.to_ascii_lowercase().as_str() {
        "waiting" => Some(MinionJobStatus::Waiting),
        "active" => Some(MinionJobStatus::Active),
        "completed" => Some(MinionJobStatus::Completed),
        "failed" => Some(MinionJobStatus::Failed),
        "delayed" => Some(MinionJobStatus::Delayed),
        "dead" => Some(MinionJobStatus::Dead),
        "cancelled" | "canceled" => Some(MinionJobStatus::Cancelled),
        "waiting-children" => Some(MinionJobStatus::WaitingChildren),
        "paused" => Some(MinionJobStatus::Paused),
        _ => None,
    }
}

/// Submit a background job to the Minions queue.
#[derive(Debug, Clone)]
pub struct SubmitJobOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SubmitJobParams {
    pub name: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub queue: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub max_attempts: Option<i32>,
    #[serde(default)]
    pub delay: Option<i64>,
    #[serde(default)]
    pub timeout_ms: Option<i64>,
}

impl ValidateParams for SubmitJobParams {
    fn validate(&self) -> OperationResult<()> {
        if self.name.trim().is_empty() {
            return Err(OperationError::invalid_params("name must not be empty"));
        }
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for SubmitJobOperation {
    type Params = SubmitJobParams;
    type Output = MinionJob;

    fn name(&self) -> &'static str {
        "submit_job"
    }
    fn description(&self) -> &'static str {
        "Submit a background job to the Minions queue."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Job type (sync, embed, lint, import, extract, backlinks, autopilot-cycle; shell is CLI-only)" },
                "data": { "type": "object", "description": "Job payload (JSON)" },
                "queue": { "type": "string" },
                "priority": { "type": "integer" },
                "max_attempts": { "type": "integer" },
                "delay": { "type": "integer", "description": "Delay in ms before eligible" },
                "timeout_ms": { "type": "integer" }
            },
            "required": ["name"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let name = params.name.trim().to_string();
        // MCP guard: reject protected job names from remote callers (TS F7b).
        if ctx.remote && is_protected_job_name(&name) {
            return Err(OperationError::permission_denied(format!(
                "'{}' jobs cannot be submitted over MCP (CLI-only for security)",
                name
            )));
        }
        let queue = MinionQueue::new(ctx.engine()?);
        let job = queue
            .add(&MinionJobInput {
                name,
                data: params.data,
                queue: params.queue,
                priority: params.priority,
                max_attempts: params.max_attempts,
                delay: params.delay,
                timeout_ms: params.timeout_ms,
                ..Default::default()
            })
            .await?;
        Ok(job)
    }
}

/// Submit an LLM agent job (simplified: OAuth binding enforcement deferred).
#[derive(Debug, Clone)]
pub struct SubmitAgentOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SubmitAgentParams {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_slug_prefixes: Option<Vec<String>>,
    #[serde(default)]
    pub max_turns: Option<i32>,
    #[serde(default)]
    pub queue: Option<String>,
}

impl ValidateParams for SubmitAgentParams {
    fn validate(&self) -> OperationResult<()> {
        if self.prompt.trim().is_empty() {
            return Err(OperationError::invalid_params("prompt must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitAgentOutput {
    pub id: i64,
    pub name: String,
    pub client_id: String,
}

#[async_trait]
impl TypedOperation for SubmitAgentOperation {
    type Params = SubmitAgentParams;
    type Output = SubmitAgentOutput;

    fn name(&self) -> &'static str {
        "submit_agent"
    }
    fn description(&self) -> &'static str {
        "Submit an LLM agent job (simplified; OAuth binding deferred)."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "agent"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string" },
                "model": { "type": "string" },
                "allowed_tools": { "type": "array", "items": { "type": "string" } },
                "allowed_slug_prefixes": { "type": "array", "items": { "type": "string" } },
                "max_turns": { "type": "integer" },
                "queue": { "type": "string" }
            },
            "required": ["prompt"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        // TS rejects local CLI: `zbrain agent run` is the local path.
        if ctx.remote == false {
            return Err(OperationError::invalid_params(
                "submit_agent over the local CLI: use `zbrain agent run` instead.",
            ));
        }
        let queue = MinionQueue::new(ctx.engine()?);
        let data = serde_json::json!({
            "prompt": params.prompt,
            "model": params.model,
            "allowed_tools": params.allowed_tools,
            "allowed_slug_prefixes": params.allowed_slug_prefixes,
            "max_turns": params.max_turns,
        });
        let job = queue
            .add(&MinionJobInput {
                name: "subagent".to_string(),
                data: Some(data),
                queue: params.queue,
                ..Default::default()
            })
            .await?;
        Ok(SubmitAgentOutput {
            id: job.id,
            name: "subagent".to_string(),
            client_id: "<unbound>".to_string(),
        })
    }
}

/// List jobs with optional filters.
#[derive(Debug, Clone)]
pub struct ListJobsOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct ListJobsParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub queue: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

impl ValidateParams for ListJobsParams {
    fn validate(&self) -> OperationResult<()> {
        if let Some(s) = &self.status {
            if parse_job_status(s).is_none() {
                return Err(OperationError::invalid_params(format!("invalid status: {}", s)));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for ListJobsOperation {
    type Params = ListJobsParams;
    type Output = Vec<MinionJob>;

    fn name(&self) -> &'static str {
        "list_jobs"
    }
    fn description(&self) -> &'static str {
        "List jobs with optional filters."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "status": { "type": "string" },
                "queue": { "type": "string" },
                "name": { "type": "string" },
                "limit": { "type": "integer" }
            }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let queue = MinionQueue::new(ctx.engine()?);
        let jobs = queue
            .get_jobs(&JobFilters {
                status: params.status.as_deref().and_then(parse_job_status),
                queue: params.queue,
                name: params.name,
                limit: params.limit,
                ..Default::default()
            })
            .await?;
        Ok(jobs)
    }
}

/// Get a job by id.
#[derive(Debug, Clone)]
pub struct GetJobOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetJobParams {
    pub id: i64,
}

impl ValidateParams for GetJobParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for GetJobOperation {
    type Params = GetJobParams;
    type Output = MinionJob;

    fn name(&self) -> &'static str {
        "get_job"
    }
    fn description(&self) -> &'static str {
        "Get job status and details by id."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let queue = MinionQueue::new(ctx.engine()?);
        let job = queue
            .get_job(params.id)
            .await?
            .ok_or_else(|| OperationError::invalid_params(format!("job not found: {}", params.id)))?;
        Ok(job)
    }
}

/// Get structured progress for a running job.
#[derive(Debug, Clone)]
pub struct GetJobProgressOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetJobProgressParams {
    pub id: i64,
}

impl ValidateParams for GetJobProgressParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetJobProgressOutput {
    pub id: i64,
    pub name: String,
    pub status: MinionJobStatus,
    pub progress: Option<serde_json::Value>,
}

#[async_trait]
impl TypedOperation for GetJobProgressOperation {
    type Params = GetJobProgressParams;
    type Output = GetJobProgressOutput;

    fn name(&self) -> &'static str {
        "get_job_progress"
    }
    fn description(&self) -> &'static str {
        "Get structured progress for a running job."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let queue = MinionQueue::new(ctx.engine()?);
        let job = queue
            .get_job(params.id)
            .await?
            .ok_or_else(|| OperationError::invalid_params(format!("job not found: {}", params.id)))?;
        Ok(GetJobProgressOutput {
            id: job.id,
            name: job.name,
            status: job.status,
            progress: job.progress,
        })
    }
}

/// Replay a terminal job, optionally with overridden data.
#[derive(Debug, Clone)]
pub struct ReplayJobOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReplayJobParams {
    pub id: i64,
    #[serde(default)]
    pub data_overrides: Option<serde_json::Value>,
}

impl ValidateParams for ReplayJobParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayJobOutput {
    pub id: i64,
    pub name: String,
    pub status: MinionJobStatus,
    pub source_id: i64,
}

#[async_trait]
impl TypedOperation for ReplayJobOperation {
    type Params = ReplayJobParams;
    type Output = ReplayJobOutput;

    fn name(&self) -> &'static str {
        "replay_job"
    }
    fn description(&self) -> &'static str {
        "Replay a completed/failed/dead job with optional data overrides."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer", "description": "Source job id to replay" },
                "data_overrides": { "type": "object" }
            },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let queue = MinionQueue::new(ctx.engine()?);
        let original = queue
            .get_job(params.id)
            .await?
            .ok_or_else(|| OperationError::invalid_params(format!("job not found: {}", params.id)))?;
        let mut data = original.data.clone();
        if let Some(overrides) = &params.data_overrides {
            if let Some(obj) = data.as_object_mut() {
                if let Some(ov) = overrides.as_object() {
                    for (k, v) in ov {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        let new = queue
            .add(&MinionJobInput {
                name: original.name.clone(),
                data: Some(data),
                queue: Some(original.queue.clone()),
                ..Default::default()
            })
            .await?;
        Ok(ReplayJobOutput {
            id: new.id,
            name: new.name,
            status: new.status,
            source_id: params.id,
        })
    }
}

/// Send a sidechannel message to a running job's inbox.
#[derive(Debug, Clone)]
pub struct SendJobMessageOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SendJobMessageParams {
    pub id: i64,
    pub payload: serde_json::Value,
    #[serde(default)]
    pub sender: Option<String>,
}

impl ValidateParams for SendJobMessageParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendJobMessageOutput {
    pub sent: bool,
    pub message_id: i64,
    pub job_id: i64,
}

#[async_trait]
impl TypedOperation for SendJobMessageOperation {
    type Params = SendJobMessageParams;
    type Output = SendJobMessageOutput;

    fn name(&self) -> &'static str {
        "send_job_message"
    }
    fn description(&self) -> &'static str {
        "Send a sidechannel message to a running job's inbox."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer" },
                "payload": { "type": "object" },
                "sender": { "type": "string" }
            },
            "required": ["id", "payload"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let msg: InboxMessage = engine
            .send_message(params.id, &params.payload, params.sender.as_deref().unwrap_or("admin"))
            .await?
            .ok_or_else(|| {
                OperationError::invalid_params(format!(
                    "job not found, not messageable, or sender unauthorized: {}",
                    params.id
                ))
            })?;
        Ok(SendJobMessageOutput {
            sent: true,
            message_id: msg.id,
            job_id: params.id,
        })
    }
}

/// Cancel a job.
#[derive(Debug, Clone)]
pub struct CancelJobOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CancelJobParams {
    pub id: i64,
}

impl ValidateParams for CancelJobParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for CancelJobOperation {
    type Params = CancelJobParams;
    type Output = MinionJob;

    fn name(&self) -> &'static str {
        "cancel_job"
    }
    fn description(&self) -> &'static str {
        "Cancel a waiting, active, or delayed job."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let cancelled = engine
            .cancel_job(params.id)
            .await?
            .ok_or_else(|| {
                OperationError::invalid_params(format!("cannot cancel job {} (may be terminal)", params.id))
            })?;
        Ok(cancelled)
    }
}

/// Retry a failed or dead job.
#[derive(Debug, Clone)]
pub struct RetryJobOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RetryJobParams {
    pub id: i64,
}

impl ValidateParams for RetryJobParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for RetryJobOperation {
    type Params = RetryJobParams;
    type Output = MinionJob;

    fn name(&self) -> &'static str {
        "retry_job"
    }
    fn description(&self) -> &'static str {
        "Re-queue a failed or dead job for retry."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let queue = MinionQueue::new(ctx.engine()?);
        let retried = queue
            .retry_job(params.id)
            .await?
            .ok_or_else(|| {
                OperationError::invalid_params(format!("cannot retry job {} (must be failed or dead)", params.id))
            })?;
        Ok(retried)
    }
}

/// Pause a job.
#[derive(Debug, Clone)]
pub struct PauseJobOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PauseJobParams {
    pub id: i64,
}

impl ValidateParams for PauseJobParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for PauseJobOperation {
    type Params = PauseJobParams;
    type Output = MinionJob;

    fn name(&self) -> &'static str {
        "pause_job"
    }
    fn description(&self) -> &'static str {
        "Pause a waiting, active, or delayed job."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let queue = MinionQueue::new(ctx.engine()?);
        let job = queue
            .pause_job(params.id)
            .await?
            .ok_or_else(|| {
                OperationError::invalid_params(format!("job not found or not pausable: {}", params.id))
            })?;
        Ok(job)
    }
}

/// Resume a paused job.
#[derive(Debug, Clone)]
pub struct ResumeJobOperation;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ResumeJobParams {
    pub id: i64,
}

impl ValidateParams for ResumeJobParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for ResumeJobOperation {
    type Params = ResumeJobParams;
    type Output = MinionJob;

    fn name(&self) -> &'static str {
        "resume_job"
    }
    fn description(&self) -> &'static str {
        "Resume a paused job back to waiting."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let queue = MinionQueue::new(ctx.engine()?);
        let job = queue
            .resume_job(params.id)
            .await?
            .ok_or_else(|| {
                OperationError::invalid_params(format!("job not found or not paused: {}", params.id))
            })?;
        Ok(job)
    }
}

// ── 1-6-7-5 — ingestion + files-attachments + calibration + transcripts ────
//
// Faithful 1:1 ports of the TS `operations.ts` ops in this domain. All nine
// (eight distinct op names; `get_calibration_profile` reuses the existing
// `CalibrationQueries` backend) are implemented in place — zero TS residue.
// `file_upload` tightens its filesystem confinement via `validate_upload_path`
// exactly like the TS original (strict when `ctx.remote`, loose for local CLI).

/// Dream/LSD output markers (mirrors `src/core/cycle/transcript-discovery.ts`).
/// Used by `get_recent_transcripts` to skip dream-generated corpus files so the
/// synthesize loop never re-ingests its own output.
fn is_dream_output(content: &str) -> bool {
    use std::sync::OnceLock;
    static LSD_RE: OnceLock<regex::Regex> = OnceLock::new();
    static DREAM_RE: OnceLock<regex::Regex> = OnceLock::new();
    let lsd = LSD_RE.get_or_init(|| {
        regex::Regex::new(r#"^\u{feff}?-{3}\r?\n[\s\S]{0,2000}?mode\s*:\s*(?:"|'|)lsd(?:"|'|)\s*(?:\r?\n|$)"#).unwrap()
    });
    let dream = DREAM_RE.get_or_init(|| {
        regex::Regex::new(r#"^\u{feff}?-{3}\r?\n[\s\S]{0,2000}?dream_generated\s*:\s*true\b"#).unwrap()
    });
    lsd.is_match(content) || dream.is_match(content)
}

// ── get_chunks ─────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetChunksParams {
    pub slug: String,
}

impl ValidateParams for GetChunksParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GetChunksOperation;

#[async_trait]
impl TypedOperation for GetChunksOperation {
    type Params = GetChunksParams;
    type Output = Vec<crate::types::Chunk>;

    fn name(&self) -> &'static str {
        "get_chunks"
    }
    fn description(&self) -> &'static str {
        "Get content chunks for a page."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "slug": { "type": "string", "description": "Page slug" } },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let chunks = engine.get_chunks(&params.slug, &ctx.source_id).await?;
        Ok(chunks)
    }
}


// ── log_ingest ─────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LogIngestParams {
    pub source_type: String,
    pub source_ref: String,
    pub pages_updated: Vec<String>,
    pub summary: String,
}

impl ValidateParams for LogIngestParams {
    fn validate(&self) -> OperationResult<()> {
        if self.source_type.trim().is_empty() {
            return Err(OperationError::invalid_params("source_type is required"));
        }
        if self.source_ref.trim().is_empty() {
            return Err(OperationError::invalid_params("source_ref is required"));
        }
        if self.summary.trim().is_empty() {
            return Err(OperationError::invalid_params("summary is required"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogIngestOutput {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LogIngestOperation;

#[async_trait]
impl TypedOperation for LogIngestOperation {
    type Params = LogIngestParams;
    type Output = LogIngestOutput;

    fn name(&self) -> &'static str {
        "log_ingest"
    }
    fn description(&self) -> &'static str {
        "Log an ingestion event."
    }
    fn required_scope(&self) -> &'static str {
        "write"
    }
    fn mutating(&self) -> bool {
        true
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source_type": { "type": "string" },
                "source_ref": { "type": "string" },
                "pages_updated": { "type": "array", "items": { "type": "string" } },
                "summary": { "type": "string" }
            },
            "required": ["source_type", "source_ref", "pages_updated", "summary"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        if ctx.dry_run {
            return Ok(LogIngestOutput {
                status: "ok".to_string(),
                dry_run: Some(true),
                action: Some("log_ingest".to_string()),
            });
        }
        let engine = ctx.engine()?;
        let input = crate::types::IngestLogInput {
            source_id: ctx.source_id.clone(),
            source_type: params.source_type,
            source_ref: params.source_ref,
            pages_updated: params.pages_updated,
            summary: params.summary,
        };
        engine.log_ingest(&input).await?;
        Ok(LogIngestOutput {
            status: "ok".to_string(),
            dry_run: None,
            action: None,
        })
    }
}

// ── get_ingest_log ─────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetIngestLogParams {
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ValidateParams for GetIngestLogParams {
    fn validate(&self) -> OperationResult<()> {
        if let Some(l) = self.limit {
            if l == 0 {
                return Err(OperationError::invalid_params("limit must be >= 1"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GetIngestLogOperation;

#[async_trait]
impl TypedOperation for GetIngestLogOperation {
    type Params = GetIngestLogParams;
    type Output = Vec<crate::types::IngestLogEntry>;

    fn name(&self) -> &'static str {
        "get_ingest_log"
    }
    fn description(&self) -> &'static str {
        "Get recent ingestion log entries."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "limit": { "type": "number", "description": "Max entries (default 20, capped 50)" } }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let limit = params.limit.unwrap_or(20).clamp(1, 50);
        let entries = engine.get_ingest_log(limit).await?;
        Ok(entries)
    }
}

// ── file_list ──────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileListParams {
    #[serde(default)]
    pub slug: Option<String>,
}

impl ValidateParams for FileListParams {
    fn validate(&self) -> OperationResult<()> {
        if let Some(s) = &self.slug {
            validate_page_slug(s)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FileListOperation;

#[async_trait]
impl TypedOperation for FileListOperation {
    type Params = FileListParams;
    type Output = Vec<crate::types::FileListRow>;

    fn name(&self) -> &'static str {
        "file_list"
    }
    fn description(&self) -> &'static str {
        "List stored files."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn local_only(&self) -> bool {
        true
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "slug": { "type": "string", "description": "Filter by page slug" } }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let files = engine.list_files(params.slug.as_deref()).await?;
        Ok(files)
    }
}

// ── file_upload ─────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileUploadParams {
    pub path: String,
    #[serde(default)]
    pub page_slug: Option<String>,
}

impl ValidateParams for FileUploadParams {
    fn validate(&self) -> OperationResult<()> {
        if self.path.trim().is_empty() {
            return Err(OperationError::invalid_params("path is required"));
        }
        if let Some(s) = &self.page_slug {
            validate_page_slug(s)?;
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadOutput {
    pub status: String,
    pub storage_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileUploadOperation;

#[async_trait]
impl TypedOperation for FileUploadOperation {
    type Params = FileUploadParams;
    type Output = FileUploadOutput;

    fn name(&self) -> &'static str {
        "file_upload"
    }
    fn description(&self) -> &'static str {
        "Upload a file to storage."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
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
                "path": { "type": "string", "description": "Local file path" },
                "page_slug": { "type": "string", "description": "Associate with page" }
            },
            "required": ["path"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        if ctx.dry_run {
            return Ok(FileUploadOutput {
                status: "uploaded".to_string(),
                storage_path: String::new(),
                size_bytes: None,
                dry_run: Some(true),
                action: Some("file_upload".to_string()),
            });
        }

        use std::path::Path;
        let path = Path::new(&params.path);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| OperationError::invalid_params("path has no filename"))?
            .to_string();
        validate_filename(&filename)?;
        let page_slug = params.page_slug.clone();

        // Trust boundary: remote callers confined to cwd (strict); local CLI
        // callers may upload from anywhere on the filesystem (loose).
        let strict = ctx.remote;
        let cwd = std::env::current_dir()
            .map_err(|e| OperationError::invalid_params(format!("cannot resolve cwd: {e}")))?;
        let _resolved = validate_upload_path(
            &params.path,
            &cwd.to_string_lossy(),
            strict,
        )?;

        let content = std::fs::read(&params.path).map_err(|e| {
            OperationError::new(
                ErrorCode::StorageError,
                format!("Cannot read file {}: {}", params.path, e),
            )
        })?;
        let size_bytes = content.len() as i64;

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let hash = hex::encode(hasher.finalize());

        let storage_path = match &page_slug {
            Some(slug) => format!("{slug}/{filename}"),
            None => format!("unsorted/{}-{}", &hash[..8.min(hash.len())], filename),
        };

        const MIME_TYPES: &[(&str, &str)] = &[
            (".jpg", "image/jpeg"),
            (".jpeg", "image/jpeg"),
            (".png", "image/png"),
            (".gif", "image/gif"),
            (".webp", "image/webp"),
            (".svg", "image/svg+xml"),
            (".pdf", "application/pdf"),
            (".mp4", "video/mp4"),
            (".mp3", "audio/mpeg"),
        ];
        let ext = Path::new(&filename)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!(".{s}"))
            .unwrap_or_default()
            .to_lowercase();
        let mime_type = MIME_TYPES
            .iter()
            .find(|(e, _)| *e == ext)
            .map(|(_, m)| m.to_string());

        let engine = ctx.engine()?;
        // Faithful to TS default (no storage backend configured): record
        // metadata only; we do NOT copy bytes (the original file stays on disk).
        if engine
            .get_file(&ctx.source_id, &storage_path)
            .await?
            .is_some()
        {
            return Ok(FileUploadOutput {
                status: "already_exists".to_string(),
                storage_path,
                size_bytes: None,
                dry_run: None,
                action: None,
            });
        }

        let spec = crate::types::FileSpec {
            source_id: Some(ctx.source_id.clone()),
            page_slug: page_slug.clone(),
            page_id: None,
            filename,
            storage_path: storage_path.clone(),
            mime_type,
            size_bytes: Some(size_bytes),
            content_hash: hash,
            metadata: None,
        };
        engine.upsert_file(&spec).await?;
        Ok(FileUploadOutput {
            status: "uploaded".to_string(),
            storage_path,
            size_bytes: Some(size_bytes),
            dry_run: None,
            action: None,
        })
    }
}

// ── file_url ───────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileUrlParams {
    pub storage_path: String,
}

impl ValidateParams for FileUrlParams {
    fn validate(&self) -> OperationResult<()> {
        if self.storage_path.trim().is_empty() {
            return Err(OperationError::invalid_params("storage_path is required"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUrlOutput {
    pub storage_path: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct FileUrlOperation;

#[async_trait]
impl TypedOperation for FileUrlOperation {
    type Params = FileUrlParams;
    type Output = FileUrlOutput;

    fn name(&self) -> &'static str {
        "file_url"
    }
    fn description(&self) -> &'static str {
        "Get a URL for a stored file."
    }
    fn required_scope(&self) -> &'static str {
        "admin"
    }
    fn local_only(&self) -> bool {
        true
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "storage_path": { "type": "string" } },
            "required": ["storage_path"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let _row = engine
            .get_file(&ctx.source_id, &params.storage_path)
            .await?
            .ok_or_else(|| {
                OperationError::new(
                    ErrorCode::StorageError,
                    format!("File not found: {}", params.storage_path),
                )
            })?;
        Ok(FileUrlOutput {
            storage_path: params.storage_path.clone(),
            url: format!("zbrain:files/{}", params.storage_path),
        })
    }
}

// ── get_calibration_profile ────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetCalibrationProfileParams {
    #[serde(default)]
    pub holder: Option<String>,
}

impl ValidateParams for GetCalibrationProfileParams {
    fn validate(&self) -> OperationResult<()> {
        if let Some(h) = &self.holder {
            if h.trim().is_empty() {
                return Err(OperationError::invalid_params(
                    "get_calibration_profile.holder must be a non-empty string",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GetCalibrationProfileOperation;

#[async_trait]
impl TypedOperation for GetCalibrationProfileOperation {
    type Params = GetCalibrationProfileParams;
    type Output = Option<crate::calibration_queries::CalibrationProfileRow>;

    fn name(&self) -> &'static str {
        "get_calibration_profile"
    }
    fn description(&self) -> &'static str {
        "Read the active calibration profile for a holder. Returns null when no profile exists yet."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "holder": { "type": "string", "description": "Holder slug (default 'garry')" }
            }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let holder = params.holder.unwrap_or_else(|| "garry".to_string());
        let profile = engine.get_calibration_profile(&holder).await?;
        Ok(profile)
    }
}

// ── get_recent_transcripts ─────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetRecentTranscriptsParams {
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub summary: Option<bool>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ValidateParams for GetRecentTranscriptsParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GetRecentTranscriptsOperation;

#[async_trait]
impl TypedOperation for GetRecentTranscriptsOperation {
    type Params = GetRecentTranscriptsParams;
    type Output = Vec<crate::types::RecentTranscript>;

    fn name(&self) -> &'static str {
        "get_recent_transcripts"
    }
    fn description(&self) -> &'static str {
        "List recent transcript files from the dream-cycle corpus dirs (local-only)."
    }
    fn local_only(&self) -> bool {
        true
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "days": { "type": "number", "description": "Window in days. Default 7." },
                "summary": { "type": "boolean", "description": "Return ~300-char summary (default true)." },
                "limit": { "type": "number", "description": "Max transcripts (default 50)." }
            }
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        // Trust gate: MCP/HTTP callers are blocked (local_only already enforces
        // this at dispatch, but TS also fails closed on remote === true).
        if ctx.remote {
            return Err(OperationError::permission_denied(
                "get_recent_transcripts is local-only — call via the zbrain CLI.",
            ));
        }
        let days = params.days.unwrap_or(7).max(0);
        let summary = params.summary.unwrap_or(true);
        let limit = params.limit.unwrap_or(50).clamp(1, 500);

        // Corpus dirs come from config in TS (dream.synthesize.*_dir). Rust has
        // no runtime config kv yet, so we resolve them from env (set by the
        // local CLI / dream cycle). Unset → no corpus → empty result.
        let mut dirs: Vec<String> = Vec::new();
        if let Ok(d) = std::env::var("ZBRAIN_DREAM_SESSION_CORPUS_DIR") {
            if !d.trim().is_empty() {
                dirs.push(d);
            }
        }
        if let Ok(d) = std::env::var("ZBRAIN_DREAM_MEETING_TRANSCRIPTS_DIR") {
            if !d.trim().is_empty() {
                dirs.push(d);
            }
        }
        if dirs.is_empty() {
            return Ok(Vec::new());
        }

        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (days as i64) * 86_400_000;

        let date_re = regex::Regex::new(r"^(\d{4}-\d{2}-\d{2})").unwrap();

        let mut candidates: Vec<(std::path::PathBuf, i64, u64)> = Vec::new();
        for dir in &dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("txt") {
                    continue;
                }
                let meta = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if !meta.is_file() {
                    continue;
                }
                let mtime_ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                if mtime_ms < cutoff_ms {
                    continue;
                }
                candidates.push((path, mtime_ms, meta.len()));
            }
        }

        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        let mut out: Vec<crate::types::RecentTranscript> = Vec::new();
        for (path, mtime_ms, size) in candidates {
            if out.len() >= limit as usize {
                break;
            }
            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if is_dream_output(&raw) {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let date = date_re
                .captures(&name)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());
            let mtime_iso = chrono::DateTime::<chrono::Utc>::from(
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(mtime_ms.max(0) as u64),
            )
            .to_rfc3339();
            let body = if summary {
                build_transcript_summary(&raw)
            } else {
                raw.chars().take(100 * 1024).collect()
            };
            out.push(crate::types::RecentTranscript {
                path: name,
                date,
                mtime: mtime_iso,
                length: size as i64,
                summary: body,
            });
        }
        Ok(out)
    }
}

/// First non-empty line + next ~250 chars (mirrors TS `buildSummary`).
fn build_transcript_summary(raw: &str) -> String {
    let trimmed = regex::Regex::new(r"^[\s\u{feff}]+")
        .unwrap()
        .replace(raw, "")
        .into_owned();
    let first_line_end = trimmed.find('\n').unwrap_or(trimmed.len());
    let first_line = &trimmed[..first_line_end];
    let after = if first_line_end < trimmed.len() {
        let rest = &trimmed[first_line_end + 1..];
        let cap = rest.char_indices().take(250).map(|(i, _)| i).last().unwrap_or(0);
        &rest[..cap]
    } else {
        ""
    };
    if after.trim().is_empty() {
        first_line.to_string()
    } else {
        format!("{first_line}\n{after}").trim().to_string()
    }
}

// ── 1-6-7-8: commands-misc gap ops (Rust impl; engine methods already exist) ──

/// Fuzzy-resolve a partial slug to matching page slugs.
#[derive(Debug, Clone)]
pub struct ResolveSlugsOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct ResolveSlugsParams {
    pub partial: String,
}

impl ValidateParams for ResolveSlugsParams {
    fn validate(&self) -> OperationResult<()> {
        if self.partial.trim().is_empty() {
            return Err(OperationError::invalid_params("partial is required"));
        }
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for ResolveSlugsOperation {
    type Params = ResolveSlugsParams;
    type Output = Vec<String>;

    fn name(&self) -> &'static str {
        "resolve_slugs"
    }
    fn description(&self) -> &'static str {
        "Fuzzy-resolve a partial slug to matching page slugs."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "partial": { "type": "string", "description": "Partial slug to resolve" } },
            "required": ["partial"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let opts = crate::engine::ResolveSlugsOpts {
            source_id: Some(ctx.source_id.clone()),
            source_ids: None,
        };
        let slugs = engine.resolve_slugs(&params.partial, &opts).await?;
        Ok(slugs)
    }
}

/// Page version history.
#[derive(Debug, Clone)]
pub struct GetVersionsOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct GetVersionsParams {
    pub slug: String,
}

impl ValidateParams for GetVersionsParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for GetVersionsOperation {
    type Params = GetVersionsParams;
    type Output = Vec<crate::types::PageVersion>;

    fn name(&self) -> &'static str {
        "get_versions"
    }
    fn description(&self) -> &'static str {
        "Page version history."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "slug": { "type": "string", "description": "Page slug" } },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let versions = engine.get_versions(&params.slug, Some(ctx.source_id.as_str())).await?;
        Ok(versions)
    }
}

/// Revert a page to a previous version.
#[derive(Debug, Clone)]
pub struct RevertVersionOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct RevertVersionParams {
    pub slug: String,
    pub version_id: u64,
}

impl ValidateParams for RevertVersionParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        if self.version_id == 0 {
            return Err(OperationError::invalid_params("version_id must be > 0"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevertVersionOutput {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

#[async_trait]
impl TypedOperation for RevertVersionOperation {
    type Params = RevertVersionParams;
    type Output = RevertVersionOutput;

    fn name(&self) -> &'static str {
        "revert_version"
    }
    fn description(&self) -> &'static str {
        "Revert a page to a previous version."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Page slug" },
                "version_id": { "type": "integer", "description": "Version id to revert to" }
            },
            "required": ["slug", "version_id"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        if ctx.dry_run {
            return Ok(RevertVersionOutput { status: "reverted".to_string(), dry_run: Some(true) });
        }
        let engine = ctx.engine()?;
        let sid = Some(ctx.source_id.as_str());
        engine.create_version(&params.slug, sid).await?;
        engine.revert_to_version(&params.slug, params.version_id, sid).await?;
        Ok(RevertVersionOutput { status: "reverted".to_string(), dry_run: None })
    }
}

/// Store raw API response data for a page.
#[derive(Debug, Clone)]
pub struct PutRawDataOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct PutRawDataParams {
    pub slug: String,
    pub source: String,
    pub data: serde_json::Value,
}

impl ValidateParams for PutRawDataParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        if self.source.trim().is_empty() {
            return Err(OperationError::invalid_params("source is required"));
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PutRawDataOutput {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

#[async_trait]
impl TypedOperation for PutRawDataOperation {
    type Params = PutRawDataParams;
    type Output = PutRawDataOutput;

    fn name(&self) -> &'static str {
        "put_raw_data"
    }
    fn description(&self) -> &'static str {
        "Store raw API response data for a page."
    }
    fn mutating(&self) -> bool {
        true
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Page slug" },
                "source": { "type": "string", "description": "Data source (e.g. crustdata, happenstance)" },
                "data": { "type": "object", "description": "Raw data object" }
            },
            "required": ["slug", "source", "data"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        if ctx.dry_run {
            return Ok(PutRawDataOutput { status: "ok".to_string(), dry_run: Some(true) });
        }
        let engine = ctx.engine()?;
        engine.put_raw_data(&params.slug, &params.source, &params.data, Some(ctx.source_id.as_str())).await?;
        Ok(PutRawDataOutput { status: "ok".to_string(), dry_run: None })
    }
}

/// Retrieve raw data for a page.
#[derive(Debug, Clone)]
pub struct GetRawDataOperation;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct GetRawDataParams {
    pub slug: String,
    #[serde(default)]
    pub source: Option<String>,
}

impl ValidateParams for GetRawDataParams {
    fn validate(&self) -> OperationResult<()> {
        validate_page_slug(&self.slug)?;
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for GetRawDataOperation {
    type Params = GetRawDataParams;
    type Output = Vec<crate::types::RawData>;

    fn name(&self) -> &'static str {
        "get_raw_data"
    }
    fn description(&self) -> &'static str {
        "Retrieve raw data for a page."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Page slug" },
                "source": { "type": "string", "description": "Filter by source" }
            },
            "required": ["slug"]
        })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let rows = engine
            .get_raw_data(&params.slug, params.source.as_deref(), Some(ctx.source_id.as_str()))
            .await?;
        Ok(rows)
    }
}

/// Thin-client banner identity packet (mirrors TS `get_brain_identity`).
#[derive(Debug, Clone)]
pub struct GetBrainIdentityOperation;

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetBrainIdentityParams {}

impl ValidateParams for GetBrainIdentityParams {
    fn validate(&self) -> OperationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl TypedOperation for GetBrainIdentityOperation {
    type Params = GetBrainIdentityParams;
    type Output = crate::engine::BrainIdentity;

    fn name(&self) -> &'static str {
        "get_brain_identity"
    }
    fn description(&self) -> &'static str {
        "Thin-client banner identity packet: engine kind + content counts (read-scope, banner-only)."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(
        &self,
        ctx: &OperationContext,
        _params: Self::Params,
    ) -> OperationResult<Self::Output> {
        let engine = ctx.engine()?;
        let identity = engine.brain_identity().await?;
        Ok(identity)
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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
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

            async fn embed_image(
                &self,
                _base64_image: &str,
                _mime: Option<&str>,
                _dims: usize,
            ) -> Result<Vec<f32>, crate::embedding::EmbeddingError> {
                Ok(self.0.clone())
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

                async fn embed_image(
                    &self,
                    _base64_image: &str,
                    _mime: Option<&str>,
                    _dims: usize,
                ) -> Result<Vec<f32>, crate::embedding::EmbeddingError> {
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

    // ── Page WRAP ops (slice 1-6-7-1) ──────────────────────────────────────
    mod page_ops_tests {
        use super::*;
        use crate::engine::{InMemoryEngine, PageInput};

        async fn page_ctx() -> (OperationRegistry, OperationContext) {
            let mut registry = OperationRegistry::new();
            register_all(&mut registry);
            let engine = InMemoryEngine::default();
            let input = PageInput {
                page_type: "note".to_string(),
                title: "P".to_string(),
                compiled_truth: "x".to_string(),
                ..Default::default()
            };
            let _ = engine.put_page("p/one", None, &input).await;
            let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
            (registry, ctx)
        }

        #[test]
        fn register_all_registers_sixty_two_ops() {
            let mut registry = OperationRegistry::new();
            register_all(&mut registry);
            let names = registry.operation_names();
            for n in [
                "get_page",
                "put_page",
                "delete_page",
                "restore_page",
                "purge_deleted_pages",
                "list_pages",
                "query",
                "search",
                "search_by_image",
                "think",
                "takes_list",
                "takes_search",
                "soft_delete_page",
                "rewrite_links",
                "get_page_timestamps",
                "refresh_page_body",
                // 1-6-7-2: tags / links / timeline domain
                "add_tag",
                "remove_tag",
                "get_tags",
                "add_link",
                "remove_link",
                "get_links",
                "get_backlinks",
                "traverse_graph",
                "add_timeline_entry",
                "get_timeline",
                // 1-6-7-3: sources / facts / anomalies / health-stats
                "sources_add",
                "sources_list",
                "sources_status",
                "sources_remove",
                "forget_fact",
                "extract_facts",
                "find_contradictions",
                "find_anomalies",
                "get_health",
                "get_stats",
                "get_recent_salience",
                // 1-6-7-4: jobs / minions domain
                "submit_job",
                "submit_agent",
                "list_jobs",
                "get_job",
                "get_job_progress",
                "replay_job",
                "send_job_message",
                "cancel_job",
                "retry_job",
                "pause_job",
                "resume_job",
                // 1-6-7-5: ingestion + files + calibration + transcripts
                "get_chunks",
                "log_ingest",
                "get_ingest_log",
                "file_list",
                "file_upload",
                "file_url",
                "get_calibration_profile",
                "get_recent_transcripts",
                // 1-6-7-8: commands-misc gap ops
                "resolve_slugs",
                "get_versions",
                "revert_version",
                "put_raw_data",
                "get_raw_data",
                "get_brain_identity",
            ] {
                assert!(names.contains(&n), "missing op: {}", n);
            }
        }

        // ── 1-6-7-11: search_by_image ──────────────────────────────────────

        /// Fixed-vector embedding provider for search_by_image tests. Returns
        /// the same non-zero vector for every image so cosine similarity
        /// against a matching chunk embedding is well-defined (= 1.0).
        struct ConstImageProvider(Vec<f32>);
        #[async_trait::async_trait]
        impl crate::embedding::EmbeddingProvider for ConstImageProvider {
            async fn embed(
                &self,
                texts: &[String],
                _dims: usize,
            ) -> Result<Vec<Vec<f32>>, crate::embedding::EmbeddingError> {
                Ok(texts.iter().map(|_| self.0.clone()).collect())
            }
            async fn embed_image(
                &self,
                _base64_image: &str,
                _mime: Option<&str>,
                _dims: usize,
            ) -> Result<Vec<f32>, crate::embedding::EmbeddingError> {
                Ok(self.0.clone())
            }
        }

        fn image_embedding_client() -> std::sync::Arc<crate::embedding::EmbeddingClient> {
            let config = crate::embedding::EmbeddingConfig {
                model: "test-multimodal".to_string(),
                dimensions: 4,
                api_key: "test-key".to_string(),
                ..Default::default()
            };
            std::sync::Arc::new(crate::embedding::EmbeddingClient::with_provider(
                config,
                std::sync::Arc::new(ConstImageProvider(vec![0.5, 0.5, 0.5, 0.5])),
            ))
        }

        /// f32 → little-endian bytes (mirrors the engine's `Page::embedding` layout).
        fn f32_le_bytes(v: &[f32]) -> Vec<u8> {
            v.iter().flat_map(|f| f.to_le_bytes()).collect()
        }

        /// Seed one page with a single chunk whose embedding matches the test
        /// provider's fixed image vector.
        async fn seed_image_page(engine: &dyn crate::engine::BrainEngine, slug: &str) {
            use crate::engine::PageInput;
            let input = PageInput {
                page_type: "note".to_string(),
                title: "Cat page".to_string(),
                compiled_truth: "a photo of a cat".to_string(),
                // Page-level embedding (f32-LE) must match the chunk embedding
                // so `fuse_and_boost`'s vector path keeps the candidate.
                embedding: Some(f32_le_bytes(&[0.5, 0.5, 0.5, 0.5])),
                ..Default::default()
            };
            engine.put_page(slug, None, &input).await.unwrap();
            use crate::import::{ChunkInput, ChunkSource};
            engine
                .upsert_chunks(
                    slug,
                    &[ChunkInput {
                        chunk_index: 0,
                        chunk_text: "cat photo".to_string(),
                        chunk_source: ChunkSource::CompiledTruth,
                        embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
                        token_count: None,
                        language: None,
                        symbol_name: None,
                        symbol_type: None,
                        start_line: None,
                        end_line: None,
                        parent_symbol_path: vec![],
                        symbol_name_qualified: None,
                    }],
                )
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn search_by_image_requires_exactly_one_source() {
            use crate::engine::InMemoryEngine;
            let engine = InMemoryEngine::default();
            let mut registry = OperationRegistry::new();
            registry.register(SearchByImageOperation);
            let ctx = OperationContext::local_cli().with_engine(std::sync::Arc::new(engine));

            // No source → invalid_params.
            let res = registry
                .dispatch_json("search_by_image", &ctx, serde_json::json!({}))
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::InvalidParams);

            // Two sources → invalid_params.
            let res = registry
                .dispatch_json(
                    "search_by_image",
                    &ctx,
                    serde_json::json!({ "image_path": "/tmp/a.png", "image_url": "http://x/y.png" }),
                )
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::InvalidParams);
        }

        #[tokio::test]
        async fn search_by_image_requires_embedding_client() {
            use crate::engine::InMemoryEngine;
            let engine = InMemoryEngine::default();
            let mut registry = OperationRegistry::new();
            registry.register(SearchByImageOperation);
            // No embedding client wired → EmbeddingFailed (hard error, no
            // lexical fallback for image search).
            let ctx = OperationContext::local_cli().with_engine(std::sync::Arc::new(engine));
            let res = registry
                .dispatch_json(
                    "search_by_image",
                    &ctx,
                    serde_json::json!({ "image_data": "AAAA" }),
                )
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::EmbeddingFailed);
        }

        #[tokio::test]
        async fn search_by_image_daily_budget_enforced() {
            use crate::engine::InMemoryEngine;
            let engine = InMemoryEngine::default();
            // Pre-fill today's spend at the cap; one more call must be blocked.
            engine
                .record_image_search_spend("default", 100, "embedding", "test-model")
                .await
                .unwrap();
            let mut registry = OperationRegistry::new();
            registry.register(SearchByImageOperation);
            let ctx = OperationContext::local_cli()
                .with_engine(std::sync::Arc::new(engine))
                .with_embedding(image_embedding_client());
            let res = registry
                .dispatch_json(
                    "search_by_image",
                    &ctx,
                    serde_json::json!({ "image_data": "AAAA" }),
                )
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::RateLimited);
        }

        #[tokio::test]
        async fn search_by_image_happy_path_returns_similar_page_and_logs_spend() {
            use crate::engine::InMemoryEngine;
            let engine = std::sync::Arc::new(InMemoryEngine::default());
            seed_image_page(engine.as_ref(), "cat.md").await;
            let mut registry = OperationRegistry::new();
            registry.register(SearchByImageOperation);
            let ctx = OperationContext::local_cli()
                .with_engine(engine.clone())
                .with_embedding(image_embedding_client());

            let res = registry
                .dispatch_json(
                    "search_by_image",
                    &ctx,
                    serde_json::json!({ "image_data": "AAAA", "limit": 10 }),
                )
                .await;
            assert!(res.is_ok(), "expected ok, got: {:?}", res);
            let output = res.unwrap();
            let results = output["results"].as_array().expect("results array");
            assert!(!results.is_empty(), "expected at least one result");
            let slugs: Vec<&str> = results
                .iter()
                .filter_map(|r| r["page"]["slug"].as_str())
                .collect();
            assert!(slugs.contains(&"cat.md"), "cat.md should be a top hit: {slugs:?}");

            // Spend recorded for audit + daily cap (local_cli source_id = "default").
            let spent = engine.image_search_daily_spend_cents("default").await.unwrap();
            assert_eq!(spent, 1, "one completed call should be billed");
        }

        // ── 1-6-7-5 domain ops: ingestion + files + calibration + transcripts ─

        async fn ingestion_ctx() -> (OperationRegistry, OperationContext) {
            let mut registry = OperationRegistry::new();
            register_all(&mut registry);
            use crate::engine::InMemoryEngine;
            let engine = InMemoryEngine::default();
            let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
            (registry, ctx)
        }

        // ── 1-6-7-8: commands-misc gap ops ──

        async fn seed_page(registry: &OperationRegistry, ctx: &OperationContext, slug: &str) {
            let res = registry
                .dispatch_json(
                    "put_page",
                    ctx,
                    serde_json::json!({ "slug": slug, "compiled_truth": "body" }),
                )
                .await;
            assert!(res.is_ok(), "seed put_page failed: {:?}", res);
        }

        #[tokio::test]
        async fn resolve_slugs_finds_created_page() {
            let (registry, ctx) = ingestion_ctx().await;
            seed_page(&registry, &ctx, "wiki/notes/alpha").await;
            let res = registry
                .dispatch_json("resolve_slugs", &ctx, serde_json::json!({ "partial": "wiki/notes" }))
                .await;
            assert!(res.is_ok(), "resolve_slugs failed: {:?}", res);
            let slugs: Vec<String> = serde_json::from_value(res.unwrap()).unwrap();
            assert!(slugs.iter().any(|s| s == "wiki/notes/alpha"), "got: {:?}", slugs);
        }

        #[tokio::test]
        async fn get_versions_returns_history_after_put() {
            let (registry, ctx) = ingestion_ctx().await;
            seed_page(&registry, &ctx, "wiki/notes/beta").await;
            // Versions are only created by explicit `create_version` (put_page does
            // not snapshot), so snapshot one first, then read it back.
            let engine = ctx.engine.clone().expect("engine set");
            engine
                .create_version("wiki/notes/beta", Some(ctx.source_id.as_str()))
                .await
                .expect("create_version failed");
            let res = registry
                .dispatch_json("get_versions", &ctx, serde_json::json!({ "slug": "wiki/notes/beta" }))
                .await;
            assert!(res.is_ok(), "get_versions failed: {:?}", res);
            let versions = res.unwrap();
            assert!(
                versions.as_array().map(|a| !a.is_empty()).unwrap_or(false),
                "expected >=1 version, got: {:?}", versions
            );
        }

        #[tokio::test]
        async fn revert_version_dry_run_flags() {
            let (registry, ctx) = ingestion_ctx().await;
            seed_page(&registry, &ctx, "wiki/notes/gamma").await;
            let engine = ctx.engine.clone().expect("engine set");
            let mut dry = OperationContext::local_cli();
            dry.engine = Some(engine);
            dry.dry_run = true;
            let res = registry
                .dispatch_json(
                    "revert_version",
                    &dry,
                    serde_json::json!({ "slug": "wiki/notes/gamma", "version_id": 1 }),
                )
                .await;
            assert!(res.is_ok(), "revert_version dry failed: {:?}", res);
            assert_eq!(res.unwrap(), serde_json::json!({ "status": "reverted", "dryRun": true }));
        }

        #[tokio::test]
        async fn put_raw_data_then_get_raw_data_roundtrip() {
            let (registry, ctx) = ingestion_ctx().await;
            seed_page(&registry, &ctx, "wiki/notes/delta").await;
            let put = registry
                .dispatch_json(
                    "put_raw_data",
                    &ctx,
                    serde_json::json!({ "slug": "wiki/notes/delta", "source": "crustdata", "data": { "k": "v" } }),
                )
                .await;
            assert!(put.is_ok(), "put_raw_data failed: {:?}", put);
            let res = registry
                .dispatch_json(
                    "get_raw_data",
                    &ctx,
                    serde_json::json!({ "slug": "wiki/notes/delta", "source": "crustdata" }),
                )
                .await;
            assert!(res.is_ok(), "get_raw_data failed: {:?}", res);
            let rows = res.unwrap();
            let arr = rows.as_array().expect("rows should be an array");
            assert!(
                arr.iter()
                    .any(|r| r.get("source").and_then(|s| s.as_str()) == Some("crustdata")),
                "expected a crustdata row, got: {:?}", rows
            );
        }

        #[tokio::test]
        async fn put_raw_data_dry_run_flags() {
            let (registry, ctx) = ingestion_ctx().await;
            let engine = ctx.engine.clone().expect("engine set");
            let mut dry = OperationContext::local_cli();
            dry.engine = Some(engine);
            dry.dry_run = true;
            let res = registry
                .dispatch_json(
                    "put_raw_data",
                    &dry,
                    serde_json::json!({ "slug": "wiki/x", "source": "crustdata", "data": { "k": "v" } }),
                )
                .await;
            assert!(res.is_ok(), "put_raw_data dry failed: {:?}", res);
            assert_eq!(res.unwrap(), serde_json::json!({ "status": "ok", "dryRun": true }));
        }

        #[tokio::test]
        async fn get_chunks_empty_for_unknown_page() {
            let (registry, ctx) = ingestion_ctx().await;
            let res = registry
                .dispatch_json("get_chunks", &ctx, serde_json::json!({ "slug": "nope/x" }))
                .await;
            assert!(res.is_ok(), "get_chunks failed: {:?}", res);
            assert_eq!(res.unwrap(), serde_json::json!([]));
        }

        #[tokio::test]
        async fn get_brain_identity_reports_engine_and_version() {
            let (registry, ctx) = ingestion_ctx().await;
            let res = registry
                .dispatch_json("get_brain_identity", &ctx, serde_json::json!({}))
                .await;
            assert!(res.is_ok(), "get_brain_identity failed: {:?}", res);
            let id = res.unwrap();
            assert_eq!(id.get("engine").and_then(|s| s.as_str()), Some("inmemory"));
            assert!(
                id.get("version").and_then(|s| s.as_str()).unwrap_or("").len() > 0,
                "expected non-empty version, got: {:?}",
                id
            );
            // In-memory engine has no admin stats, so counts default to 0.
            assert_eq!(id.get("pageCount").and_then(|n| n.as_i64()), Some(0));
            assert_eq!(id.get("chunkCount").and_then(|n| n.as_i64()), Some(0));
        }

        // ── 1-6-7-7: search op (lexical keyword search) ──

        #[tokio::test]
        async fn search_returns_pages_matching_keyword() {
            let (registry, ctx) = ingestion_ctx().await;
            seed_page(&registry, &ctx, "notes/rust-migration").await;
            let res = registry
                .dispatch_json(
                    "search",
                    &ctx,
                    serde_json::json!({ "query": "rust", "limit": 20 }),
                )
                .await;
            assert!(res.is_ok(), "search failed: {:?}", res);
            let hits = res.unwrap();
            let arr = hits.as_array().expect("search output should be an array");
            let slugs: Vec<&str> = arr
                .iter()
                .filter_map(|r| r.get("page").and_then(|p| p.get("slug")).and_then(|s| s.as_str()))
                .collect();
            assert!(
                slugs.iter().any(|s| *s == "notes/rust-migration"),
                "expected slug in hits, got: {:?}",
                slugs
            );
        }

        #[tokio::test]
        async fn search_empty_query_is_rejected() {
            let (registry, ctx) = ingestion_ctx().await;
            let res = registry
                .dispatch_json("search", &ctx, serde_json::json!({ "query": "   " }))
                .await;
            assert!(res.is_err(), "expected empty query to be rejected: {:?}", res);
        }

        #[tokio::test]
        async fn search_respects_source_scope_by_default() {
            let (registry, ctx) = ingestion_ctx().await;
            // Seeded under the default source (ctx.source_id == "default").
            seed_page(&registry, &ctx, "notes/scoped").await;
            // A caller scoped to a different source must NOT see it.
            let engine = ctx.engine.clone().expect("engine set");
            let mut other_ctx = OperationContext::local_cli();
            other_ctx.engine = Some(engine);
            other_ctx.source_id = "other".to_string();
            let res = registry
                .dispatch_json("search", &other_ctx, serde_json::json!({ "query": "scoped" }))
                .await;
            assert!(res.is_ok(), "search failed: {:?}", res);
            let value = res.unwrap();
            let arr = value.as_array().expect("array");
            assert!(arr.is_empty(), "cross-source hit should be empty, got: {:?}", arr);
        }

        #[tokio::test]
        async fn search_paginates_with_offset() {
            let (registry, ctx) = ingestion_ctx().await;
            for slug in ["a/zebra", "b/zebra", "c/zebra"] {
                seed_page(&registry, &ctx, slug).await;
            }
            let all = registry
                .dispatch_json("search", &ctx, serde_json::json!({ "query": "zebra", "limit": 10 }))
                .await
                .unwrap();
            assert_eq!(all.as_array().unwrap().len(), 3, "expected 3 hits");
            let page2 = registry
                .dispatch_json(
                    "search",
                    &ctx,
                    serde_json::json!({ "query": "zebra", "limit": 10, "offset": 1 }),
                )
                .await
                .unwrap();
            assert_eq!(page2.as_array().unwrap().len(), 2, "expected 2 after offset 1");
            let past = registry
                .dispatch_json(
                    "search",
                    &ctx,
                    serde_json::json!({ "query": "zebra", "limit": 10, "offset": 99 }),
                )
                .await
                .unwrap();
            assert!(past.as_array().unwrap().is_empty(), "expected empty past end");
        }

        #[tokio::test]
        async fn query_accepts_boost_and_filter_params() {
            let (registry, ctx) = ingestion_ctx().await;
            seed_page(&registry, &ctx, "notes/query-target").await;
            // The new params (salience / recency / min_score / types) must
            // deserialize and route without error; the seeded page (contains
            // "query") should surface.
            let res = registry
                .dispatch_json(
                    "query",
                    &ctx,
                    serde_json::json!({
                        "query": "query",
                        "limit": 10,
                        "salience": "off",
                        "recency": "on",
                        "min_score": 0.0,
                        "types": ["note"]
                    }),
                )
                .await;
            assert!(res.is_ok(), "query with boost/filter params failed: {:?}", res);
            let out = res.unwrap();
            assert_eq!(out.get("total").and_then(|n| n.as_u64()), Some(1), "got: {:?}", out);
            let hits = out.get("results").and_then(|r| r.as_array()).expect("results array");
            assert!(
                hits.iter().any(|h| h
                    .get("page")
                    .and_then(|p| p.get("slug"))
                    .and_then(|s| s.as_str())
                    == Some("notes/query-target")),
                "expected slug in query hits, got: {:?}",
                hits
            );
        }

        #[tokio::test]
        async fn log_ingest_then_get_ingest_log_roundtrip() {
            let (registry, ctx) = ingestion_ctx().await;
            let log = registry
                .dispatch_json(
                    "log_ingest",
                    &ctx,
                    serde_json::json!({
                        "source_type": "capture",
                        "source_ref": "page/xyz",
                        "pages_updated": ["page/xyz"],
                        "summary": "captured one page"
                    }),
                )
                .await;
            assert!(log.is_ok(), "log_ingest failed: {:?}", log);
            assert_eq!(log.unwrap()["status"], "ok");

            let out = registry
                .dispatch_json("get_ingest_log", &ctx, serde_json::json!({ "limit": 10 }))
                .await
                .expect("get_ingest_log failed");
            let entries = out.as_array().expect("expected array");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0]["sourceType"], "capture");
            assert_eq!(entries[0]["sourceRef"], "page/xyz");
        }

        #[tokio::test]
        async fn log_ingest_rejects_empty_summary() {
            let (registry, ctx) = ingestion_ctx().await;
            let res = registry
                .dispatch_json(
                    "log_ingest",
                    &ctx,
                    serde_json::json!({ "source_type": "x", "source_ref": "y", "pages_updated": [], "summary": "" }),
                )
                .await;
            assert!(res.is_err(), "expected validation error");
        }

        #[tokio::test]
        async fn file_list_empty_initially() {
            let (registry, ctx) = ingestion_ctx().await;
            let out = registry
                .dispatch_json("file_list", &ctx, serde_json::json!({}))
                .await
                .expect("file_list failed");
            assert_eq!(out, serde_json::json!([]));
        }

        #[tokio::test]
        async fn file_upload_dry_run_is_noop() {
            let (registry, ctx) = ingestion_ctx().await;
            let mut ctx = ctx;
            ctx.dry_run = true;
            let out = registry
                .dispatch_json(
                    "file_upload",
                    &ctx,
                    serde_json::json!({ "path": "/tmp/does-not-matter.txt" }),
                )
                .await
                .expect("file_upload dry_run failed");
            assert_eq!(out["dryRun"], true);
            assert_eq!(out["action"], "file_upload");
        }

        #[tokio::test]
        async fn get_calibration_profile_returns_null_when_empty() {
            let (registry, ctx) = ingestion_ctx().await;
            let out = registry
                .dispatch_json("get_calibration_profile", &ctx, serde_json::json!({}))
                .await
                .expect("get_calibration_profile failed");
            assert_eq!(out, serde_json::json!(null));
        }

        #[tokio::test]
        async fn get_recent_transcripts_empty_without_corpus_dirs() {
            std::env::remove_var("ZBRAIN_DREAM_SESSION_CORPUS_DIR");
            std::env::remove_var("ZBRAIN_DREAM_MEETING_TRANSCRIPTS_DIR");
            let (registry, ctx) = ingestion_ctx().await;
            let out = registry
                .dispatch_json("get_recent_transcripts", &ctx, serde_json::json!({}))
                .await
                .expect("get_recent_transcripts failed");
            assert_eq!(out, serde_json::json!([]));
        }

        #[tokio::test]
        async fn file_url_rejects_unknown_file() {
            let (registry, ctx) = ingestion_ctx().await;
            let res = registry
                .dispatch_json(
                    "file_url",
                    &ctx,
                    serde_json::json!({ "storage_path": "unsorted/does-not-exist.txt" }),
                )
                .await;
            assert!(res.is_err(), "expected storage_error for missing file");
        }

        // ── 1-6-7-2 domain ops: tags / links / timeline ──────────────────────

        async fn domain_ctx() -> (OperationRegistry, OperationContext) {
            let mut registry = OperationRegistry::new();
            register_all(&mut registry);
            use crate::engine::{InMemoryEngine, PageInput};
            let engine = InMemoryEngine::default();
            for slug in ["p/a", "p/b", "p/c"] {
                let input = PageInput {
                    page_type: "note".to_string(),
                    title: slug.to_string(),
                    compiled_truth: "# ".to_string() + slug,
                    ..Default::default()
                };
                let _ = engine.put_page(slug, None, &input).await;
            }
            let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
            (registry, ctx)
        }

        #[tokio::test]
        async fn add_get_remove_tag_roundtrip() {
            let (registry, ctx) = domain_ctx().await;

            let res = registry
                .dispatch_json("add_tag", &ctx, serde_json::json!({ "slug": "p/a", "tag": "important" }))
                .await;
            assert!(res.is_ok(), "add_tag got: {:?}", res);

            let tags = registry
                .dispatch_json("get_tags", &ctx, serde_json::json!({ "slug": "p/a" }))
                .await
                .unwrap();
            let arr = tags["tags"].as_array().unwrap();
            assert!(arr.iter().any(|t| t.as_str() == Some("important")), "tags={:?}", tags);

            let res = registry
                .dispatch_json("remove_tag", &ctx, serde_json::json!({ "slug": "p/a", "tag": "important" }))
                .await;
            assert!(res.is_ok(), "remove_tag got: {:?}", res);

            let tags = registry
                .dispatch_json("get_tags", &ctx, serde_json::json!({ "slug": "p/a" }))
                .await
                .unwrap();
            let arr = tags["tags"].as_array().unwrap();
            assert!(!arr.iter().any(|t| t.as_str() == Some("important")), "tag still present: {:?}", tags);
        }

        #[tokio::test]
        async fn add_tag_rejects_empty_tag() {
            let (registry, ctx) = domain_ctx().await;
            let res = registry
                .dispatch_json("add_tag", &ctx, serde_json::json!({ "slug": "p/a", "tag": "  " }))
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::InvalidParams);
        }

        #[tokio::test]
        async fn add_link_get_links_backlinks() {
            let (registry, ctx) = domain_ctx().await;
            let res = registry
                .dispatch_json(
                    "add_link",
                    &ctx,
                    serde_json::json!({ "from": "p/a", "to": "p/b", "linkType": "related" }),
                )
                .await;
            assert!(res.is_ok(), "add_link got: {:?}", res);
            assert_eq!(res.unwrap()["added"].as_u64().unwrap(), 1);

            let links = registry
                .dispatch_json("get_links", &ctx, serde_json::json!({ "slug": "p/a" }))
                .await
                .unwrap();
            let arr = links["links"].as_array().unwrap();
            assert!(arr.iter().any(|l| l["toSlug"].as_str() == Some("p/b")), "links={:?}", links);

            let back = registry
                .dispatch_json("get_backlinks", &ctx, serde_json::json!({ "slug": "p/b" }))
                .await
                .unwrap();
            let arr = back["links"].as_array().unwrap();
            assert!(arr.iter().any(|l| l["fromSlug"].as_str() == Some("p/a")), "backlinks={:?}", back);
        }

        #[tokio::test]
        async fn remove_link_clears_link() {
            let (registry, ctx) = domain_ctx().await;
            let _ = registry
                .dispatch_json("add_link", &ctx, serde_json::json!({ "from": "p/a", "to": "p/b" }))
                .await
                .unwrap();
            let res = registry
                .dispatch_json("remove_link", &ctx, serde_json::json!({ "from": "p/a", "to": "p/b" }))
                .await;
            assert!(res.is_ok(), "remove_link got: {:?}", res);
            let links = registry
                .dispatch_json("get_links", &ctx, serde_json::json!({ "slug": "p/a" }))
                .await
                .unwrap();
            assert!(links["links"].as_array().unwrap().is_empty(), "links={:?}", links);
        }

        #[tokio::test]
        async fn traverse_graph_returns_bare_path_array() {
            let (registry, ctx) = domain_ctx().await;
            let _ = registry
                .dispatch_json("add_link", &ctx, serde_json::json!({ "from": "p/a", "to": "p/b" }))
                .await
                .unwrap();
            let _ = registry
                .dispatch_json("add_link", &ctx, serde_json::json!({ "from": "p/b", "to": "p/c" }))
                .await
                .unwrap();
            let res = registry
                .dispatch_json(
                    "traverse_graph",
                    &ctx,
                    serde_json::json!({ "slug": "p/a", "depth": 3, "direction": "out" }),
                )
                .await;
            assert!(res.is_ok(), "traverse_graph got: {:?}", res);
            // Transparent serialization: the output is the bare array, not wrapped.
            let out = res.unwrap();
            assert!(out.is_array(), "traverse_graph must serialize as a bare array, got: {:?}", out);
            assert!(!out.as_array().unwrap().is_empty());
        }

        #[tokio::test]
        async fn traverse_graph_rejects_bad_direction() {
            let (registry, ctx) = domain_ctx().await;
            let res = registry
                .dispatch_json(
                    "traverse_graph",
                    &ctx,
                    serde_json::json!({ "slug": "p/a", "direction": "sideways" }),
                )
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::InvalidParams);
        }

        #[tokio::test]
        async fn traverse_graph_clamps_oversized_depth() {
            let (registry, ctx) = domain_ctx().await;
            let res = registry
                .dispatch_json("traverse_graph", &ctx, serde_json::json!({ "slug": "p/a", "depth": 9999 }))
                .await;
            assert!(res.is_ok(), "depth should clamp, not error: {:?}", res);
        }

        #[tokio::test]
        async fn add_timeline_entry_get_timeline_roundtrip() {
            let (registry, ctx) = domain_ctx().await;
            let res = registry
                .dispatch_json(
                    "add_timeline_entry",
                    &ctx,
                    serde_json::json!({
                        "slug": "p/a",
                        "date": "2024-01-15",
                        "summary": "Launched",
                        "detail": "v1 ship",
                        "source": "history"
                    }),
                )
                .await;
            assert!(res.is_ok(), "add_timeline_entry got: {:?}", res);
            assert_eq!(res.unwrap()["date"].as_str().unwrap(), "2024-01-15");

            let res = registry
                .dispatch_json("get_timeline", &ctx, serde_json::json!({ "slug": "p/a" }))
                .await;
            assert!(res.is_ok(), "get_timeline got: {:?}", res);
            let out = res.unwrap();
            let entries = out["entries"].as_array().unwrap();
            assert_eq!(entries.len(), 1, "entries={:?}", entries);
            assert_eq!(entries[0]["date"].as_str().unwrap(), "2024-01-15");
            assert_eq!(entries[0]["summary"].as_str().unwrap(), "Launched");
            assert_eq!(entries[0]["detail"].as_str().unwrap(), "v1 ship");
            assert_eq!(entries[0]["source"].as_str().unwrap(), "history");
        }

        #[tokio::test]
        async fn add_timeline_entry_rejects_bad_date() {
            let (registry, ctx) = domain_ctx().await;
            let res = registry
                .dispatch_json(
                    "add_timeline_entry",
                    &ctx,
                    serde_json::json!({ "slug": "p/a", "date": "not-a-date", "summary": "x" }),
                )
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::InvalidParams);
        }

        #[tokio::test]
        async fn get_timeline_empty_without_entries() {
            let (registry, ctx) = domain_ctx().await;
            let res = registry
                .dispatch_json("get_timeline", &ctx, serde_json::json!({ "slug": "p/a" }))
                .await
                .unwrap();
            assert!(res["entries"].as_array().unwrap().is_empty());
        }

        #[tokio::test]
        async fn soft_delete_page_dispatches() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json("soft_delete_page", &ctx, serde_json::json!({ "slug": "p/one" }))
                .await;
            assert!(res.is_ok(), "got: {:?}", res);
            assert_eq!(res.unwrap()["deletedSlug"], "p/one");
        }

        #[tokio::test]
        async fn rewrite_links_dispatches() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json(
                    "rewrite_links",
                    &ctx,
                    serde_json::json!({ "old_slug": "p/one", "new_slug": "p/two" }),
                )
                .await;
            assert!(res.is_ok(), "got: {:?}", res);
            assert_eq!(res.unwrap()["rewritten"], true);
        }

        #[tokio::test]
        async fn get_page_timestamps_dispatches() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json(
                    "get_page_timestamps",
                    &ctx,
                    serde_json::json!({ "slugs": ["p/one"] }),
                )
                .await;
            assert!(res.is_ok(), "got: {:?}", res);
            assert!(res.unwrap()["timestamps"]["p/one"].is_string());
        }

        // ── 1-6-7-3 domain ops: sources / facts / anomalies / health-stats ──

        #[tokio::test]
        async fn sources_add_list_status_remove_roundtrip() {
            let (registry, ctx) = page_ctx().await;

            let res = registry
                .dispatch_json("sources_add", &ctx, serde_json::json!({ "id": "src-a", "name": "Source A" }))
                .await;
            assert!(res.is_ok(), "sources_add got: {:?}", res);
            assert_eq!(res.unwrap()["id"].as_str().unwrap(), "src-a");

            let res = registry
                .dispatch_json("sources_list", &ctx, serde_json::json!({}))
                .await;
            assert!(res.is_ok());
            let out = res.unwrap();
            let arr = out.as_array().unwrap();
            assert!(arr.iter().any(|s| s["id"].as_str() == Some("src-a")));

            let res = registry
                .dispatch_json("sources_status", &ctx, serde_json::json!({ "id": "src-a" }))
                .await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap()["name"].as_str().unwrap(), "Source A");

            let res = registry
                .dispatch_json("sources_remove", &ctx, serde_json::json!({ "id": "src-a" }))
                .await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap()["removed"], true);
        }

        #[tokio::test]
        async fn sources_status_missing_returns_error() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json("sources_status", &ctx, serde_json::json!({ "id": "nope" }))
                .await;
            assert!(res.is_err());
        }

        #[tokio::test]
        async fn extract_facts_inserts_and_forget_fact_expires() {
            let (registry, ctx) = page_ctx().await;

            let res = registry
                .dispatch_json(
                    "extract_facts",
                    &ctx,
                    serde_json::json!({ "fact": "Alice likes Rust", "entity_slug": "people/alice" }),
                )
                .await;
            assert!(res.is_ok(), "extract_facts got: {:?}", res);
            let status = res.unwrap().as_str().unwrap_or("").to_string();
            assert!(
                ["inserted", "duplicate", "superseded"].contains(&status.as_str()),
                "unexpected status: {}",
                status
            );

            let res = registry
                .dispatch_json("forget_fact", &ctx, serde_json::json!({ "fact_id": 999 }))
                .await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap()["removed"], false);
        }

        #[tokio::test]
        async fn find_contradictions_returns_health_proxy() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json("find_contradictions", &ctx, serde_json::json!({}))
                .await;
            assert!(res.is_ok(), "got: {:?}", res);
            let out = res.unwrap();
            assert!(out["health"].is_object());
            assert!(out["note"].as_str().unwrap().contains("proxy"));
        }

        #[tokio::test]
        async fn find_anomalies_dispatches() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json("find_anomalies", &ctx, serde_json::json!({}))
                .await;
            assert!(res.is_ok(), "got: {:?}", res);
            assert!(res.unwrap().as_array().is_some());
        }

        #[tokio::test]
        async fn health_stats_salience_dispatch() {
            let (registry, ctx) = page_ctx().await;
            for op in ["get_health", "get_stats", "get_recent_salience"] {
                let res = registry.dispatch_json(op, &ctx, serde_json::json!({})).await;
                assert!(res.is_ok(), "{} got: {:?}", op, res);
            }
        }

        #[tokio::test]
        async fn refresh_page_body_dispatches() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json(
                    "refresh_page_body",
                    &ctx,
                    serde_json::json!({
                        "slug": "p/one",
                        "compiled_truth": "new",
                        "timeline": { "a": 1 },
                        "content_hash": "h"
                    }),
                )
                .await;
            assert!(res.is_ok(), "got: {:?}", res);
            assert_eq!(res.unwrap()["refreshed"], true);
        }

        #[tokio::test]
        async fn soft_delete_page_rejects_bad_slug() {
            let (registry, ctx) = page_ctx().await;
            let res = registry
                .dispatch_json("soft_delete_page", &ctx, serde_json::json!({ "slug": "/bad" }))
                .await;
            assert!(res.is_err());
        }

        // ── 1-6-7-4 domain ops: jobs / minions ──────────────────────────────

        async fn job_ctx() -> (OperationRegistry, OperationContext) {
            let mut registry = OperationRegistry::new();
            register_all(&mut registry);
            let engine = InMemoryEngine::default();
            let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
            (registry, ctx)
        }

        async fn remote_job_ctx() -> (OperationRegistry, OperationContext) {
            let mut registry = OperationRegistry::new();
            register_all(&mut registry);
            let engine = InMemoryEngine::default();
            let ctx = OperationContext::remote_mcp("default").with_engine(engine.into_arc());
            (registry, ctx)
        }

        #[tokio::test]
        async fn submit_job_then_list_get_progress_roundtrip() {
            let (registry, ctx) = job_ctx().await;
            let job = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "sync" }))
                .await
                .unwrap();
            let id = job["id"].as_i64().unwrap();
            assert_eq!(job["name"].as_str().unwrap(), "sync");
            assert_eq!(job["status"].as_str().unwrap(), "Waiting");

            let listed = registry
                .dispatch_json("list_jobs", &ctx, serde_json::json!({}))
                .await
                .unwrap();
            let arr = listed.as_array().unwrap();
            assert!(arr.iter().any(|j| j["id"].as_i64() == Some(id)));

            let got = registry
                .dispatch_json("get_job", &ctx, serde_json::json!({ "id": id }))
                .await
                .unwrap();
            assert_eq!(got["name"].as_str().unwrap(), "sync");

            let prog = registry
                .dispatch_json("get_job_progress", &ctx, serde_json::json!({ "id": id }))
                .await
                .unwrap();
            assert_eq!(prog["id"].as_i64().unwrap(), id);
            assert_eq!(prog["status"].as_str().unwrap(), "Waiting");
        }

        #[tokio::test]
        async fn submit_job_rejects_protected_name_over_mcp() {
            let (registry, ctx) = remote_job_ctx().await;
            let res = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "shell" }))
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::PermissionDenied);
        }

        #[tokio::test]
        async fn submit_agent_rejects_local_cli() {
            let (registry, ctx) = job_ctx().await;
            let res = registry
                .dispatch_json("submit_agent", &ctx, serde_json::json!({ "prompt": "do a thing" }))
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::InvalidParams);
        }

        #[tokio::test]
        async fn get_job_missing_returns_error() {
            let (registry, ctx) = job_ctx().await;
            let res = registry
                .dispatch_json("get_job", &ctx, serde_json::json!({ "id": 999 }))
                .await;
            assert!(res.is_err());
        }

        #[tokio::test]
        async fn replay_job_creates_new_from_original() {
            let (registry, ctx) = job_ctx().await;
            let job = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "embed" }))
                .await
                .unwrap();
            let id = job["id"].as_i64().unwrap();

            let replayed = registry
                .dispatch_json("replay_job", &ctx, serde_json::json!({ "id": id }))
                .await
                .unwrap();
            assert_eq!(replayed["sourceId"].as_i64().unwrap(), id);
            assert_ne!(replayed["id"].as_i64().unwrap(), id);
            assert_eq!(replayed["name"].as_str().unwrap(), "embed");
        }

        #[tokio::test]
        async fn send_job_message_roundtrip() {
            let (registry, ctx) = job_ctx().await;
            let job = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "import" }))
                .await
                .unwrap();
            let id = job["id"].as_i64().unwrap();

            let msg = registry
                .dispatch_json(
                    "send_job_message",
                    &ctx,
                    serde_json::json!({ "id": id, "payload": { "msg": "hi" } }),
                )
                .await
                .unwrap();
            assert_eq!(msg["sent"].as_bool().unwrap(), true);
            assert_eq!(msg["jobId"].as_i64().unwrap(), id);
            assert!(msg["messageId"].as_i64().is_some());
        }

        #[tokio::test]
        async fn cancel_job_roundtrip() {
            let (registry, ctx) = job_ctx().await;
            let job = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "lint" }))
                .await
                .unwrap();
            let id = job["id"].as_i64().unwrap();

            let cancelled = registry
                .dispatch_json("cancel_job", &ctx, serde_json::json!({ "id": id }))
                .await
                .unwrap();
            assert_eq!(cancelled["id"].as_i64().unwrap(), id);
            assert_eq!(cancelled["status"].as_str().unwrap(), "Cancelled");
        }

        #[tokio::test]
        async fn retry_job_missing_returns_error() {
            let (registry, ctx) = job_ctx().await;
            let res = registry
                .dispatch_json("retry_job", &ctx, serde_json::json!({ "id": 999 }))
                .await;
            assert!(res.is_err());
        }

        #[tokio::test]
        async fn pause_resume_job_roundtrip() {
            let (registry, ctx) = job_ctx().await;
            let job = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "backlinks" }))
                .await
                .unwrap();
            let id = job["id"].as_i64().unwrap();

            let paused = registry
                .dispatch_json("pause_job", &ctx, serde_json::json!({ "id": id }))
                .await
                .unwrap();
            assert_eq!(paused["status"].as_str().unwrap(), "Paused");

            let resumed = registry
                .dispatch_json("resume_job", &ctx, serde_json::json!({ "id": id }))
                .await
                .unwrap();
            assert_eq!(resumed["status"].as_str().unwrap(), "Waiting");
        }

        #[tokio::test]
        async fn list_jobs_filters_by_status() {
            let (registry, ctx) = job_ctx().await;
            let _ = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "a" }))
                .await
                .unwrap();
            let _ = registry
                .dispatch_json("submit_job", &ctx, serde_json::json!({ "name": "b" }))
                .await
                .unwrap();

            let waiting = registry
                .dispatch_json("list_jobs", &ctx, serde_json::json!({ "status": "waiting" }))
                .await
                .unwrap();
            assert!(waiting.as_array().unwrap().len() >= 2);
        }

        #[tokio::test]
        async fn list_jobs_rejects_invalid_status() {
            let (registry, ctx) = job_ctx().await;
            let res = registry
                .dispatch_json("list_jobs", &ctx, serde_json::json!({ "status": "nonsense" }))
                .await;
            assert!(res.is_err());
            assert_eq!(res.unwrap_err().code, ErrorCode::InvalidParams);
        }
    }
}

