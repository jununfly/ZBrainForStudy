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
    time::current_utc_iso8601, CRMode, EffectiveDateSource, Error, FindDuplicatePageOpts,
    OrphanPage, PageKind, PageRef, PageType, PurgeResult, RefreshPageBodyArgs,
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
#[derive(Debug, Clone, PartialEq)]
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
    /// Source scope for slug lookup. `None` is normalised to `"default"`,
    /// matching [`BrainEngine::put_page`] rather than performing an unscoped
    /// cross-source search.
    pub source_id: Option<String>,
    pub include_deleted: bool,
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
pub trait BrainEngine: Send + Sync {
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

    // ── Page CRUD (slice 3 subset) ────────────────────────────────────────

    /// Fetch a single page by `slug` within `opts.source_id`.
    ///
    /// `opts.source_id = None` is normalised to `"default"`; callers that need
    /// a non-default source must pass it explicitly. Returns `None` if not found
    /// or soft-deleted (unless `opts.include_deleted` is true).
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

    /// Hard-delete a page row by slug.
    async fn delete_page(&self, slug: &str) -> crate::Result<()>;

    /// Return all pages matching `filters`, in insertion order.
    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>>;

    /// Fuzzy slug resolver — returns all slugs containing `partial` as a
    /// substring. Mirrors `resolveSlugs` in `engine.ts:708`.
    async fn resolve_slugs(&self, partial: &str) -> crate::Result<Vec<String>>;

    // ── Slice 6a S6 method group (13 new methods) ─────────────────────────
    //
    // Default implementations return `Error::Unsupported("pending slice 6a")`
    // so existing backends (postgres / libsql / in-memory) compile unchanged.
    // The S6-T2 green phase overrides them per backend; postgres holds on
    // `pending slice 6a-pg` until slice 6a-pg lands.
    //
    // Method ordering: §13.2 of `13-slice-6a-gap-checklist.md`.

    // — Duplicate detection (1) —
    async fn find_duplicate_page(
        &self,
        _source_id: &str,
        _opts: &FindDuplicatePageOpts,
    ) -> crate::Result<Option<Page>> {
        Err(Error::unsupported("pending slice 6a"))
    }

    // — Soft-delete lifecycle (3) —
    /// Soft-delete a page (set `deleted_at = CURRENT_TIMESTAMP`).
    /// Returns `Some(slug)` if a row was hit, `None` if the slug was already
    /// missing or already soft-deleted. Mirrors TS `softDeletePage` which
    /// returns `{ slug } | null`.
    async fn soft_delete_page(
        &self,
        _slug: &str,
        _source_id: Option<&str>,
    ) -> crate::Result<Option<String>> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// Restore a previously soft-deleted page. Returns `true` if a row was
    /// affected, `false` otherwise. Mirrors TS `restorePage`.
    async fn restore_page(&self, _slug: &str, _source_id: Option<&str>) -> crate::Result<bool> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// Hard-delete pages whose `deleted_at` is older than `older_than_hours`
    /// ago. Returns the cleared slugs plus the count. Mirrors TS
    /// `purgeDeletedPages`.
    async fn purge_deleted_pages(&self, _older_than_hours: u32) -> crate::Result<PurgeResult> {
        Err(Error::unsupported("pending slice 6a"))
    }

    // — Tag CRUD (3) —
    /// Attach `tag` to the page identified by (`slug`, `source_id`). Mirrors
    /// TS `addTag` which throws when the page is missing — Rust returns
    /// `Err(Error::page_not_found(..))` in that case. Idempotent on duplicate
    /// (tag, page) pairs.
    async fn add_tag(
        &self,
        _slug: &str,
        _tag: &str,
        _source_id: Option<&str>,
    ) -> crate::Result<()> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// Detach `tag` from the page identified by (`slug`, `source_id`). Mirrors
    /// TS `removeTag` whose sub-select silently no-ops when the page is
    /// missing — Rust preserves that asymmetry and returns `Ok(())`.
    async fn remove_tag(
        &self,
        _slug: &str,
        _tag: &str,
        _source_id: Option<&str>,
    ) -> crate::Result<()> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// List the tags currently attached to (`slug`, `source_id`), ordered by
    /// tag ascending. Mirrors TS `getTags` which returns `[]` for missing
    /// pages.
    async fn get_tags(&self, _slug: &str, _source_id: Option<&str>) -> crate::Result<Vec<String>> {
        Err(Error::unsupported("pending slice 6a"))
    }

