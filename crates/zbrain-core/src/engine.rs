//! Slice 3 — `BrainEngine` trait skeleton + in-memory mock.
//!
//! Slice 6a S2 expanded `Page` / `PageInput` / `PageFilters` to mirror the
//! full TS shape (`src/core/types.ts:73|199|277`) so that the libsql and
//! postgres engines have a place to land every column the 0002 schema exposes.
//!
//! Wider method groups (chunks, links, takes, facts, timeline, config,
//! migrations, eval, emotional) are intentionally deferred to later slices
//! so this trait boundary stays reviewable in a single PR.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::{
    calibration_queries::{CalibrationBucket, CalibrationProfileRow, CalibrationQueries,
        PatternDetail, TakeSummary, TakesScorecard},
    oauth_queries::{ExchangeTokens, OAuthClientInfo, OAuthQueries, RegisterClientRequest,
        RegisterClientResponse, RevokeClientResponse, UpdateClientTtlResponse},
    time::current_utc_iso8601, types::PageVersion, types::RawData, CRMode, DuplicatePage,
    EffectiveDateSource, Error, FileRow, FileSpec, FindDuplicatePageOpts, OrphanPage, PageKind,
    PageRef, PageType, PurgeResult, RefreshPageBodyArgs, UpsertFileResult,
};

// ─── Value types ─────────────────────────────────────────────────────────────

/// Discriminates engine backend. Lets migrations / diagnostics branch without
/// `instanceof` / dynamic dispatch tricks. Mirrors the TS `'postgres'|'pglite'`
/// literal union; `InMemory` is Rust-only (test double).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Postgres,
    Libsql,
    /// In-process mock used in unit tests.
    InMemory,
}

/// Startup configuration passed to [`BrainEngine::connect`].
/// Mirrors `EngineConfig` in `src/core/types.ts:1285`.
#[derive(Debug, Default, Clone)]
pub struct EngineConfig {
    pub database_url: Option<String>,
    pub database_path: Option<String>,
}

/// A persisted brain page. Mirrors `Page` in `src/core/types.ts:73`.
///
/// Slice 6a S2 expanded the struct to carry the 0002 projection (24 fields).
/// Slice 6a S5 added the last five columns — `last_retrieved_at`,
/// `generation`, `embedding`, `chunker_version`, `source_path` — bringing
/// the total to **29 fields**, matching the full TS shape after the 0003
/// migration. TS `Date` fields are `String` (ISO-8601) to avoid depending
/// on `chrono` in this slice; `frontmatter` is `serde_json::Value`
/// (TEXT-stored JSON in `SQLite`, JSONB in Postgres).
///
/// `Eq` is intentionally dropped because `emotional_weight: Option<f64>` and
/// `frontmatter: Value` do not implement `Eq`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    // ── identity (always present) ────────────────────────────────────────
    pub id: u64,
    pub slug: String,
    pub page_type: PageType,
    pub page_kind: PageKind,
    pub title: String,
    pub compiled_truth: String,
    pub timeline: String,

    // ── content metadata ─────────────────────────────────────────────────
    /// Free-form JSON frontmatter stored as TEXT (`SQLite`) / JSONB (Postgres).
    pub frontmatter: Value,
    /// Content hash (e.g. `sha256:…`). NULL until the importer computes it.
    pub content_hash: Option<String>,
    /// Deterministic 0..1 emotional-weight score. NULL until the recompute cycle.
    pub emotional_weight: Option<f64>,

    // ── timestamps ───────────────────────────────────────────────────────
    /// ISO-8601 `created_at`. Always set by the DB (`DEFAULT CURRENT_TIMESTAMP`).
    pub created_at: String,
    /// ISO-8601 `updated_at`. Always set by the DB.
    pub updated_at: String,
    /// Soft-delete marker. `None` = live; `Some(ts)` = pending purge.
    pub deleted_at: Option<String>,
    /// ISO-8601 timestamp of the last read access. Powers salience decay;
    /// updated by the retrieval pipeline (`brain.recordPageRead`) rather
    /// than the schema trigger. `None` until first read after import.
    pub last_retrieved_at: Option<String>,

    // ── effective-date chain ─────────────────────────────────────────────
    pub effective_date: Option<String>,
    pub effective_date_source: Option<EffectiveDateSource>,
    pub import_filename: Option<String>,

    // ── salience ─────────────────────────────────────────────────────────
    /// Bumped by `recompute_emotional_weight` so salient old pages surface.
    pub salience_touched_at: Option<String>,
    /// Persisted salience score (TS migration adds the column; structurally
    /// catch-up landed in S5 alongside `last_retrieved_at`). Intentionally
    /// excluded from the `bump_page_generation` allow-list — salience is a
    /// background recomputation and bumping `generation` on every salience
    /// touch would over-invalidate the query cache.
    pub salience_score: Option<f64>,

    // ── revision counter + embedding ────────────────────────────────────
    /// Monotonic per-page revision counter. Starts at `1`; the
    /// `bump_page_generation_fn` trigger (0002 + 0003) bumps it whenever a
    /// watched column changes. Consumers use this to invalidate caches.
    pub generation: i64,
    /// Vector embedding bytes (PG `BYTEA` / `SQLite` `BLOB`). `None` until
    /// the embedding worker writes it. Encoding format (f32 LE flat
    /// array vs f16 vs other) is deferred to slice 6e per C4; this slice
    /// only carries the column.
    pub embedding: Option<Vec<u8>>,

    // ── chunker + source path ───────────────────────────────────────────
    /// Chunker generation marker (`NOT NULL DEFAULT 1`). Bumped when the
    /// chunker contract changes so reindex jobs can detect stale chunks.
    pub chunker_version: i32,
    /// Original on-disk path of the imported page (TS migration 2698).
    /// `None` for pages that were never on disk (e.g. captured via API).
    pub source_path: Option<String>,

    // ── source / provenance ──────────────────────────────────────────────
    /// Required (`NOT NULL DEFAULT 'default'`). Which source owns this page.
    pub source_id: String,
    pub source_kind: Option<String>,
    pub source_uri: Option<String>,
    pub ingested_via: Option<String>,
    pub ingested_at: Option<String>,

    // ── contextual retrieval ─────────────────────────────────────────────
    pub contextual_retrieval_mode: Option<CRMode>,
    pub corpus_generation: Option<String>,
}

/// Write-side representation. Mirrors `PageInput` in `src/core/types.ts:199`.
///
/// Only `page_type`, `title`, `compiled_truth` are required; everything else
/// is `Option<…>` with safe `Default::default()` so existing callers compile
/// unchanged.
///
/// Slice 6a S5 added the last two write-side fields (`last_retrieved_at`,
/// `embedding`) so the importer / retrieval pipeline can persist them via
/// the engine without a side-channel. The remaining 14 fields landed in S2.
#[derive(Debug, Clone, Default)]
pub struct PageInput {
    pub page_type: PageType,
    pub title: String,
    pub compiled_truth: String,
    pub timeline: Option<String>,
    pub frontmatter: Option<Value>,
    pub content_hash: Option<String>,
    pub page_kind: Option<PageKind>,
    pub effective_date: Option<String>,
    pub effective_date_source: Option<EffectiveDateSource>,
    pub import_filename: Option<String>,
    pub chunker_version: Option<i32>,
    pub source_path: Option<String>,
    pub source_kind: Option<String>,
    pub source_uri: Option<String>,
    pub ingested_via: Option<String>,
    pub ingested_at: Option<String>,
    /// NEW in S5: ISO-8601 timestamp of the last read access. Lets callers
    /// (e.g. retrieval pipeline) push the value through `put_page` instead
    /// of touching the column out-of-band.
    pub last_retrieved_at: Option<String>,
    /// NEW in S5: vector embedding bytes. `None` leaves the column
    /// untouched on upsert (per C4, encoding deferred to slice 6e).
    pub embedding: Option<Vec<u8>>,
}

