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
use serde_json::{Map, Value};

use crate::{CRMode, EffectiveDateSource, PageKind, PageType};

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
/// Slice 6a S2 expanded the struct to carry the full 19-column projection
/// matching the 0002 schema migration. TS `Date` fields are `String` (ISO-8601)
/// to avoid depending on `chrono` in this slice; `frontmatter` is
/// `serde_json::Value` (TEXT-stored JSON in `SQLite`, JSONB in Postgres).
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

    // ── effective-date chain ─────────────────────────────────────────────
    pub effective_date: Option<String>,
    pub effective_date_source: Option<EffectiveDateSource>,
    pub import_filename: Option<String>,

    // ── salience ─────────────────────────────────────────────────────────
    /// Bumped by `recompute_emotional_weight` so salient old pages surface.
    pub salience_touched_at: Option<String>,

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

    /// Fetch a single page by `slug`. Returns `None` if not found or
    /// soft-deleted (unless `opts.include_deleted` is true).
    async fn get_page(
        &self,
        slug: &str,
        opts: &GetPageOpts,
    ) -> crate::Result<Option<Page>>;

    /// Insert or update a page (upsert semantics — same slug → same `id`).
    async fn put_page(&self, slug: &str, input: &PageInput) -> crate::Result<Page>;

    /// Hard-delete a page row by slug.
    async fn delete_page(&self, slug: &str) -> crate::Result<()>;

    /// Return all pages matching `filters`, in insertion order.
    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>>;

    /// Fuzzy slug resolver — returns all slugs containing `partial` as a
    /// substring. Mirrors `resolveSlugs` in `engine.ts:708`.
    async fn resolve_slugs(&self, partial: &str) -> crate::Result<Vec<String>>;
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

    async fn get_page(&self, slug: &str, _opts: &GetPageOpts) -> crate::Result<Option<Page>> {
        let store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        Ok(store.iter().find(|p| p.slug == slug).cloned())
    }

    async fn put_page(&self, slug: &str, input: &PageInput) -> crate::Result<Page> {
        let mut store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let mut id_guard = self.next_id.lock().expect("InMemoryEngine next_id mutex poisoned");

        if let Some(existing) = store.iter_mut().find(|p| p.slug == slug) {
            existing.page_type.clone_from(&input.page_type);
            existing.title.clone_from(&input.title);
            existing.compiled_truth.clone_from(&input.compiled_truth);
            if let Some(ref pk) = input.page_kind {
                existing.page_kind = *pk;
            }
            return Ok(existing.clone());
        }

        *id_guard += 1;
        let now = "2026-01-01T00:00:00Z".to_string();
        let page = Page {
            id: *id_guard,
            slug: slug.to_string(),
            page_type: input.page_type.clone(),
            page_kind: input.page_kind.unwrap_or(PageKind::Markdown),
            title: input.title.clone(),
            compiled_truth: input.compiled_truth.clone(),
            timeline: input.timeline.clone().unwrap_or_default(),
            frontmatter: input.frontmatter.clone().unwrap_or(Value::Object(Map::default())),
            content_hash: input.content_hash.clone(),
            emotional_weight: None,
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
            effective_date: input.effective_date.clone(),
            effective_date_source: input.effective_date_source,
            import_filename: input.import_filename.clone(),
            salience_touched_at: None,
            source_id: "default".to_string(),
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
        let mut store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        store.retain(|p| p.slug != slug);
        Ok(())
    }

    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>> {
        let store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
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
        let store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .filter(|p| p.slug.contains(partial))
            .map(|p| p.slug.clone())
            .collect())
    }
}
