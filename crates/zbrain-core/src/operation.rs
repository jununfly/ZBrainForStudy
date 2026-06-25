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
/// Mirrors the operation definition pattern in TS. Every operation implements
/// this trait to provide metadata (`name()`, `description()`, `local_only()`)
/// and the handler function that executes the operation.
///
/// Operations are object-safe so they can be stored in a registry and
/// dispatched dynamically (MCP tool listing + invocation).
pub trait Operation: fmt::Debug + Send + Sync {
    /// Stable machine-readable operation name (snake_case).
    ///
    /// Used for MCP tool naming and CLI command dispatch.
    fn name(&self) -> &'static str;

    /// Human-readable one-sentence description.
    ///
    /// Used for MCP tool description and CLI help text.
    fn description(&self) -> &'static str;

    /// Whether this operation is ONLY available to local callers.
    ///
    /// Security-critical: `local_only = true` operations are NOT exposed
    /// via MCP (stdio or HTTP). Used for operations that require direct
    /// filesystem access or machine-level privileges.
    ///
    /// Default: false (exposed to MCP by default). Override to true for
    /// local-only operations (e.g. config editing, direct file imports).
    fn local_only(&self) -> bool {
        false
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
}