/// Sort ordering for [`PageFilters::sort`]. Mirrors the TS `PageFilters.sort`
/// literal union at `types.ts:305`. Each variant maps to a whitelisted SQL
/// fragment via [`page_sort_sql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageSort {
    #[default]
    UpdatedDesc,
    UpdatedAsc,
    CreatedDesc,
    Slug,
}

/// A single search result from `search_pages`.
///
/// Contains the matched page and a relevance score (0..1, higher = more relevant).
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The matched page
    pub page: Page,
    /// Relevance score (0..1)
    pub score: f64,
    /// Keyword snippet extracted from content (for UI display)
    pub snippet: Option<String>,
}

/// Options for `search_pages`.
#[derive(Debug, Default, Clone)]
pub struct SearchOpts {
    /// Keywords to search for (case-insensitive substring match)
    pub keywords: Vec<String>,
    /// Maximum number of results to return
    pub limit: Option<usize>,
    /// Minimum score threshold (0..1)
    pub min_score: Option<f64>,
    /// Source scope (None = all sources)
    pub source_id: Option<String>,
}

/// Returns the whitelisted SQL `ORDER BY` fragment for the given sort mode.
/// Mirrors `PAGE_SORT_SQL` at `types.ts:332`. Engines splice this literal
/// into prepared statements — no SQL-injection risk because the enum is closed.
#[must_use]
pub fn page_sort_sql(sort: PageSort) -> &'static str {
    match sort {
        PageSort::UpdatedDesc => "p.updated_at DESC",
        PageSort::UpdatedAsc => "p.updated_at ASC",
        PageSort::CreatedDesc => "p.created_at DESC",
        PageSort::Slug => "p.slug ASC",
    }
}

/// Filter options for [`BrainEngine::list_pages`]. Mirrors `PageFilters`
/// at `types.ts:277`.
#[derive(Debug, Default, Clone)]
pub struct PageFilters {
    pub page_type: Option<PageType>,
    pub tag: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub updated_after: Option<String>,
    pub slug_prefix: Option<String>,
    pub include_deleted: bool,
    pub sort: Option<PageSort>,
    pub source_id: Option<String>,
    pub source_ids: Option<Vec<String>>,
}

/// Options for [`BrainEngine::get_page`]. Mirrors `GetPageOpts`.
#[derive(Debug, Default, Clone)]
pub struct GetPageOpts {
    /// Source scope for slug lookup. `None` performs an unscoped slug lookup,
    /// matching TS `getPage(slug)` semantics. Callers that need the default
    /// source only must pass `Some("default")` explicitly.
    pub source_id: Option<String>,
    pub include_deleted: bool,
}

/// Options for [`BrainEngine::resolve_slugs`]. Mirrors TS
/// `resolveSlugs(partial, { sourceId, sourceIds })`.
#[derive(Debug, Default, Clone)]
pub struct ResolveSlugsOpts {
    /// Scope to a single source when present.
    pub source_id: Option<String>,
    /// Scope to a federated set of sources when non-empty. This takes
    /// precedence over `source_id`, matching TS source scope precedence.
    pub source_ids: Option<Vec<String>>,
}

/// A row from the `sources` table. Used by webhook handlers to look up
/// source configuration (webhook_secret, tracked_branch, github_repo).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceRow {
    pub id: String,
    pub name: String,
    pub config: serde_json::Value,
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Core engine contract. Every storage backend (postgres, libsql, in-memory)
/// must implement this trait.
///
/// Object-safe by design — callers hold `Arc<dyn BrainEngine>` so that
/// operations / CLI layer can be storage-agnostic.
///
/// Slices 4-8 will extend this trait with additional method groups (chunks,
/// links, takes, facts, …) following the same append-only pattern.
#[async_trait]
pub trait BrainEngine: Send + Sync + std::fmt::Debug {
    // ── Identity ──────────────────────────────────────────────────────────

    /// Returns the backend discriminator. Used for conditional logic in
    /// migrations and diagnostics without `downcast`.
    fn kind(&self) -> EngineKind;

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Open / authenticate the underlying connection pool.
    async fn connect(&self, config: &EngineConfig) -> crate::Result<()>;

    /// Gracefully drain the connection pool and release resources.
    async fn disconnect(&self) -> crate::Result<()>;

    /// Apply schema migrations up to the current version.
    async fn init_schema(&self) -> crate::Result<()>;

    // ── Source lookup ──────────────────────────────────────────────────────

    /// Look up a source by its `config->>'github_repo'` value.
    /// Used by POST /webhooks/github to find the matching source.
    /// Returns `None` if no source is configured for this GitHub repo.
    async fn get_source_by_github_repo(
        &self,
        github_repo: &str,
    ) -> crate::Result<Option<SourceRow>>;

    // ── Page CRUD (slice 3 subset) ────────────────────────────────────────

    /// Fetch a single page by `slug`.
    ///
    /// `opts.source_id = Some(_)` scopes the lookup to that source. `None`
    /// performs the TS-compatible unscoped slug lookup. Returns `None` if not
    /// found or soft-deleted (unless `opts.include_deleted` is true).
    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> crate::Result<Option<Page>>;

