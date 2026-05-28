//! Slice 3 — `BrainEngine` trait skeleton + in-memory mock.
//!
//! This module defines:
//! - Minimal value types: [`Page`], [`PageInput`], [`PageFilters`],
//!   [`GetPageOpts`], [`EngineConfig`]
//! - [`BrainEngine`] trait (lifecycle + Page CRUD subset)
//! - [`InMemoryEngine`] — test double used to prove object-safety and
//!   round-trip correctness before any DB engine lands in slice 4/5.
//!
//! Wider method groups (chunks, links, takes, facts, timeline, config,
//! migrations, eval, emotional) are intentionally deferred to later slices
//! so this trait boundary stays reviewable in a single PR.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{PageKind, PageType};

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
/// Slice 3 carries the stable, DB-agnostic fields only. Embedding vectors,
/// soft-delete timestamps, and source scoping fields are added in slice 4/5
/// alongside the concrete engine implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub id: u64,
    pub slug: String,
    pub page_type: PageType,
    pub page_kind: PageKind,
    pub title: String,
    pub compiled_truth: String,
    pub timeline: String,
}

/// Write-side representation. Mirrors `PageInput` in `src/core/types.ts:199`.
#[derive(Debug, Clone)]
pub struct PageInput {
    pub page_type: PageType,
    pub title: String,
    pub compiled_truth: String,
}

/// Filter options for [`BrainEngine::list_pages`]. Mirrors `PageFilters`.
#[derive(Debug, Default, Clone)]
pub struct PageFilters {
    pub page_type: Option<PageType>,
    pub limit: Option<usize>,
    pub source_id: Option<String>,
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
            return Ok(existing.clone());
        }

        *id_guard += 1;
        let page = Page {
            id: *id_guard,
            slug: slug.to_string(),
            page_type: input.page_type.clone(),
            page_kind: PageKind::Markdown,
            title: input.title.clone(),
            compiled_truth: input.compiled_truth.clone(),
            timeline: String::new(),
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
