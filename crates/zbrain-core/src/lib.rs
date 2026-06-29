//! `zbrain-core` — core engine, types and operation contracts.
//!
//! Slice 2 added the structured-error envelope (`error`) and the pure
//! enum/constant subset of the type system (`types`). Slice 3 introduces the
//! `engine` module with the `BrainEngine` trait skeleton and a minimal
//! in-memory mock used to validate object-safety + Page CRUD round-trips.
//! DB-backed engines (postgres / libsql) land in later slices per
//! `docs/plans/20260526/04-plan.md`.

pub mod engine;
pub mod error;
pub mod libsql;
pub mod llm;
pub mod migration;
pub mod operation;
pub mod postgres;
pub mod time;
pub mod types;

pub use error::{from_std_error, Error, Result, StructuredError};
pub use types::{
    is_base_page_type, CRMode, DuplicatePage, EffectiveDateSource, FileRow, FileSpec,
    FindDuplicatePageOpts, OrphanPage, PageKind, PageRef, PageType, PurgeResult,
    RefreshPageBodyArgs, UpsertFileResult, ALL_PAGE_TYPES,
};
pub use engine::{BrainEngine, InMemoryEngine, Page, PageInput, PageFilters, SearchOpts, SearchResult, GetPageOpts, ResolveSlugsOpts};
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