    /// Insert or update a page (upsert semantics — same `(source_id, slug)` →
    /// same `id`).
    ///
    /// `source_id = None` is normalised to `"default"` to mirror the TS
    /// `opts?.sourceId ?? 'default'` rule
    /// (`zbrain/src/core/pglite-engine.ts:838`). Pages in different sources
    /// with the same slug are independent rows.
    async fn put_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
        input: &PageInput,
    ) -> crate::Result<Page>;

    /// Hard-delete a page row by (`source_id`, `slug`).
    ///
    /// `source_id = None` is normalised to `"default"`, mirroring TS
    /// `deletePage(slug, opts)` where `opts?.sourceId ?? "default"`.
    async fn delete_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<()>;

    /// Return all pages matching `filters`, ordered by `filters.sort` or the
    /// default [`PageSort::UpdatedDesc`] when no explicit sort is supplied.
    ///
    /// Source filter precedence mirrors TS/PGLite: non-empty
    /// `filters.source_ids` wins over `filters.source_id`; an empty
    /// `filters.source_ids` falls back to `filters.source_id`, or remains
    /// unscoped when no single source is provided.
    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>>;

    /// Slug resolver with TS-compatible exact-first / fuzzy-fallback behavior.
    /// Exact slug matches win and suppress fuzzy candidates; otherwise fuzzy
    /// fallback returns live slugs containing `partial`. Source scoping mirrors
    /// TS `resolveSlugs(partial, opts)` where non-empty `source_ids` takes
    /// precedence over `source_id`.
    async fn resolve_slugs(
        &self,
        partial: &str,
        opts: &ResolveSlugsOpts,
    ) -> crate::Result<Vec<String>>;

    // ── Files (metadata rows; bytes live outside the DB) ──────────────────
    /// Insert or update a file metadata row. Mirrors TS `upsertFile`.
    ///
    /// Identity follows the current TS schema/backend implementation:
    /// `UNIQUE(storage_path)`, not `(source_id, storage_path)`.
    async fn upsert_file(&self, spec: &FileSpec) -> crate::Result<UpsertFileResult>;

    /// Fetch one file metadata row by source and storage path. Mirrors TS
    /// `getFile(sourceId, storagePath)`.
    async fn get_file(&self, source_id: &str, storage_path: &str)
        -> crate::Result<Option<FileRow>>;

    /// List file metadata rows linked to a page id. Mirrors TS
    /// `listFilesForPage(pageId)`.
    async fn list_files_for_page(&self, page_id: u64) -> crate::Result<Vec<FileRow>>;

    // ── Slice 6a S6 method group (15 required methods) ────────────────────
    //
    // Backends must implement the full Slice 6a S6 method group explicitly;
    // these methods have no default body, so leaving one unimplemented is a
    // compile error — exactly the safety net the C1 contract hardening plan
    // (2026-06-01) demands.
    //
    // Method ordering: §13.2 of `13-slice-6a-gap-checklist.md`.

    // — Duplicate detection (1) —
    /// Return the first live duplicate page identity within `source_id`.
    ///
    /// Mirrors TS `findDuplicatePage`, including its minimal return shape:
    /// `{ slug: string; id: number } | null`. Matching is by `content_hash` or,
    /// when supplied, `frontmatter.id`; soft-deleted rows are ignored.
    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> crate::Result<Option<DuplicatePage>>;

    // — Soft-delete lifecycle (3) —
    /// Soft-delete a page (set `deleted_at = CURRENT_TIMESTAMP`).
    /// Returns `Some(slug)` if a row was hit, `None` if the slug was already
    /// missing or already soft-deleted. Mirrors TS `softDeletePage` which
    /// returns `{ slug } | null`.
    async fn soft_delete_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Option<String>>;

    /// Restore a previously soft-deleted page. Returns `true` if a row was
    /// affected, `false` otherwise. Mirrors TS `restorePage`.
    async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<bool>;

    /// Hard-delete pages whose `deleted_at` is older than `older_than_hours`
    /// ago. Returns the cleared slugs plus the count. Mirrors TS
    /// `purgeDeletedPages`.
    async fn purge_deleted_pages(&self, older_than_hours: u32) -> crate::Result<PurgeResult>;

    // — Tag CRUD (3) —
    /// Attach `tag` to the page identified by (`slug`, `source_id`). Mirrors
    /// TS `addTag` which throws when the page is missing — Rust returns
    /// `Err(Error::page_not_found(..))` in that case. Idempotent on duplicate
    /// (tag, page) pairs.
    async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()>;

    /// Detach `tag` from the page identified by (`slug`, `source_id`). Mirrors
    /// TS `removeTag` whose sub-select silently no-ops when the page is
    /// missing — Rust preserves that asymmetry and returns `Ok(())`.
    async fn remove_tag(&self, slug: &str, tag: &str, source_id: Option<&str>)
        -> crate::Result<()>;

    /// List the tags currently attached to (`slug`, `source_id`), ordered by
    /// tag ascending. Mirrors TS `getTags` which returns `[]` for missing
    /// pages.
    async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> crate::Result<Vec<String>>;

    // — Search (1) —
    /// Keyword-based page search.
    ///
    /// Simple single-pass keyword matching against title, compiled_truth, and
    /// frontmatter. No vector search yet — that's for a later slice. Results
    /// are ordered by relevance score (descending).
    ///
    /// Default implementation returns an empty Vec — override with actual
    /// implementation per backend.
    async fn search_pages(&self, _opts: &SearchOpts) -> crate::Result<Vec<SearchResult>> {
        Ok(Vec::new())
    }

    // — Content refresh (2) —
    /// Update `compiled_truth`, `timeline`, `content_hash` for an existing
    /// page (typically after a re-importer pass). Mirrors TS
    /// `refreshPageBody`.
    async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> crate::Result<()>;

    /// Update the `contextual_retrieval_mode` + `corpus_generation` columns.
    /// `mode` is `&str` (not `CRMode`) in 6a so we can ship without
    /// re-validating every TS string literal; the S6-T2 review may upgrade
    /// the param to `CRMode` if the enum is found to be stable.
    async fn update_page_contextual_retrieval_state(
        &self,
        slug: &str,
        source_id: &str,
        mode: &str,
        corpus_generation: Option<&str>,
    ) -> crate::Result<()>;

    // — Raw data / versions / slug rewrite (7) —
    /// Upsert raw sidecar data for a page. Mirrors TS `putRawData`.
    async fn put_raw_data(
        &self,
        slug: &str,
        source: &str,
        data: &Value,
        source_id: Option<&str>,
    ) -> crate::Result<()>;

    /// Get raw sidecar data for a page. Mirrors TS `getRawData`.
    async fn get_raw_data(
        &self,
        slug: &str,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<RawData>>;

    /// Create a version snapshot for a page. Mirrors TS `createVersion`.
    async fn create_version(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<PageVersion>;

    /// Get all versions for a page, newest-first. Mirrors TS `getVersions`.
    async fn get_versions(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<PageVersion>>;

    /// Revert a page to a previous version. Mirrors TS `revertToVersion`.
    async fn revert_to_version(
        &self,
        slug: &str,
        version_id: u64,
        source_id: Option<&str>,
    ) -> crate::Result<()>;

    /// Update a page's slug. Mirrors TS `updateSlug`.
    async fn update_slug(
        &self,
        old_slug: &str,
        new_slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<()>;

    /// Rewrite links after a slug change. Mirrors TS `rewriteLinks`.
    /// Explicit no-op; links use integer page_id foreign keys.
    async fn rewrite_links(&self, _old_slug: &str, _new_slug: &str) -> crate::Result<()>;

    // — Bulk slug / ref enumeration (3) —
    /// Return the set of all live (non-soft-deleted) slugs, optionally
    /// scoped to `source_id`. Mirrors TS `getAllSlugs`.
    async fn get_all_slugs(
        &self,
        source_id: Option<&str>,
    ) -> crate::Result<std::collections::HashSet<String>>;

    /// Return every live `(slug, source_id)` pair, ordered by
    /// `(source_id, slug)` ascending. Mirrors TS `listAllPageRefs`.
    async fn list_all_page_refs(&self) -> crate::Result<Vec<PageRef>>;

    /// Return pages with zero inbound links from live pages. Mirrors TS
    /// `findOrphanPages` — discovered late in S6-T0 (was missing from the
    /// initial 12-method tally). Both sides of the join must filter out
    /// soft-deleted rows.
    async fn find_orphan_pages(&self) -> crate::Result<Vec<OrphanPage>>;

    // — Batch timestamps / scores (3) —
    /// Resolve `slug` → `COALESCE(updated_at, created_at)` for many slugs at
    /// once. Mirrors TS `getPageTimestamps`, including deleted-row visibility:
    /// the TS query does not filter `deleted_at`. Missing slugs are omitted
    /// from the returned map (caller must handle absence).
    ///
    /// Values are ISO-8601 strings, matching the rest of the core API (see
    /// `Page::created_at` / `Page::updated_at`). §13 originally specified
    /// `chrono::DateTime<Utc>`; we keep `String` to avoid pulling `chrono`
    /// into `zbrain-core` and to stay aligned with `Page`'s field types.
    /// Deviation logged in §13.6.
    async fn get_page_timestamps(
        &self,
        slugs: &[String],
    ) -> crate::Result<std::collections::HashMap<String, String>>;

    /// Resolve `(slug, source_id)` → `COALESCE(effective_date, updated_at,
    /// created_at)`. Key format: `"{source_id}::{slug}"` so the caller can
    /// disambiguate slugs that collide across sources. Mirrors TS
    /// `getEffectiveDates`.
    ///
    /// Values are ISO-8601 strings; see `get_page_timestamps` for rationale.
    async fn get_effective_dates(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, String>>;

    /// Compute the salience score for each ref. Formula (mirrors TS
    /// `getSalienceScores`):
    ///
    /// ```text
    /// score = COALESCE(emotional_weight, 0) * 5
    ///       + ln(1 + distinct_active_take_count)
    /// ```
    ///
    /// **6a quirk**: the `takes` table lands in slice 6c. Until then,
    /// `distinct_active_take_count` is hard-coded to `0`, so every score
    /// degenerates to `emotional_weight * 5`. The dedicated red test
    /// `page_methods_salience_scores_takes_zero_until_6c.rs` locks this
    /// behaviour so we cannot accidentally claim 6a is "done with takes".
    async fn get_salience_scores(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, f64>>;
}

// ─── InMemoryEngine ──────────────────────────────────────────────────────────

/// Internal raw-data entry used by `InMemoryEngine`.
/// Keyed by `(page_id, source)` for upsert semantics.
#[derive(Debug, Clone)]
struct InternalRawData {
    page_id: u64,
    source: String,
    data: Value,
    fetched_at: String,
}

/// In-process engine backed by a `Vec<Page>`. Not persistent, not
/// transactional — its only job is to validate the trait contract in unit
/// tests and integration harnesses.
#[derive(Debug, Default)]
pub struct InMemoryEngine {
    store: Mutex<Vec<Page>>,
    next_id: Mutex<u64>,
    file_store: Mutex<Vec<FileRow>>,
    next_file_id: Mutex<u64>,
    /// Raw sidecar data, keyed by `(page_id, source)`.
    raw_data_store: Mutex<Vec<InternalRawData>>,
    /// Page version snapshots, newest-first within each page.
    version_store: Mutex<Vec<PageVersion>>,
    next_version_id: Mutex<u64>,
    /// Token-scope map for testing: access_token → scopes.
    token_scopes: Mutex<std::collections::HashMap<String, Vec<String>>>,
    /// Source configuration rows for testing: source id → SourceRow.
    sources: Mutex<Vec<SourceRow>>,
}

// ─── Tag helpers ─────────────────────────────────────────────────────────────

/// Extract the sorted, deduped tag list from `Page::frontmatter["tags"]`.
fn page_tags(page: &Page) -> Vec<String> {
    page.frontmatter
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Set the tag list on `Page::frontmatter["tags"]`, sorting and deduping.
fn set_page_tags(page: &mut Page, mut tags: Vec<String>) {
    tags.sort();
    tags.dedup();
    if let Some(metadata) = page.frontmatter.as_object_mut() {
        metadata.insert("tags".to_string(), json!(tags));
    } else {
        page.frontmatter = json!({"tags": tags});
    }
}

impl InMemoryEngine {
    /// Wrap in an `Arc` for use as `Arc<dyn BrainEngine>`.
    #[must_use]
    pub fn into_arc(self) -> Arc<dyn BrainEngine> {
        Arc::new(self)
    }

    /// Add a source row for testing webhook source lookups.
    pub fn add_source(&self, source: SourceRow) {
        self.sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned")
            .push(source);
    }
}

#[async_trait]
impl BrainEngine for InMemoryEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::InMemory
    }

    async fn connect(&self, _config: &EngineConfig) -> crate::Result<()> {
        Ok(())
    }

    async fn disconnect(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn init_schema(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn get_source_by_github_repo(
        &self,
        github_repo: &str,
    ) -> crate::Result<Option<SourceRow>> {
        let sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");
        Ok(sources
            .iter()
            .find(|s| {
                s.config
                    .get("github_repo")
                    .and_then(|v| v.as_str())
                    .is_some_and(|repo| repo == github_repo)
            })
            .cloned())
    }

    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> crate::Result<Option<Page>> {
        let source_id = opts.source_id.as_deref();
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .find(|p| {
                p.slug == slug
                    && source_id.is_none_or(|scope| p.source_id == scope)
                    && (opts.include_deleted || p.deleted_at.is_none())
            })
            .cloned())
    }

    async fn put_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
        input: &PageInput,
    ) -> crate::Result<Page> {
        // S6-T8 — normalise `source_id = None` to "default" to mirror TS
        // `opts?.sourceId ?? 'default'` (pglite-engine.ts:838).
        let source_id_norm = source_id.unwrap_or("default");

        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut id_guard = self
            .next_id
            .lock()
            .expect("InMemoryEngine next_id mutex poisoned");

        // S6-T8 — match on compound key `(slug, source_id)` so two sources
        // can hold independent rows under the same slug.
        if let Some(existing) = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id_norm)
        {
            existing.page_type.clone_from(&input.page_type);
            existing.title.clone_from(&input.title);
            existing.compiled_truth.clone_from(&input.compiled_truth);
            if let Some(ref pk) = input.page_kind {
                existing.page_kind = *pk;
            }
            return Ok(existing.clone());
        }

        *id_guard += 1;
        let now = current_utc_iso8601();
        let page = Page {
            id: *id_guard,
            slug: slug.to_string(),
            page_type: input.page_type.clone(),
            page_kind: input.page_kind.unwrap_or(PageKind::Markdown),
            title: input.title.clone(),
            compiled_truth: input.compiled_truth.clone(),
            timeline: input.timeline.clone().unwrap_or_default(),
            frontmatter: input
                .frontmatter
                .clone()
                .unwrap_or(Value::Object(Map::default())),
            content_hash: input.content_hash.clone(),
            emotional_weight: None,
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
            // Mirrors the input on the way in; `None` for fresh rows.
            last_retrieved_at: input.last_retrieved_at.clone(),
            effective_date: input.effective_date.clone(),
            effective_date_source: input.effective_date_source,
            import_filename: input.import_filename.clone(),
            salience_touched_at: None,
            // S5 catch-up: salience_score column added in S4 SQL; fresh rows
            // start with no score (recomputed by background salience job).
            salience_score: None,
            // PG `BIGINT DEFAULT 1` — fresh rows start at generation 1, not 0.
            generation: 1,
            embedding: input.embedding.clone(),
            // PG `INT NOT NULL DEFAULT 1` — fresh rows start at chunker v1
            // unless the caller pins an explicit version.
            chunker_version: input.chunker_version.unwrap_or(1),
            source_path: input.source_path.clone(),
            source_id: source_id_norm.to_string(),
            source_kind: input.source_kind.clone(),
            source_uri: input.source_uri.clone(),
            ingested_via: input.ingested_via.clone(),
            ingested_at: input.ingested_at.clone(),
            contextual_retrieval_mode: None,
            corpus_generation: None,
        };
        store.push(page.clone());
        Ok(page)
    }

    async fn delete_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<()> {
        let source_id = source_id.unwrap_or("default");
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // Soft delete: set deleted_at instead of removing
        for p in store.iter_mut() {
            if p.slug == slug && p.source_id == source_id {
                p.deleted_at = Some("2026-01-01T00:00:00Z".to_string());
            }
        }
        Ok(())
    }

    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let source_ids_filter = filters.source_ids.as_ref().filter(|ids| !ids.is_empty());
        let mut pages: Vec<Page> = store
            .iter()
            .filter(|p| {
                filters
                    .page_type
                    .as_deref()
                    .is_none_or(|t| p.page_type == t)
            })
            .filter(|p| {
                if let Some(ids) = source_ids_filter {
                    ids.iter().any(|id| id == &p.source_id)
                } else {
                    filters
                        .source_id
                        .as_deref()
                        .is_none_or(|source_id| p.source_id == source_id)
                }
            })
            .cloned()
            .collect();
        match filters.sort.unwrap_or_default() {
            PageSort::UpdatedDesc => pages.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)),
            PageSort::UpdatedAsc => pages.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            PageSort::CreatedDesc => pages.sort_by(|a, b| b.created_at.cmp(&a.created_at)),
            PageSort::Slug => pages.sort_by(|a, b| a.slug.cmp(&b.slug)),
        }
        if let Some(limit) = filters.limit {
            pages.truncate(limit);
        }
        Ok(pages)
    }

    async fn resolve_slugs(
        &self,
        partial: &str,
        opts: &ResolveSlugsOpts,
    ) -> crate::Result<Vec<String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let source_matches =
            |source_id: &str| match opts.source_ids.as_ref().filter(|ids| !ids.is_empty()) {
                Some(ids) => ids.iter().any(|id| id == source_id),
                None => opts.source_id.as_deref().is_none_or(|id| source_id == id),
            };

        let exact: Vec<String> = store
            .iter()
            .filter(|p| p.deleted_at.is_none() && p.slug == partial && source_matches(&p.source_id))
            .map(|p| p.slug.clone())
            .collect();
        if !exact.is_empty() {
            return Ok(exact);
        }

        Ok(store
            .iter()
            .filter(|p| {
                p.deleted_at.is_none() && p.slug.contains(partial) && source_matches(&p.source_id)
            })
            .take(5)
            .map(|p| p.slug.clone())
            .collect())
    }

    async fn upsert_file(&self, spec: &FileSpec) -> crate::Result<UpsertFileResult> {
        let source_id = spec.source_id.as_deref().unwrap_or("default");
        let metadata = spec.metadata.clone().unwrap_or_else(|| json!({}));
        let mut file_store = self
            .file_store
            .lock()
            .expect("InMemoryEngine file_store mutex poisoned");

        if let Some(existing) = file_store
            .iter_mut()
            .find(|file| file.storage_path == spec.storage_path)
        {
            existing.source_id = source_id.to_string();
            existing.page_slug = spec.page_slug.clone();
            existing.page_id = spec.page_id;
            existing.filename.clone_from(&spec.filename);
            existing.mime_type = spec.mime_type.clone();
            existing.size_bytes = spec.size_bytes;
            existing.content_hash.clone_from(&spec.content_hash);
            existing.metadata = metadata;
            return Ok(UpsertFileResult {
                id: existing.id,
                created: false,
            });
        }

        let mut next_file_id = self
            .next_file_id
            .lock()
            .expect("InMemoryEngine next_file_id mutex poisoned");
        *next_file_id += 1;
        let id = *next_file_id;
        let row = FileRow {
            id,
            source_id: source_id.to_string(),
            page_slug: spec.page_slug.clone(),
            page_id: spec.page_id,
            filename: spec.filename.clone(),
            storage_path: spec.storage_path.clone(),
            mime_type: spec.mime_type.clone(),
            size_bytes: spec.size_bytes,
            content_hash: spec.content_hash.clone(),
            metadata,
            created_at: current_utc_iso8601(),
        };
        file_store.push(row);
        Ok(UpsertFileResult { id, created: true })
    }

    async fn get_file(
        &self,
        source_id: &str,
        storage_path: &str,
    ) -> crate::Result<Option<FileRow>> {
        let file_store = self
            .file_store
            .lock()
            .expect("InMemoryEngine file_store mutex poisoned");
        Ok(file_store
            .iter()
            .find(|file| file.source_id == source_id && file.storage_path == storage_path)
            .cloned())
    }

    async fn list_files_for_page(&self, page_id: u64) -> crate::Result<Vec<FileRow>> {
        let file_store = self
            .file_store
            .lock()
            .expect("InMemoryEngine file_store mutex poisoned");
        Ok(file_store
            .iter()
            .filter(|file| file.page_id == Some(page_id))
            .cloned()
            .collect())
    }

    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> crate::Result<Option<DuplicatePage>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .find(|p| {
                p.source_id == source_id
                    && p.deleted_at.is_none()
                    && (p.content_hash.as_deref() == Some(opts.content_hash.as_str())
                        || opts.frontmatter_id.as_deref().is_some_and(|id| {
                            p.frontmatter.get("id").and_then(Value::as_str) == Some(id)
                        }))
            })
            .map(|p| DuplicatePage {
                slug: p.slug.clone(),
                id: p.id,
            }))
    }

    async fn soft_delete_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Option<String>> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_none()
                && source_id.is_none_or(|source_id| p.source_id == source_id)
        }) else {
            return Ok(None);
        };

        page.deleted_at = Some(current_utc_iso8601());
        Ok(Some(page.slug.clone()))
    }

    async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<bool> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_some()
                && source_id.is_none_or(|source_id| p.source_id == source_id)
        }) else {
            return Ok(false);
        };

        page.deleted_at = None;
        page.updated_at = current_utc_iso8601();
        Ok(true)
    }

    async fn purge_deleted_pages(&self, _older_than_hours: u32) -> crate::Result<PurgeResult> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut slugs = Vec::new();
        store.retain(|page| {
            if page.deleted_at.is_some() {
                slugs.push(page.slug.clone());
                false
            } else {
                true
            }
        });

        Ok(PurgeResult {
            count: slugs.len() as u64,
            slugs,
        })
    }

    async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_none()
                && source_id.is_none_or(|sid| p.source_id == sid)
        }) else {
            return Err(Error::page_not_found(slug, source_id));
        };
        let mut tags = page_tags(page);
        tags.push(tag.to_string());
        set_page_tags(page, tags);
        Ok(())
    }

    async fn remove_tag(
        &self,
        slug: &str,
        tag: &str,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // `deleted_at IS NULL` mirrors the inner SELECT guard used in
        // postgres.rs:634 / libsql.rs:859. TS `removeTag` is silently
        // no-op on soft-deleted rows; we preserve the same shape here.
        if let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_none()
                && source_id.is_none_or(|sid| p.source_id == sid)
        }) {
            let mut tags = page_tags(page);
            tags.retain(|t| t != tag);
            set_page_tags(page, tags);
        }
        // Silent OK for missing page or absent tag — mirrors TS `removeTag`.
        Ok(())
    }

    async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> crate::Result<Vec<String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // `deleted_at IS NULL` mirrors the inner SELECT guard used in
        // postgres.rs:655 / libsql.rs:878 so soft-deleted pages report
        // no tags regardless of stored state.
        Ok(store
            .iter()
            .find(|p| {
                p.slug == slug
                    && p.deleted_at.is_none()
                    && source_id.is_none_or(|sid| p.source_id == sid)
            })
            .map(page_tags)
            .unwrap_or_default())
    }

    async fn search_pages(&self, opts: &SearchOpts) -> crate::Result<Vec<SearchResult>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");

        let mut results = Vec::new();
        let keywords_lower: Vec<String> = opts.keywords.iter()
            .map(|k| k.to_lowercase())
            .collect();

        for page in store.iter() {
            // Skip deleted pages
            if page.deleted_at.is_some() {
                continue;
            }
            // Source filtering
            if let Some(source_id) = &opts.source_id {
                if page.source_id != *source_id {
                    continue;
                }
            }

            let mut score: f64 = 0.0;
            let mut match_count = 0;

            // Count keyword matches in title, compiled_truth, frontmatter
            let title_lower = page.title.to_lowercase();
            let content_lower = page.compiled_truth.to_lowercase();
            let frontmatter_lower = page.frontmatter.to_string().to_lowercase();

            for keyword in &keywords_lower {
                if title_lower.contains(keyword) {
                    score += 0.4; // Title matches count more
                    match_count += 1;
                }
                if content_lower.contains(keyword) {
                    score += 0.4; // Content matches
                    match_count += 1;
                }
                if frontmatter_lower.contains(keyword) {
                    score += 0.2; // Frontmatter matches
                    match_count += 1;
                }
            }

            if match_count > 0 {
                // Cap score at 1.0
                score = score.min(1.0);

                // Extract snippet (first 150 chars of content around first match)
                let snippet = if !content_lower.is_empty() {
                    let first_match = keywords_lower.iter()
                        .find_map(|k| content_lower.find(k))
                        .unwrap_or(0);
                    let start = first_match.saturating_sub(50);
                    let end = (start + 150).min(content_lower.len());
                    Some(page.compiled_truth[start..end].to_string())
                } else {
                    None
                };

                // Filter by min_score if set
                if opts.min_score.map_or(true, |min| score >= min) {
                    results.push(SearchResult {
                        page: page.clone(),
                        score,
                        snippet,
                    });
                }
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Apply limit if set
        if let Some(limit) = opts.limit {
            results.truncate(limit);
        }

        Ok(results)
    }

    async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        if let Some(page) = store.iter_mut().find(|p| {
            p.slug == args.slug && p.source_id == args.source_id && p.deleted_at.is_none()
        }) {
            page.compiled_truth.clone_from(&args.compiled_truth);
            page.timeline = args.timeline.to_string();
            page.content_hash = Some(args.content_hash.clone());
            page.updated_at = current_utc_iso8601();
        }
        // Silent Ok for missing or soft-deleted rows — mirrors
        // postgres.rs:689 / libsql.rs:899 WHERE deleted_at IS NULL.
        Ok(())
    }

    async fn update_page_contextual_retrieval_state(
        &self,
        slug: &str,
        source_id: &str,
        mode: &str,
        corpus_generation: Option<&str>,
    ) -> crate::Result<()> {
        let cr_mode = match mode {
            "none" => CRMode::None,
            "title" => CRMode::Title,
            "per_chunk_synopsis" => CRMode::PerChunkSynopsis,
            other => {
                return Err(Error::engine(format!(
                    "invalid contextual_retrieval_mode: {other}"
                )))
            }
        };
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        if let Some(page) = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id && p.deleted_at.is_none())
        {
            page.contextual_retrieval_mode = Some(cr_mode);
            page.corpus_generation = corpus_generation.map(std::string::ToString::to_string);
            page.updated_at = current_utc_iso8601();
        }
        // Silent Ok for missing or soft-deleted rows — mirrors
        // postgres.rs:718 / libsql.rs:926 WHERE deleted_at IS NULL.
        Ok(())
    }

    // ─── Raw data / versions / slug rewrite (7) ─────────────────────────────
    // Slice #20: full InMemory behavior replacing the #19 unimplemented!() stubs.

    async fn put_raw_data(
        &self,
        slug: &str,
        source: &str,
        data: &Value,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        // Resolve the page_id for this (slug, source_id).
        let page_id = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            let source_id_norm = source_id.unwrap_or("default");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm)
                .map(|p| p.id)
            // Silent no-op for missing pages mirrors TS putRawData (no
            // explicit page existence check in the TS impl).
        };
        let Some(page_id) = page_id else {
            // Page not found — return an error to surface mis-wired callers.
            return Err(Error::page_not_found(slug, source_id));
        };
        let mut rd_store = self
            .raw_data_store
            .lock()
            .expect("InMemoryEngine raw_data_store mutex poisoned");
        if let Some(existing) = rd_store
            .iter_mut()
            .find(|r| r.page_id == page_id && r.source == source)
        {
            // Upsert: overwrite data + refresh fetched_at.
            existing.data = data.clone();
            existing.fetched_at = current_utc_iso8601();
        } else {
            rd_store.push(InternalRawData {
                page_id,
                source: source.to_string(),
                data: data.clone(),
                fetched_at: current_utc_iso8601(),
            });
        }
        Ok(())
    }

    async fn get_raw_data(
        &self,
        slug: &str,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<RawData>> {
        // Resolve page_id.
        let page_id = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            let source_id_norm = source_id.unwrap_or("default");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm)
                .map(|p| p.id)
        };
        let Some(page_id) = page_id else {
            // Missing page → empty result (mirrors TS getRawData returning []).
            return Ok(vec![]);
        };
        let rd_store = self
            .raw_data_store
            .lock()
            .expect("InMemoryEngine raw_data_store mutex poisoned");
        Ok(rd_store
            .iter()
            .filter(|r| r.page_id == page_id && source.is_none_or(|s| r.source == s))
            .map(|r| RawData {
                source: r.source.clone(),
                data: r.data.clone(),
                fetched_at: r.fetched_at.clone(),
            })
            .collect())
    }

    async fn create_version(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<PageVersion> {
        let source_id_norm = source_id.unwrap_or("default");
        // Snapshot the current page state.
        let (page_id, compiled_truth, frontmatter) = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm && p.deleted_at.is_none())
                .map(|p| (p.id, p.compiled_truth.clone(), p.frontmatter.clone()))
                .ok_or_else(|| Error::page_not_found(slug, source_id))?
        };
        let mut vid_guard = self
            .next_version_id
            .lock()
            .expect("InMemoryEngine next_version_id mutex poisoned");
        *vid_guard += 1;
        let version = PageVersion {
            id: *vid_guard,
            page_id,
            compiled_truth,
            frontmatter,
            snapshot_at: current_utc_iso8601(),
        };
        self.version_store
            .lock()
            .expect("InMemoryEngine version_store mutex poisoned")
            .push(version.clone());
        Ok(version)
    }

    async fn get_versions(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<PageVersion>> {
        let source_id_norm = source_id.unwrap_or("default");
        let page_id = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm)
                .map(|p| p.id)
        };
        let Some(page_id) = page_id else {
            return Ok(vec![]);
        };
        let vs = self
            .version_store
            .lock()
            .expect("InMemoryEngine version_store mutex poisoned");
        // Newest-first: sort by id DESC (InMemory ids are monotonically
        // increasing, so id order == insertion order == snapshot_at order).
        let mut versions: Vec<PageVersion> = vs
            .iter()
            .filter(|v| v.page_id == page_id)
            .cloned()
            .collect();
        versions.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(versions)
    }

    async fn revert_to_version(
        &self,
        slug: &str,
        version_id: u64,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        let source_id_norm = source_id.unwrap_or("default");
        // Find the version snapshot.
        let (compiled_truth, frontmatter) = {
            let vs = self
                .version_store
                .lock()
                .expect("InMemoryEngine version_store mutex poisoned");
            vs.iter()
                .find(|v| v.id == version_id)
                .map(|v| (v.compiled_truth.clone(), v.frontmatter.clone()))
                .ok_or_else(|| {
                    Error::engine(format!("version {version_id} not found for page '{slug}'"))
                })?
        };
        // Apply the snapshot to the live page.
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let page = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id_norm && p.deleted_at.is_none())
            .ok_or_else(|| Error::page_not_found(slug, source_id))?;
        page.compiled_truth = compiled_truth;
        page.frontmatter = frontmatter;
        page.updated_at = current_utc_iso8601();
        Ok(())
    }

    async fn update_slug(
        &self,
        old_slug: &str,
        new_slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        let source_id_norm = source_id.unwrap_or("default");
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // Conflict check: new_slug must not already exist in this source.
        if store
            .iter()
            .any(|p| p.slug == new_slug && p.source_id == source_id_norm)
        {
            return Err(Error::engine(format!(
                "slug '{new_slug}' already exists in source '{source_id_norm}'"
            )));
        }
        let page = store
            .iter_mut()
            .find(|p| p.slug == old_slug && p.source_id == source_id_norm)
            .ok_or_else(|| Error::page_not_found(old_slug, source_id))?;
        page.slug = new_slug.to_string();
        page.updated_at = current_utc_iso8601();
        Ok(())
    }

    /// Explicit no-op — `InMemoryEngine` uses integer `page_id` foreign keys
    /// so there are no embedded slug strings to rewrite. Returns `Ok(())`.
    async fn rewrite_links(&self, _old_slug: &str, _new_slug: &str) -> crate::Result<()> {
        Ok(())
    }

    // ─── Advanced-read overrides (C1 Task 4) ────────────────────────────────
    // Six read-only methods promoted from trait-default `Unsupported` to real
    // InMemory implementations, mirroring postgres.rs:757-983 semantics.

    /// `get_all_slugs` — does NOT filter `deleted_at` (mirrors postgres.rs:759).
    async fn get_all_slugs(
        &self,
        source_id: Option<&str>,
    ) -> crate::Result<std::collections::HashSet<String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .filter(|p| source_id.is_none_or(|sid| p.source_id == sid))
            .map(|p| p.slug.clone())
            .collect())
    }

    /// `list_all_page_refs` — live pages only, ordered by `(source_id, slug)`.
    async fn list_all_page_refs(&self) -> crate::Result<Vec<PageRef>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut refs: Vec<PageRef> = store
            .iter()
            .filter(|p| p.deleted_at.is_none())
            .map(|p| PageRef {
                slug: p.slug.clone(),
                source_id: p.source_id.clone(),
            })
            .collect();
        refs.sort_by(|a, b| {
            a.source_id
                .cmp(&b.source_id)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        Ok(refs)
    }

    /// `find_orphan_pages` — no links table in `InMemory`, so ALL live pages are
    /// orphans. `title` uses `COALESCE(title, slug)` (slug when title empty).
    /// `domain` is extracted from `frontmatter["domain"]` as a string.
    /// Ordered by slug ASC.
    async fn find_orphan_pages(&self) -> crate::Result<Vec<OrphanPage>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut orphans: Vec<OrphanPage> = store
            .iter()
            .filter(|p| p.deleted_at.is_none())
            .map(|p| {
                let title = if p.title.is_empty() {
                    p.slug.clone()
                } else {
                    p.title.clone()
                };
                let domain = p
                    .frontmatter
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(std::string::ToString::to_string);
                OrphanPage {
                    slug: p.slug.clone(),
                    title,
                    domain,
                }
            })
            .collect();
        orphans.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(orphans)
    }

    /// `get_page_timestamps` — key = slug, value = `COALESCE(updated_at,
    /// created_at)`. Mirrors TS deleted-row visibility; missing slugs omitted.
    async fn get_page_timestamps(
        &self,
        slugs: &[String],
    ) -> crate::Result<std::collections::HashMap<String, String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut out = std::collections::HashMap::new();
        for p in store.iter().filter(|p| slugs.iter().any(|s| s == &p.slug)) {
            let ts = if p.updated_at.is_empty() {
                p.created_at.clone()
            } else {
                p.updated_at.clone()
            };
            out.insert(p.slug.clone(), ts);
        }
        Ok(out)
    }

    /// `get_effective_dates` — key = `"{source_id}::{slug}"`, value =
    /// `COALESCE(effective_date, updated_at, created_at)`.
    async fn get_effective_dates(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut out = std::collections::HashMap::new();
        for r in refs {
            if let Some(p) = store
                .iter()
                .find(|p| p.slug == r.slug && p.source_id == r.source_id && p.deleted_at.is_none())
            {
                let ts = p.effective_date.clone().unwrap_or_else(|| {
                    if p.updated_at.is_empty() {
                        p.created_at.clone()
                    } else {
                        p.updated_at.clone()
                    }
                });
                out.insert(format!("{}::{}", r.source_id, r.slug), ts);
            }
        }
        Ok(out)
    }

    /// `get_salience_scores` — key = `"{source_id}::{slug}"`, score =
    /// `emotional_weight.unwrap_or(0.0) * 5.0 + ln(1 + 0)` (no takes table
    /// in `InMemory`, so the takes term is 0 → score = `emotional_weight * 5`).
    async fn get_salience_scores(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, f64>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut out = std::collections::HashMap::new();
        for r in refs {
            if let Some(p) = store
                .iter()
                .find(|p| p.slug == r.slug && p.source_id == r.source_id && p.deleted_at.is_none())
            {
                let value = p.emotional_weight.unwrap_or(0.0) * 5.0;
                out.insert(format!("{}::{}", r.source_id, r.slug), value);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PageKind;

    fn test_page(slug: &str, title: &str, content: &str) -> Page {
        Page {
            id: 0,
            slug: slug.to_string(),
            page_type: "note".to_string(),
            page_kind: PageKind::Markdown,
            title: title.to_string(),
            compiled_truth: content.to_string(),
            timeline: "[]".to_string(),
            frontmatter: serde_json::json!({}),
            content_hash: None,
            emotional_weight: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
            last_retrieved_at: None,
            effective_date: None,
            effective_date_source: None,
            import_filename: None,
            salience_touched_at: None,
            salience_score: None,
            generation: 1,
            embedding: None,
            chunker_version: 1,
            source_path: None,
            source_id: "default".to_string(),
            source_kind: None,
            source_uri: None,
            ingested_via: None,
            ingested_at: None,
            contextual_retrieval_mode: Some(CRMode::None),
            corpus_generation: None,
        }
    }

    #[tokio::test]
    async fn search_pages_finds_keyword_in_title() {
        let engine = InMemoryEngine::default();
        // Put a page with known content
        engine.put_page(
            "rust-guide",
            Some("default"),
            &PageInput {
                page_type: "guide".to_string(),
                title: "Rust Programming Guide".to_string(),
                compiled_truth: "Learn Rust with examples.".to_string(),
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
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
        }).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.slug, "rust-guide");
        assert!(results[0].score > 0.0);
    }

    #[tokio::test]
    async fn search_pages_finds_keyword_in_content() {
        let engine = InMemoryEngine::default();
        engine.put_page(
            "ai-notes",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Random Notes".to_string(),
                compiled_truth: "Contains discussion about machine learning and AI.".to_string(),
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
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["machine learning".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
        }).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.slug, "ai-notes");
    }

    #[tokio::test]
    async fn search_pages_returns_empty_for_no_match() {
        let engine = InMemoryEngine::default();
        engine.put_page(
            "page1",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Test Page".to_string(),
                compiled_truth: "Some content here.".to_string(),
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
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["xyznotfound".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
        }).await.unwrap();

        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn search_pages_respects_limit() {
        let engine = InMemoryEngine::default();
        for i in 1..=5 {
            engine.put_page(
                &format!("page-{i}"),
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: format!("Rust Page {i}").to_string(),
                    compiled_truth: format!("Content about Rust {i}").to_string(),
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
                },
            ).await.unwrap();
        }

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: Some(2),
            min_score: None,
            source_id: None,
        }).await.unwrap();

        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn search_pages_filters_by_source() {
        let engine = InMemoryEngine::default();
        engine.put_page(
            "page-1",
            Some("source-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Rust in Source A".to_string(),
                compiled_truth: "Content".to_string(),
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
            },
        ).await.unwrap();
        engine.put_page(
            "page-2",
            Some("source-b"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Rust in Source B".to_string(),
                compiled_truth: "Content".to_string(),
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
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: Some("source-a".to_string()),
        }).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.source_id, "source-a");
    }

    #[tokio::test]
    async fn search_pages_sorts_by_score() {
        let engine = InMemoryEngine::default();
        // Page 1: Rust in title AND content (higher score)
        engine.put_page(
            "high-score",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Rust Guide".to_string(),  // title match
                compiled_truth: "Rust is a systems language.".to_string(), // content match
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
            },
        ).await.unwrap();
        // Page 2: Rust in content only (lower score)
        engine.put_page(
            "low-score",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Other Title".to_string(),
                compiled_truth: "Has Rust content.".to_string(), // content match only
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
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
        }).await.unwrap();

        assert_eq!(results.len(), 2);
        // Higher score page should come first
        assert!(results[0].score > results[1].score);
    }
}

