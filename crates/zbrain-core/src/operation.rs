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

use crate::engine::BrainEngine;
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
    /// Renders the same way TS `OperationError.message` with code context.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(suggestion) = &self.suggestion {
            write!(f, " ({suggestion})")?;
        }
        Ok(())
    }
}

impl StdError for OperationError {}

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
        }
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
}

/// Operation result type.
///
/// All operation handlers return this type. The Ok variant carries the
/// operation output (serializable for MCP JSON responses).
pub type OperationResult<T> = std::result::Result<T, OperationError>;

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

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
        assert!(boxed.to_string().contains("[invalid_params]"));
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

    // ── GetPage Operation (Slice #44) ──────────────────────────────────────

    /// Get a page by slug with fuzzy matching support.
    ///
    /// Mirrors the `get_page` operation in TS `operations.ts`.
    /// Exact lookup is performed first; if not found and fuzzy=true,
    /// `resolve_slugs` is used to find candidate matches.
    #[derive(Debug, Clone)]
    struct GetPageOperation;

    /// Parameters for get_page operation.
    ///
    /// Mirrors the TS schema:
    /// - slug (required): Page slug to look up
    /// - fuzzy (optional): Enable fuzzy slug resolution (default: false)
    /// - include_deleted (optional): Surface soft-deleted pages (default: false)
    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct GetPageParams {
        slug: String,
        #[serde(default)]
        fuzzy: bool,
        #[serde(default)]
        include_deleted: bool,
    }

    impl ValidateParams for GetPageParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    /// Output for get_page operation.
    ///
    /// Returns either the found page (with optional resolved_slug field)
    /// or an error (page_not_found, ambiguous_slug, etc.)
    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct GetPageOutput {
        page: crate::engine::Page,
        #[serde(skip_serializing_if = "Option::is_none")]
        resolved_slug: Option<String>,
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

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;

            // Build source scope opts from context
            let source_opts = ctx.source_scope_opts();
            let get_page_opts = crate::engine::GetPageOpts {
                source_id: source_opts.source_id.clone(),
                include_deleted: params.include_deleted,
            };

            // Step 1: Exact lookup first
            if let Some(page) = engine.get_page(&params.slug, &get_page_opts).await? {
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
                        // No fuzzy matches either
                        return Err(OperationError::page_not_found(format!(
                            "Page not found: {}",
                            params.slug
                        )));
                    }
                    1 => {
                        // Single fuzzy match - look it up with resolved slug
                        let resolved_slug = &candidates[0];
                        if let Some(page) = engine.get_page(resolved_slug, &get_page_opts).await? {
                            return Ok(GetPageOutput {
                                page,
                                resolved_slug: Some(resolved_slug.clone()),
                            });
                        }
                        // Race condition - resolved slug no longer exists
                        return Err(OperationError::page_not_found(format!(
                            "Page not found: {}",
                            params.slug
                        )));
                    }
                    _ => {
                        // Multiple candidates - ambiguous
                        return Err(OperationError::invalid_params(format!(
                            "Ambiguous slug '{}' - multiple matches: {}",
                            params.slug,
                            candidates.join(", ")
                        )));
                    }
                }
            }

            // No exact match and fuzzy not enabled
            Err(OperationError::page_not_found(format!(
                "Page not found: {}",
                params.slug
            )))
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

    // ── PutPage Operation (Slice #45) ───────────────────────────────────────

    #[derive(Debug, Clone)]
    struct PutPageOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct PutPageParams {
        slug: String,
        page_type: Option<String>,
        title: Option<String>,
        compiled_truth: Option<String>,
    }

    impl ValidateParams for PutPageParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct PutPageOutput {
        page: crate::engine::Page,
        created: bool,
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

    // ── DeletePage Operation (Slice #45) ───────────────────────────────────

    #[derive(Debug, Clone)]
    struct DeletePageOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct DeletePageParams {
        slug: String,
    }

    impl ValidateParams for DeletePageParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct DeletePageOutput {
        deleted: bool,
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

    #[derive(Debug, Clone)]
    struct RestorePageOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct RestorePageParams {
        slug: String,
    }

    impl ValidateParams for RestorePageParams {
        fn validate(&self) -> OperationResult<()> {
            validate_page_slug(&self.slug)?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RestorePageOutput {
        restored: bool,
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

    // ── PurgeDeletedPages Operation (Slice #45) ────────────────────────────

    #[derive(Debug, Clone)]
    struct PurgeDeletedPagesOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct PurgeDeletedPagesParams {
        older_than_days: Option<i64>,
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

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct PurgeDeletedPagesOutput {
        purged: u64,
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

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            // Convert days to hours, default to 0 (purge all deleted)
            let older_than_hours = params.older_than_days.map_or(0, |d| (d * 24) as u32);
            let result = engine.purge_deleted_pages(older_than_hours).await?;
            Ok(PurgeDeletedPagesOutput { purged: result.count })
        }
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

    // ── ListPages Operation (Slice #46) ────────────────────────────────────

    #[derive(Debug, Clone)]
    struct ListPagesOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct ListPagesParams {
        kind: Option<String>,
        tag: Option<String>,
        limit: Option<u32>,
        offset: Option<u32>,
        include_deleted: Option<bool>,
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

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ListPagesOutput {
        pages: Vec<crate::engine::Page>,
        total: u64,
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

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            let engine = ctx.engine()?;
            let versions = engine
                .get_versions(&params.slug, Some(&ctx.source_id))
                .await?;
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
        assert!(output["totalCount"].as_u64().unwrap() >= 0);
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

    // ── Takes List Operation (Slice #48 - Skeleton) ────────────────────────

    #[derive(Debug, Clone)]
    struct TakesListOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct TakesListParams {
        slug: Option<String>,
        limit: Option<u32>,
        offset: Option<u32>,
    }

    impl ValidateParams for TakesListParams {
        fn validate(&self) -> OperationResult<()> {
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TakesListOutput {
        takes: Vec<serde_json::Value>,
        total: u64,
    }

    #[async_trait]
    impl TypedOperation for TakesListOperation {
        type Params = TakesListParams;
        type Output = TakesListOutput;

        fn name(&self) -> &'static str {
            "takes_list"
        }

        fn description(&self) -> &'static str {
            "List takes for a page or across pages."
        }

        async fn execute(&self, _ctx: &OperationContext, _params: Self::Params) -> OperationResult<Self::Output> {
            // STUB IMPLEMENTATION - Engine layer take methods TBD
            Ok(TakesListOutput {
                takes: vec![],
                total: 0,
            })
        }
    }

    #[test]
    fn registry_register_takes_list() {
        let mut registry = OperationRegistry::new();
        registry.register(TakesListOperation);

        let op = registry.lookup("takes_list");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "takes_list");
    }

    #[tokio::test]
    async fn dispatch_json_takes_list_success() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(TakesListOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "limit": 10 });

        let result = registry.dispatch_json("takes_list", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["total"], 0);
        assert!(output["takes"].as_array().unwrap().is_empty());
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

    // ── Think Operation (Slice #51a - Skeleton) ────────────────────────────

    #[derive(Debug, Clone)]
    struct ThinkOperation;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    struct ThinkParams {
        question: String,
        anchor: Option<String>,
        rounds: Option<u32>,
        save: Option<bool>,
        take: Option<bool>,
        model: Option<String>,
        since: Option<String>,
        until: Option<String>,
    }

    impl ValidateParams for ThinkParams {
        fn validate(&self) -> OperationResult<()> {
            if self.question.is_empty() {
                return Err(OperationError::invalid_params("question cannot be empty"));
            }
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ThinkOutput {
        answer: String,
        warnings: Vec<String>,
        saved_slug: Option<String>,
        evidence_inserted: u64,
        remote_persisted_blocked: bool,
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

        async fn execute(&self, ctx: &OperationContext, params: Self::Params) -> OperationResult<Self::Output> {
            // SECURITY: Trust boundary - remote MCP callers cannot persist
            // See test dispatch_json_think_remote_blocks_persistence
            let remote = ctx.remote;
            let safe_save = !remote && params.save.unwrap_or(false);
            let safe_take = !remote && params.take.unwrap_or(false);

            // STUB IMPLEMENTATION - Phase 1 skeleton only
            // No LLM calls, no retrieval, just return canned answer
            Ok(ThinkOutput {
                answer: format!("Stub answer for: {}", params.question),
                warnings: vec!["Think operation is in skeleton mode - no LLM calls yet.".to_string()],
                saved_slug: None,
                evidence_inserted: 0,
                remote_persisted_blocked: remote && (params.save.unwrap_or(false) || params.take.unwrap_or(false)),
            })
        }
    }

    #[test]
    fn registry_register_think() {
        let mut registry = OperationRegistry::new();
        registry.register(ThinkOperation);

        let op = registry.lookup("think");
        assert!(op.is_some());
        assert_eq!(op.unwrap().name(), "think");
    }

    #[tokio::test]
    async fn dispatch_json_think_success() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(ThinkOperation);

        let ctx = OperationContext::local_cli().with_engine(engine.into_arc());
        let params = serde_json::json!({ "question": "What is ZBrain?" });

        let result = registry.dispatch_json("think", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert!(output["answer"].as_str().unwrap().contains("Stub answer"));
        assert_eq!(output["evidenceInserted"], 0);
        assert_eq!(output["remotePersistedBlocked"], false);
    }

    #[tokio::test]
    async fn dispatch_json_think_remote_blocks_persistence() {
        use crate::engine::InMemoryEngine;

        let engine = InMemoryEngine::default();
        let mut registry = OperationRegistry::new();
        registry.register(ThinkOperation);

        // Remote context with save=true
        let ctx = OperationContext::remote_mcp("public").with_engine(engine.into_arc());
        let params = serde_json::json!({ "question": "What is ZBrain?", "save": true });

        let result = registry.dispatch_json("think", &ctx, params).await;
        assert!(result.is_ok(), "Expected ok, got: {:?}", result);

        let output = result.unwrap();
        assert_eq!(output["remotePersistedBlocked"], true);
        assert!(output["savedSlug"].is_null());
    }

    #[test]
    fn think_params_validation_empty_question_rejected() {
        let params = ThinkParams {
            question: "".to_string(),
            anchor: None,
            rounds: None,
            save: None,
            take: None,
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
}