    // — Content refresh (2) —
    /// Update `compiled_truth`, `timeline`, `content_hash` for an existing
    /// page (typically after a re-importer pass). Mirrors TS
    /// `refreshPageBody`.
    async fn refresh_page_body(&self, _args: &RefreshPageBodyArgs) -> crate::Result<()> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// Update the `contextual_retrieval_mode` + `corpus_generation` columns.
    /// `mode` is `&str` (not `CRMode`) in 6a so we can ship without
    /// re-validating every TS string literal; the S6-T2 review may upgrade
    /// the param to `CRMode` if the enum is found to be stable.
    async fn update_page_contextual_retrieval_state(
        &self,
        _slug: &str,
        _source_id: &str,
        _mode: &str,
        _corpus_generation: Option<&str>,
    ) -> crate::Result<()> {
        Err(Error::unsupported("pending slice 6a"))
    }

    // — Bulk slug / ref enumeration (3) —
    /// Return the set of all live (non-soft-deleted) slugs, optionally
    /// scoped to `source_id`. Mirrors TS `getAllSlugs`.
    async fn get_all_slugs(
        &self,
        _source_id: Option<&str>,
    ) -> crate::Result<std::collections::HashSet<String>> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// Return every live `(slug, source_id)` pair, ordered by
    /// `(source_id, slug)` ascending. Mirrors TS `listAllPageRefs`.
    async fn list_all_page_refs(&self) -> crate::Result<Vec<PageRef>> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// Return pages with zero inbound links from live pages. Mirrors TS
    /// `findOrphanPages` — discovered late in S6-T0 (was missing from the
    /// initial 12-method tally). Both sides of the join must filter out
    /// soft-deleted rows.
    async fn find_orphan_pages(&self) -> crate::Result<Vec<OrphanPage>> {
        Err(Error::unsupported("pending slice 6a"))
    }

    // — Batch timestamps / scores (3) —
    /// Resolve `slug` → `COALESCE(updated_at, created_at)` for many slugs at
    /// once. Mirrors TS `getPageTimestamps`. Missing slugs are omitted from
    /// the returned map (caller must handle absence).
    ///
    /// Values are ISO-8601 strings, matching the rest of the core API (see
    /// `Page::created_at` / `Page::updated_at`). §13 originally specified
    /// `chrono::DateTime<Utc>`; we keep `String` to avoid pulling `chrono`
    /// into `zbrain-core` and to stay aligned with `Page`'s field types.
    /// Deviation logged in §13.6.
    async fn get_page_timestamps(
        &self,
        _slugs: &[String],
    ) -> crate::Result<std::collections::HashMap<String, String>> {
        Err(Error::unsupported("pending slice 6a"))
    }

    /// Resolve `(slug, source_id)` → `COALESCE(effective_date, updated_at,
    /// created_at)`. Key format: `"{source_id}::{slug}"` so the caller can
    /// disambiguate slugs that collide across sources. Mirrors TS
    /// `getEffectiveDates`.
    ///
    /// Values are ISO-8601 strings; see `get_page_timestamps` for rationale.
    async fn get_effective_dates(
        &self,
        _refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, String>> {
        Err(Error::unsupported("pending slice 6a"))
    }

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
        _refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, f64>> {
        Err(Error::unsupported("pending slice 6a"))
    }
}

// ─── InMemoryEngine ──────────────────────────────────────────────────────────

/// In-process engine backed by a `Vec<Page>`. Not persistent, not
/// transactional — its only job is to validate the trait contract in unit
/// tests and integration harnesses.
#[derive(Debug, Default)]
pub struct InMemoryEngine {
    store: Mutex<Vec<Page>>,
    next_id: Mutex<u64>,
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

    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> crate::Result<Option<Page>> {
        let source_id = opts.source_id.as_deref().unwrap_or("default");
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .find(|p| {
                p.slug == slug
                    && p.source_id == source_id
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
        // can hold independent rows under the same slug. NOTE: get_page /
        // delete_page still match slug-only (slated for S6-T9).
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

    async fn delete_page(&self, slug: &str) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        store.retain(|p| p.slug != slug);
        Ok(())
    }

    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut pages: Vec<Page> = store
            .iter()
            .filter(|p| {
                filters
                    .page_type
                    .as_deref()
                    .is_none_or(|t| p.page_type == t)
            })
            .cloned()
            .collect();
        if let Some(limit) = filters.limit {
            pages.truncate(limit);
        }
        Ok(pages)
    }

    async fn resolve_slugs(&self, partial: &str) -> crate::Result<Vec<String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .filter(|p| p.slug.contains(partial))
            .map(|p| p.slug.clone())
            .collect())
    }

    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> crate::Result<Option<Page>> {
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
            .cloned())
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
}