#[async_trait]
impl crate::admin_queries::AdminQueries for InMemoryEngine {
    async fn get_stats(&self) -> crate::error::Result<crate::admin_queries::Stats> {
        use crate::admin_queries::Stats;
        Ok(Stats {
            connected_agents: 0,
            active_tokens: 0,
            active_api_keys: 0,
            requests_today: 0,
        })
    }

    async fn get_full_stats(&self) -> crate::error::Result<crate::admin_queries::FullStats> {
        use crate::admin_queries::FullStats;
        Ok(FullStats {
            page_count: 0,
            chunk_count: 0,
            engine_ok: true,
        })
    }

    async fn check_health_indicators(&self) -> crate::error::Result<crate::admin_queries::HealthIndicators> {
        use crate::admin_queries::HealthIndicators;
        Ok(HealthIndicators {
            expiring_soon: 0,
            error_rate: 0.0,
        })
    }

    async fn list_agents(&self) -> crate::error::Result<Vec<crate::admin_queries::AgentInfo>> {
        Ok(vec![])
    }

    async fn list_api_keys(&self) -> crate::error::Result<Vec<crate::admin_queries::ApiKey>> {
        Ok(vec![])
    }

    async fn create_api_key(&self, _name: &str) -> crate::error::Result<crate::admin_queries::ApiKey> {
        Err(crate::error::Error::engine("not implemented"))
    }

