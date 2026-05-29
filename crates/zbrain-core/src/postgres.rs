//! Slice 4b — `PostgresEngine` Page CRUD on top of the 4a lifecycle skeleton.
//!
//! Implements the [`BrainEngine`] page-level surface (`get_page` /
//! `put_page` / `delete_page` / `list_pages` / `resolve_slugs`) against the
//! 4a `pages` schema. The schema deliberately omits `deleted_at`,
//! `frontmatter`, `content_hash`, and embedding columns — those land in
//! later patch slices (6.5a / 6.5b) alongside the trait methods that
//! actually consult them. As a result two narrow behaviors are explicit:
//!
//! - `GetPageOpts.include_deleted = true` returns `Error::unsupported`
//!   (the schema has no `deleted_at` column to filter by; silently
//!   ignoring the flag would mask the gap).
//! - `resolve_slugs` is exact-match only; fuzzy `ILIKE` matching is
//!   deferred to slice 6.5c so this slice stays reviewable.

use std::sync::OnceLock;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use crate::engine::{
    BrainEngine, EngineConfig, EngineKind, GetPageOpts, Page, PageFilters, PageInput,
};
use crate::error::{Error, Result};
use crate::types::PageKind;

/// Embedded SQL migrations, baked into the binary at compile time. Driven by
/// `init_schema`. Future migrations are append-only files under `migrations/`.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Connection-pool-backed engine for `PostgreSQL`.
///
/// The pool is lazily installed by [`PostgresEngine::connect`] and consumed
/// by [`PostgresEngine::disconnect`]. Calling `connect` twice on the same
/// instance is rejected to keep ownership of the pool unambiguous.
pub struct PostgresEngine {
    pool: OnceLock<PgPool>,
}

impl PostgresEngine {
    /// Construct a disconnected engine. Call [`PostgresEngine::connect`]
    /// before any other method.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pool: OnceLock::new(),
        }
    }

    /// Borrow the live pool, or return an `Engine` error if `connect` has
    /// not run yet (or the pool was torn down by `disconnect`).
    fn pool(&self) -> Result<&PgPool> {
        self.pool
            .get()
            .ok_or_else(|| Error::engine("PostgresEngine is not connected"))
    }
}

impl Default for PostgresEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PostgresEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresEngine")
            .field("connected", &self.pool.get().is_some())
            .finish()
    }
}

#[async_trait]
impl BrainEngine for PostgresEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Postgres
    }

    async fn connect(&self, config: &EngineConfig) -> Result<()> {
        let url = config
            .database_url
            .as_deref()
            .ok_or_else(|| Error::engine("PostgresEngine requires EngineConfig.database_url"))?;

        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await
            .map_err(|e| Error::engine(format!("postgres connect failed: {e}")))?;

        self.pool
            .set(pool)
            .map_err(|_| Error::engine("PostgresEngine is already connected"))?;
        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        // `OnceLock` has no `take`; the recommended teardown is to close the
        // pool reference we already hold. `sqlx::Pool::close` is idempotent
        // and safe to call concurrently — once closed, any future query
        // returns `PoolClosed` so subsequent calls through `pool()` still
        // surface a clear error.
        if let Some(pool) = self.pool.get() {
            pool.close().await;
        }
        Ok(())
    }

    async fn init_schema(&self) -> Result<()> {
        let pool = self.pool()?;
        MIGRATOR
            .run(pool)
            .await
            .map_err(|e| Error::engine(format!("migration failed: {e}")))?;
        Ok(())
    }

    // ── Page CRUD — slice 4b ──────────────────────────────────────────────
    // Hand-rolled SQL against the 4a schema. We use `sqlx::query` (not the
    // compile-time-checked `query!` macro) so the crate builds without a
    // live database at compile time — the integration suite still exercises
    // the real round-trip when `ZBRAIN_TEST_PG_URL` is set.

    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> Result<Option<Page>> {
        if opts.include_deleted {
            // FixMe: soft-delete column lands in slice 6.5a; the 4a schema
            // has no `deleted_at` to filter on, so honoring this flag would
            // be a lie. Surface it explicitly until the column exists.
            return Err(Error::unsupported(
                "GetPageOpts.include_deleted requires a deleted_at column (slice 6.5a)",
            ));
        }
        let pool = self.pool()?;
        let source_id = opts.source_id.as_deref().unwrap_or("default");
        let row = sqlx::query(
            "SELECT id, slug, type, page_kind, title, compiled_truth, timeline, source_id \
             FROM pages \
             WHERE slug = $1 \
               AND source_id = $2",
        )
        .bind(slug)
        .bind(source_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("get_page query failed: {e}")))?;

        row.as_ref().map(row_to_page).transpose()
    }

    async fn put_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
        input: &PageInput,
    ) -> Result<Page> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");
        // Upsert by (source_id, slug). ON CONFLICT keeps the original `id`
        // (BIGSERIAL) stable across re-puts within the same source, matching
        // the TS engine + InMemoryEngine contract while allowing the same slug
        // to exist independently under different sources.
        let row = sqlx::query(
            "INSERT INTO pages (slug, type, title, compiled_truth, source_id) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT ON CONSTRAINT pages_source_slug_key DO UPDATE SET \
                 type = EXCLUDED.type, \
                 title = EXCLUDED.title, \
                 compiled_truth = EXCLUDED.compiled_truth, \
                 updated_at = now() \
             RETURNING id, slug, type, page_kind, title, compiled_truth, timeline, source_id",
        )
        .bind(slug)
        .bind(&input.page_type)
        .bind(&input.title)
        .bind(&input.compiled_truth)
        .bind(source_id)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("put_page upsert failed: {e}")))?;

        row_to_page(&row)
    }

    async fn delete_page(&self, slug: &str) -> Result<()> {
        let pool = self.pool()?;
        // No-op on missing slug, matching the TS engine + InMemoryEngine
        // contract. `DELETE` returns affected-row count which we ignore on
        // purpose — callers that want a "did it exist" probe should
        // `get_page` first.
        sqlx::query("DELETE FROM pages WHERE slug = $1")
            .bind(slug)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("delete_page failed: {e}")))?;
        Ok(())
    }

    async fn list_pages(&self, filters: &PageFilters) -> Result<Vec<Page>> {
        let pool = self.pool()?;
        // Single SQL with optional filter — `type = $1 OR $1 IS NULL` keeps
        // the query string static so we do not have to assemble fragments.
        // Limit is applied with `LIMIT $2` when set, otherwise we pass NULL
        // (postgres `LIMIT NULL` = unbounded).
        let rows = sqlx::query(
            "SELECT id, slug, type, page_kind, title, compiled_truth, timeline, source_id \
             FROM pages \
             WHERE ($1::text IS NULL OR type = $1) \
             ORDER BY id ASC \
             LIMIT $2",
        )
        .bind(filters.page_type.as_deref())
        // `Option<i64>::None` becomes SQL NULL → LIMIT NULL (unbounded).
        .bind(filters.limit.map(|n| i64::try_from(n).unwrap_or(i64::MAX)))
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("list_pages failed: {e}")))?;

        rows.iter().map(row_to_page).collect()
    }

    async fn resolve_slugs(&self, partial: &str) -> Result<Vec<String>> {
        // FixMe: slug ILIKE '%partial%' fuzzy matching from TS source code
        // lands in slice 6.5c. For 4b we deliberately do exact matching so
        // callers get a deterministic, schema-honest answer.
        let pool = self.pool()?;
        let rows = sqlx::query("SELECT slug FROM pages WHERE slug = $1 ORDER BY slug ASC")
            .bind(partial)
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("resolve_slugs failed: {e}")))?;

        rows.into_iter()
            .map(|r| {
                r.try_get::<String, _>("slug")
                    .map_err(|e| Error::engine(format!("resolve_slugs decode failed: {e}")))
            })
            .collect()
    }
}

