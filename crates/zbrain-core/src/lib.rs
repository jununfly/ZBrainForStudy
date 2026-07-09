//! `zbrain-core` — core engine, types and operation contracts.
//!
//! Slice 2 added the structured-error envelope (`error`) and the pure
//! enum/constant subset of the type system (`types`). Slice 3 introduces the
//! `engine` module with the `BrainEngine` trait skeleton and a minimal
//! in-memory mock used to validate object-safety + Page CRUD round-trips.
//! DB-backed engines (postgres / libsql) land in later slices per
//! `docs/plans/20260526/04-plan.md`.

pub mod admin_queries;
pub mod chunkers;
pub mod cjk;
pub mod embedding;
pub mod embedding_context;
pub mod embedding_pricing;
pub mod import;
pub mod scope;
pub mod token_queries;
pub mod calibration_queries;
pub mod capture;
pub mod file_classify;
pub mod markdown;
pub mod oauth_queries;
pub mod engine;
pub mod error;
pub mod explain_formatter;
pub mod git_remote;
pub mod ingestion;
pub mod libsql;
pub mod llm;
pub mod migration;
pub mod operation;
pub mod postgres;
pub mod progress;
pub mod recency_decay;
pub mod rerank_audit;
pub mod rerank_client;
pub mod sources_ops;
pub mod sync;
pub mod takes_fence;
pub mod time;
pub mod types;
pub mod url_safety;

pub use scope::{has_scope, parse_scope_string, is_allowed_scope, assert_allowed_scopes, normalize_scopes_input, InvalidScopeError, ALLOWED_SCOPES};
pub use token_queries::{AuthInfo, TokenError, TokenQueries};
pub use capture::{
    capture_content, derive_title, detect_binary, merge_capture_frontmatter,
    parse_frontmatter_from_body, CaptureError, CaptureOpts, CaptureResult,
};
pub use error::{from_std_error, Error, Result, StructuredError};
pub use types::{
    is_base_page_type, CRMode, DuplicatePage, EffectiveDateSource, FileRow, FileSpec,
    FindDuplicatePageOpts, GraphNode, GraphNodeLink, GraphPath, Link, LinkBatchInput,
    OrphanPage, PageKind, PageRef, PageType, PurgeResult, RefreshPageBodyArgs, Take,
    TakeInput, TakeResolution, UpsertFileResult, UpsertTakesResult, ALL_PAGE_TYPES,
};
pub use engine::{BrainEngine, CreateSourceInput, InMemoryEngine, Page, PageInput, PageFilters, SearchOpts, SearchResult, GetPageOpts, ResolveSlugsOpts, SourceRow, UpdateSourceInput};
pub use admin_queries::{
    AdminQueries, AgentClientSpend, AgentInfo, ApiKey, BudgetOwner, ErrorClusterCount, FullStats,
    HealthIndicators, JobTypeSummary, Paginated, QueueHealth, RequestLogEntry, RequestLogFilters,
    Stats, WatchSnapshot,
};
pub use calibration_queries::{
    CalibrationQueries, CalibrationBucket, CalibrationProfileRow, PatternDetail, TakeSummary,
    TakesScorecard,
};
pub use progress::{ProgressMode, ProgressReporter};
pub use oauth_queries::{
    ExchangeTokens, OAuthClientInfo, OAuthQueries, RegisterClientRequest, RegisterClientResponse,
    RevokeClientResponse, UpdateClientTtlResponse,
};
pub use ingestion::{
    compute_content_hash, compute_raw_hash, normalize_for_hash, detect_content_type,
    is_allowed_ingest_content_type, validate_ingestion_event, IngestionEvent, IngestionEventError,
    INGESTION_CONTENT_TYPES, INGEST_ALLOWED_CONTENT_TYPES,
};
pub use git_remote::{
    clone_repo, pull_repo, validate_repo_state, CloneOpts, GitOp, GitOperationError, RepoState,
    GIT_SSRF_FLAGS, GIT_SSRF_SUBCOMMAND_FLAGS,
};
pub use sources_ops::{
    add_source, default_clone_dir, get_source_status, is_path_contained, reclone_if_missing,
    remove_source, AddSourceOpts, RemoveResult, RemoveSourceOpts, SourceOpError,
    SourceOpErrorCode, SourceStatus,
};
pub use url_safety::{
    is_internal_url, is_private_ipv4, parse_remote_url, ParsedRemoteUrl, RemoteUrlError,
    RemoteUrlErrorCode,
};
pub use markdown::{
    infer_slug, infer_tags, infer_title, infer_type, parse_markdown, split_body, ParsedMarkdown,
};
pub use file_classify::{
    classify_file, detect_code_language, detect_image_format, is_markdown_path, FileType,
};
pub use llm::{LlmClient, LlmRequest, LlmResponse, LlmError, MockLlmClient, TokenUsage};
#[cfg(feature = "openai")]
pub use llm::OpenAiClient;

/// Static crate name. Used by callers (CLI, web, mcp) for diagnostics.
#[must_use]
pub const fn crate_name() -> &'static str {
    "zbrain-core"
}

/// Static crate version, sourced from Cargo at compile time.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_zbrain_core() {
        assert_eq!(crate_name(), "zbrain-core");
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty(), "CARGO_PKG_VERSION must not be empty");
    }
}