    async fn revoke_api_key(&self, _name: &str) -> crate::error::Result<()> {
        Err(crate::error::Error::engine("not implemented"))
    }

    async fn list_requests(
        &self,
        _filters: &crate::admin_queries::RequestLogFilters,
    ) -> crate::error::Result<crate::admin_queries::Paginated<crate::admin_queries::RequestLogEntry>> {
        use crate::admin_queries::{Paginated, RequestLogEntry};
        Ok(Paginated {
            items: vec![],
            total: 0,
            page: 1,
            limit: 50,
        })
    }

    async fn list_agent_client_spend(&self) -> crate::error::Result<Vec<crate::admin_queries::AgentClientSpend>> {
        Ok(vec![])
    }

    async fn get_watch_snapshot(&self) -> crate::error::Result<crate::admin_queries::WatchSnapshot> {
        use crate::admin_queries::{QueueHealth, WatchSnapshot};
        Ok(WatchSnapshot {
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
            by_type: vec![],
            queue_health: QueueHealth { waiting: 0, active: 0, stalled: 0 },
            lease_pressure_1h: 0,
            top_errors: vec![],
            budget_owners: vec![],
        })
    }
}

/// In-memory calibration query stubs — all return empty/defaults.
#[async_trait]
impl CalibrationQueries for InMemoryEngine {
    async fn get_scorecard(&self, _holder: &str) -> crate::error::Result<TakesScorecard> {
        Ok(TakesScorecard {
            resolved: 0,
            brier: 0.0,
            accuracy: 0.0,
            correct: 0,
            incorrect: 0,
            partial_rate: 0.0,
        })
    }