/// Decode a single `pages` row into the engine-level [`Page`] value.
///
/// Centralised so every read path (get / list) shares the same column
/// projection and `page_kind` enum mapping.
fn row_to_page(row: &sqlx::postgres::PgRow) -> Result<Page> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| Error::engine(format!("row decode id: {e}")))?;
    let slug: String = row
        .try_get("slug")
        .map_err(|e| Error::engine(format!("row decode slug: {e}")))?;
    let page_type: String = row
        .try_get("type")
        .map_err(|e| Error::engine(format!("row decode type: {e}")))?;
    let page_kind_str: String = row
        .try_get("page_kind")
        .map_err(|e| Error::engine(format!("row decode page_kind: {e}")))?;
    let title: String = row
        .try_get("title")
        .map_err(|e| Error::engine(format!("row decode title: {e}")))?;
    let compiled_truth: String = row
        .try_get("compiled_truth")
        .map_err(|e| Error::engine(format!("row decode compiled_truth: {e}")))?;
    let timeline: String = row
        .try_get("timeline")
        .map_err(|e| Error::engine(format!("row decode timeline: {e}")))?;
    let source_id: String = row
        .try_get("source_id")
        .map_err(|e| Error::engine(format!("row decode source_id: {e}")))?;

    let page_kind = decode_page_kind(&page_kind_str)?;
    let id_u64 = u64::try_from(id)
        .map_err(|_| Error::engine(format!("page id {id} negative; corrupt row")))?;

    // S2/S6a-pg placeholder: decode the current narrow PG projection,
    // including `source_id`; the remaining Page fields default until the PG
    // SELECT is widened to the full 0002+ projection in a later slice.
    //
    // Slice 6a S5 added five more columns (`last_retrieved_at`,
    // `generation`, `embedding`, `chunker_version`, `source_path`). Until
    // the PG SELECT is widened, they default to PG-equivalent values
    // (`generation = 1`, `chunker_version = 1`, optionals `None`).
    Ok(Page {
        id: id_u64,
        slug,
        page_type,
        page_kind,
        title,
        compiled_truth,
        timeline,
        frontmatter: serde_json::Value::Object(serde_json::Map::new()),
        content_hash: None,
        emotional_weight: None,
        created_at: String::new(),
        updated_at: String::new(),
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
        source_id,
        source_kind: None,
        source_uri: None,
        ingested_via: None,
        ingested_at: None,
        contextual_retrieval_mode: None,
        corpus_generation: None,
    })
}

/// Map the `pages.page_kind` TEXT column (constrained to
/// `'markdown'|'code'|'image'` by the schema) to the [`PageKind`] enum.
fn decode_page_kind(value: &str) -> Result<PageKind> {
    match value {
        "markdown" => Ok(PageKind::Markdown),
        "code" => Ok(PageKind::Code),
        "image" => Ok(PageKind::Image),
        other => Err(Error::engine(format!(
            "unknown page_kind value {other:?}; schema CHECK should prevent this"
        ))),
    }
}