    async fn get_calibration_curve(
        &self,
        _holder: &str,
    ) -> crate::error::Result<Vec<CalibrationBucket>> {
        Ok(vec![])
    }

    async fn get_latest_profile(
        &self,
        _holder: &str,
    ) -> crate::error::Result<Option<CalibrationProfileRow>> {
        Ok(None)
    }

    async fn get_pattern_detail(
        &self,
        _holder: &str,
        _pattern_index: usize,
    ) -> crate::error::Result<Option<PatternDetail>> {
        Ok(None)
    }
}

// ── OAuthQueries InMemory stubs ───────────────────────────────────────

#[async_trait]
impl OAuthQueries for InMemoryEngine {
    async fn register_client(
        &self,
        _req: RegisterClientRequest,
    ) -> crate::error::Result<RegisterClientResponse> {
        Ok(RegisterClientResponse {
            client_id: "test-client-id".into(),
            client_secret: "test-client-secret".into(),
        })
    }

    async fn update_client_ttl(
        &self,
        _client_id: &str,
        ttl: Option<i64>,
    ) -> crate::error::Result<UpdateClientTtlResponse> {
        Ok(UpdateClientTtlResponse {
            updated: true,
            token_ttl: ttl,
        })
    }

    async fn revoke_client(
        &self,
        _client_id: &str,
    ) -> crate::error::Result<RevokeClientResponse> {
        Ok(RevokeClientResponse { revoked: true })
    }

    async fn get_client(
        &self,
        client_id: &str,
    ) -> crate::error::Result<Option<OAuthClientInfo>> {
        // Stub: return a fixed client for any non-empty client_id.
        if client_id.is_empty() {
            return Ok(None);
        }
        Ok(Some(OAuthClientInfo {
            client_id: client_id.to_string(),
            client_secret_hash: Some(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            ),
            client_name: "Test Client".to_string(),
            redirect_uris: vec![],
            grant_types: vec!["client_credentials".to_string()],
            scope: Some("read write".to_string()),
            token_endpoint_auth_method: Some("client_secret_post".to_string()),
            client_id_issued_at: Some(1_700_000_000),
            client_secret_expires_at: None,
            token_ttl: None,
        }))
    }

    async fn exchange_client_credentials(
        &self,
        client_id: &str,
        _client_secret: &str,
        requested_scope: Option<&str>,
    ) -> crate::error::Result<ExchangeTokens> {
        let scope = requested_scope.unwrap_or("read").to_string();
        let scopes: Vec<String> = scope
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        let access_token = format!("test_at_{}", client_id);
        self.token_scopes
            .lock()
            .unwrap()
            .insert(access_token.clone(), scopes);
        Ok(ExchangeTokens {
            access_token,
            token_type: "bearer".to_string(),
            expires_in: 3600,
            scope,
            refresh_token: None,
        })
    }

    async fn verify_confidential_client_secret(
        &self,
        client_id: &str,
        _presented_secret: &str,
    ) -> crate::error::Result<OAuthClientInfo> {
        self.get_client(client_id).await?.ok_or_else(|| {
            crate::error::Error::engine("client not found")
        })
    }

    async fn exchange_authorization_code(
        &self,
        client_id: &str,
        _authorization_code: &str,
        _redirect_uri: Option<&str>,
    ) -> crate::error::Result<ExchangeTokens> {
        Ok(ExchangeTokens {
            access_token: format!("test_at_{}", client_id),
            token_type: "bearer".to_string(),
            expires_in: 3600,
            scope: "read write".to_string(),
            refresh_token: Some(format!("test_rt_{}", client_id)),
        })
    }

    async fn exchange_refresh_token(
        &self,
        client_id: &str,
        _refresh_token: &str,
        _requested_scopes: Option<&[String]>,
    ) -> crate::error::Result<ExchangeTokens> {
        Ok(ExchangeTokens {
            access_token: format!("test_at_{}", client_id),
            token_type: "bearer".to_string(),
            expires_in: 3600,
            scope: "read write".to_string(),
            refresh_token: Some(format!("test_rt_{}", client_id)),
        })
    }

    async fn sweep_expired_tokens(&self) -> crate::error::Result<u64> {
        // InMemory is stateless for OAuth — no sweeping needed.
        Ok(0)
    }
}

// ── TokenQueries InMemory stub ────────────────────────────────────────────────

#[async_trait]
impl crate::token_queries::TokenQueries for InMemoryEngine {
    async fn verify_access_token(
        &self,
        token: &str,
    ) -> std::result::Result<crate::token_queries::AuthInfo, crate::token_queries::TokenError> {
        if token.is_empty() {
            return Err(crate::token_queries::TokenError::Invalid);
        }
        let scopes = self
            .token_scopes
            .lock()
            .unwrap()
            .get(token)
            .cloned()
            .unwrap_or_else(|| vec!["read".to_string(), "write".to_string()]);
        Ok(crate::token_queries::AuthInfo {
            token: token.to_string(),
            client_id: "test-client".to_string(),
            client_name: Some("Test Client".to_string()),
            scopes,
            expires_at: i64::MAX,
            source_id: None,
            resource: None,
            allowed_sources: None,
        })
    }
}
