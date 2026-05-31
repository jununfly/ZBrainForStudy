//! Slice 4b → #110-b → #110-c — `PostgresEngine` Page CRUD on top of the 4a
//! lifecycle skeleton.
//!
//! Implements the [`BrainEngine`] page-level surface (`get_page` /
//! `put_page` / `delete_page` / `list_pages` / `resolve_slugs`) against the
//! 0001 + 0002 + 0003 + 0004 `pages` schema.
//!
//! Slice #110-c aligns this engine to TS source-of-truth (postgres-engine.ts +
//! pglite-engine.ts + schema.sql). The #110-b PG↔libsql contract review found
//! three "intentional divergences" that were actually bugs; this slice fixes
//! the PG side:
//!
//!   - `put_page` writes 19 columns (not 20). `embedding` and
//!     `last_retrieved_at` are owned by separate code paths
//!     (embedder / retrieval-tracker, ported in a later slice); `put_page`
//!     never writes them and `get_page` always returns `None` for both,
//!     mirroring TS `putPage` in postgres-engine.ts / pglite-engine.ts.
//!   - `ingested_at` is server-stamped when ingestion metadata is present.
//!     If the caller does not supply `ingested_at` and any of `source_kind`,
//!     `source_uri`, `ingested_via` is set, the engine writes `NOW()`.
//!     Mirrors TS pglite-engine.ts:849.
//!   - `corpus_generation` is TEXT (TS schema.sql:131); migration 0004
//!     widens it from the INTEGER mistake introduced by 0003. `frontmatter`
//!     is `JSONB NOT NULL DEFAULT '{}'::jsonb` (TS schema.sql:93); 0004
//!     enforces NOT NULL and the default.
//!
//! libsql parity for the same three behaviours is tracked in slice #110-d
//! (separate task — libsql `put_page` currently has the same shape bug).
//!
//! `resolve_slugs` is still exact-match only; fuzzy `ILIKE` matching is
//! deferred to slice 6.5c so this slice stays reviewable.

use std::sync::OnceLock;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use crate::engine::{
    page_sort_sql, BrainEngine, EngineConfig, EngineKind, GetPageOpts, Page, PageFilters,
    PageInput, PageSort,
};
use crate::error::{Error, Result};
use crate::types::{CRMode, EffectiveDateSource, PageKind};

/// Embedded SQL migrations, baked into the binary at compile time. Driven by
/// `init_schema`. Future migrations are append-only files under `migrations/`.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Full 28-column projection used by every read path (`get_page`,
/// `list_pages`, and the `RETURNING` clause of `put_page`). Centralised so
/// `row_to_page` and SQL stay in lock-step.
///
/// `embedding` and `last_retrieved_at` are intentionally absent: they are
/// owned by the embedder / retrieval-tracker code paths (later slice) and
/// `get_page` always reports `None` for both — mirrors TS `putPage`.
const FULL_PAGE_PROJECTION: &str = "id, slug, type, page_kind, title, compiled_truth, timeline, \
     frontmatter, content_hash, emotional_weight, created_at, updated_at, deleted_at, \
     effective_date, effective_date_source, import_filename, \
     salience_touched_at, salience_score, generation, chunker_version, \
     source_path, source_id, source_kind, source_uri, ingested_via, ingested_at, \
     contextual_retrieval_mode, corpus_generation";

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

fn push_filter_clause(sql: &mut String, param_idx: &mut u32, active: bool, clause: &str) {
    if active {
        let frag = format!(" AND {clause} ${param_idx}");
        sql.push_str(&frag);
        *param_idx += 1;
    }
}

fn build_list_pages_sql(filters: &PageFilters) -> Option<String> {
    // Empty source_ids short-circuit: `source_ids: Some(vec![])` means
    // "match no source" → return empty immediately (mirrors libsql).
    if filters.source_ids.as_ref().is_some_and(Vec::is_empty) {
        return None;
    }

    // Dynamic SQL with optional filters. Only active filters produce bind
    // parameters, keeping the query plan cache-friendly and avoiding
    // `OR $N IS NULL` noise for every possible column.
    let mut sql = format!("SELECT {FULL_PAGE_PROJECTION} FROM pages AS p");
    if filters.tag.is_some() {
        sql.push_str(" JOIN page_tags AS pt ON pt.page_id = p.id");
    }
    sql.push_str(" WHERE 1=1");
    let mut param_idx: u32 = 1;

    push_filter_clause(
        &mut sql,
        &mut param_idx,
        filters.page_type.is_some(),
        "p.type =",
    );
    push_filter_clause(
        &mut sql,
        &mut param_idx,
        filters.source_id.is_some(),
        "p.source_id =",
    );
    push_filter_clause(
        &mut sql,
        &mut param_idx,
        filters.source_ids.is_some(),
        "p.source_id = ANY(",
    );
    if filters.source_ids.is_some() {
        sql.push_str("::text[])");
    }
    push_filter_clause(
        &mut sql,
        &mut param_idx,
        filters.slug_prefix.is_some(),
        "p.slug LIKE",
    );
    if filters.slug_prefix.is_some() {
        sql.push_str(" || '%'");
    }
    push_filter_clause(
        &mut sql,
        &mut param_idx,
        filters.updated_after.is_some(),
        "p.updated_at >",
    );
    if filters.updated_after.is_some() {
        sql.push_str("::timestamptz");
    }
    push_filter_clause(&mut sql, &mut param_idx, filters.tag.is_some(), "pt.tag =");
    if !filters.include_deleted {
        sql.push_str(" AND p.deleted_at IS NULL");
    }

    push_list_pages_sort(&mut sql, filters.sort.unwrap_or_default());
    push_list_pages_pagination(&mut sql, &mut param_idx, filters);
    Some(sql)
}

fn push_list_pages_sort(sql: &mut String, sort_mode: PageSort) {
    sql.push_str(" ORDER BY ");
    sql.push_str(page_sort_sql(sort_mode));
    if sort_mode != PageSort::Slug {
        sql.push_str(", p.slug ASC");
    }
}

fn push_list_pages_pagination(sql: &mut String, param_idx: &mut u32, filters: &PageFilters) {
    if filters.limit.is_some() {
        let frag = format!(" LIMIT ${param_idx}");
        sql.push_str(&frag);
        *param_idx += 1;
    }
    if filters.offset.is_some() {
        let frag = format!(" OFFSET ${param_idx}");
        sql.push_str(&frag);
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
        // - `source_id = $2` with `None` normalised to "default" (cross-engine contract).
        // - `(deleted_at IS NULL OR $3)` — default hides soft-deleted rows;
        //   `include_deleted=true` returns them. Mirrors libsql get_page semantics.
        // Slice #110-b: SELECT projection widened to the full 30-column shape
        //   so every `Page` field round-trips faithfully.
        let pool = self.pool()?;
        let source_id = opts.source_id.as_deref().unwrap_or("default");
        let sql = format!(
            "SELECT {FULL_PAGE_PROJECTION} \
             FROM pages \
             WHERE slug = $1 \
               AND source_id = $2 \
               AND (deleted_at IS NULL OR $3)"
        );
        let row = sqlx::query(&sql)
            .bind(slug)
            .bind(source_id)
            .bind(opts.include_deleted)
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

        // Slice #110-c: 19-column INSERT mirroring TS `putPage`
        // (postgres-engine.ts + pglite-engine.ts). `embedding` and
        // `last_retrieved_at` are NOT written by `put_page`; the embedder
        // and retrieval-tracker code paths own those columns (later slice).
        //
        // `ingested_at` is server-stamped when any ingestion metadata
        // (`source_kind`, `source_uri`, `ingested_via`) is present and the
        // caller did not supply an explicit value — mirrors TS
        // pglite-engine.ts:849.
        //
        // ON CONFLICT keeps the original `id` (BIGSERIAL) stable across
        // re-puts within the same source. UPDATE overwrites the 17
        // user-provided columns unconditionally.
        //
        // Server-managed columns NOT in this INSERT:
        //   id, created_at, updated_at, deleted_at, salience_touched_at,
        //   salience_score, generation (trigger-bumped),
        //   contextual_retrieval_mode, corpus_generation,
        //   embedding, last_retrieved_at.

        let page_kind_str = encode_page_kind(input.page_kind.unwrap_or(PageKind::Markdown));
        let effective_date_source_str = input
            .effective_date_source
            .map(encode_effective_date_source);
        let timeline = input.timeline.clone().unwrap_or_default();
        let frontmatter = input
            .frontmatter
            .clone()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        let chunker_version = input.chunker_version.unwrap_or(1);
        // Server-stamp `ingested_at` when caller omits it AND at least one
        // ingestion-provenance field is present. Otherwise pass through
        // whatever the caller supplied (which may also be None).
        let ingested_at_ts = match input.ingested_at.as_deref() {
            Some(ts) => parse_rfc3339_opt(Some(ts), "ingested_at")?,
            None => {
                if input.source_kind.is_some()
                    || input.source_uri.is_some()
                    || input.ingested_via.is_some()
                {
                    Some(sqlx::types::chrono::Utc::now())
                } else {
                    None
                }
            }
        };

        let sql = format!(
            "INSERT INTO pages (\
                 source_id, slug, type, page_kind, title, compiled_truth, timeline, frontmatter, \
                 content_hash, effective_date, effective_date_source, import_filename, \
                 chunker_version, source_path, source_kind, source_uri, ingested_via, \
                 ingested_at\
             ) VALUES (\
                 $1, $2, $3, $4, $5, $6, $7, $8::jsonb, \
                 $9, $10, $11, $12, \
                 $13, $14, $15, $16, $17, \
                 $18\
             ) \
             ON CONFLICT ON CONSTRAINT pages_source_slug_key DO UPDATE SET \
                 type = EXCLUDED.type, \
                 page_kind = EXCLUDED.page_kind, \
                 title = EXCLUDED.title, \
                 compiled_truth = EXCLUDED.compiled_truth, \
                 timeline = EXCLUDED.timeline, \
                 frontmatter = EXCLUDED.frontmatter, \
                 content_hash = EXCLUDED.content_hash, \
                 effective_date = EXCLUDED.effective_date, \
                 effective_date_source = EXCLUDED.effective_date_source, \
                 import_filename = EXCLUDED.import_filename, \
                 chunker_version = EXCLUDED.chunker_version, \
                 source_path = EXCLUDED.source_path, \
                 source_kind = EXCLUDED.source_kind, \
                 source_uri = EXCLUDED.source_uri, \
                 ingested_via = EXCLUDED.ingested_via, \
                 ingested_at = EXCLUDED.ingested_at, \
                 updated_at = now() \
             RETURNING {FULL_PAGE_PROJECTION}"
        );

        let row = sqlx::query(&sql)
            .bind(source_id)
            .bind(slug)
            .bind(&input.page_type)
            .bind(page_kind_str)
            .bind(&input.title)
            .bind(&input.compiled_truth)
            .bind(timeline)
            .bind(frontmatter)
            .bind(input.content_hash.as_deref())
            .bind(input.effective_date.as_deref())
            .bind(effective_date_source_str)
            .bind(input.import_filename.as_deref())
            .bind(chunker_version)
            .bind(input.source_path.as_deref())
            .bind(input.source_kind.as_deref())
            .bind(input.source_uri.as_deref())
            .bind(input.ingested_via.as_deref())
            .bind(ingested_at_ts)
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
        let Some(sql) = build_list_pages_sql(filters) else {
            return Ok(Vec::new());
        };
        let mut query = sqlx::query(&sql);

        // ORDER CONTRACT: bind order must match `param_idx` advancement in
        // `build_list_pages_sql`: page_type → source_id → source_ids →
        // slug_prefix → updated_after → tag → limit → offset. Reordering either
        // side silently misbinds PG `$N`.
        if let Some(pt) = filters.page_type.as_deref() {
            query = query.bind(pt);
        }
        if let Some(sid) = filters.source_id.as_deref() {
            query = query.bind(sid);
        }
        if let Some(ref ids) = filters.source_ids {
            query = query.bind(ids.as_slice());
        }
        if let Some(prefix) = filters.slug_prefix.as_deref() {
            query = query.bind(prefix);
        }
        if let Some(cutoff) = filters.updated_after.as_deref() {
            query = query.bind(cutoff);
        }
        if let Some(tag) = filters.tag.as_deref() {
            query = query.bind(tag);
        }
        if let Some(limit) = filters.limit {
            query = query.bind(i64::try_from(limit).unwrap_or(i64::MAX));
        }
        if let Some(offset) = filters.offset {
            query = query.bind(i64::try_from(offset).unwrap_or(i64::MAX));
        }

        let rows = query
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("list_pages failed: {e}")))?;

        rows.iter().map(row_to_page).collect()
    }

    async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> Result<()> {
        let pool = self.pool()?;
        let source_id_param = source_id.unwrap_or("default");

        let affected = sqlx::query(
            "INSERT INTO page_tags (page_id, tag) \
             SELECT id, $2 FROM pages \
             WHERE slug = $1 AND source_id = $3 AND deleted_at IS NULL \
             ON CONFLICT (page_id, tag) DO NOTHING",
        )
        .bind(slug)
        .bind(tag)
        .bind(source_id_param)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("add_tag insert failed: {e}")))?
        .rows_affected();

        if affected > 0 {
            return Ok(());
        }

        let page_exists = sqlx::query_scalar::<_, i32>(
            "SELECT 1 FROM pages \
             WHERE slug = $1 AND source_id = $2 AND deleted_at IS NULL \
             LIMIT 1",
        )
        .bind(slug)
        .bind(source_id_param)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("add_tag existence probe failed: {e}")))?
        .is_some();

        if page_exists {
            Ok(())
        } else {
            Err(Error::page_not_found(slug, source_id))
        }
    }

    async fn remove_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> Result<()> {
        let pool = self.pool()?;
        let source_id_param = source_id.unwrap_or("default");

        sqlx::query(
            "DELETE FROM page_tags \
             WHERE tag = $2 \
               AND page_id = ( \
                   SELECT id FROM pages \
                   WHERE slug = $1 AND source_id = $3 AND deleted_at IS NULL \
               )",
        )
        .bind(slug)
        .bind(tag)
        .bind(source_id_param)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("remove_tag delete failed: {e}")))?;

        Ok(())
    }

    async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> Result<Vec<String>> {
        let pool = self.pool()?;
        let source_id_param = source_id.unwrap_or("default");

        sqlx::query_scalar::<_, String>(
            "SELECT tag FROM page_tags \
             WHERE page_id = ( \
                 SELECT id FROM pages \
                 WHERE slug = $1 AND source_id = $2 AND deleted_at IS NULL \
             ) \
             ORDER BY tag",
        )
        .bind(slug)
        .bind(source_id_param)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_tags query failed: {e}")))
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
#[allow(clippy::too_many_lines)] // 28-column decoder — extracting per-field helpers would obscure column→field mapping.
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
    // Slice #72-a: decode `deleted_at` (TIMESTAMPTZ NULL) into ISO8601 to
    // match the engine-level `Option<String>` shape and the libsql side
    // (which stores the SQLite TEXT timestamp directly). Using RFC3339 keeps
    // round-trip lossless and parseable. We use sqlx's re-exported `chrono`
    // (gated by the `chrono` feature already enabled in workspace deps) so
    // this crate does not need a direct `chrono` dependency.
    let deleted_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>> = row
        .try_get("deleted_at")
        .map_err(|e| Error::engine(format!("row decode deleted_at: {e}")))?;
    let deleted_at = deleted_at.map(|ts| ts.to_rfc3339());

    let page_kind = decode_page_kind(&page_kind_str)?;
    let id_u64 = u64::try_from(id)
        .map_err(|_| Error::engine(format!("page id {id} negative; corrupt row")))?;

    // Slice #110-c: PG decoder for the 28-column TS-aligned projection.
    // `embedding` and `last_retrieved_at` are NOT in `FULL_PAGE_PROJECTION`;
    // we always emit `None` for both — they are owned by separate code paths
    // (embedder / retrieval-tracker, later slice). PG/libsql type
    // asymmetries handled here:
    //   * `frontmatter` JSONB NOT NULL DEFAULT '{}' → `serde_json::Value`.
    //   * `created_at`/`updated_at` TIMESTAMPTZ NOT NULL → RFC3339 String.
    //   * `deleted_at`/`salience_touched_at`/`ingested_at` TIMESTAMPTZ NULL
    //     → `Option<String>` RFC3339.
    //   * `generation` BIGINT → `i64`; `chunker_version` INTEGER → `i32`.
    //   * `corpus_generation` TEXT NULL → `Option<String>` direct
    //     (TS schema.sql:131 — 0004 widens from the 0003 INTEGER mistake).
    //   * `emotional_weight`/`salience_score` DOUBLE PRECISION NULL →
    //     `Option<f64>`.
    let frontmatter: serde_json::Value = row
        .try_get("frontmatter")
        .map_err(|e| Error::engine(format!("row decode frontmatter: {e}")))?;
    let content_hash: Option<String> = row
        .try_get("content_hash")
        .map_err(|e| Error::engine(format!("row decode content_hash: {e}")))?;
    let emotional_weight: Option<f64> = row
        .try_get("emotional_weight")
        .map_err(|e| Error::engine(format!("row decode emotional_weight: {e}")))?;
    let created_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = row
        .try_get("created_at")
        .map_err(|e| Error::engine(format!("row decode created_at: {e}")))?;
    let updated_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = row
        .try_get("updated_at")
        .map_err(|e| Error::engine(format!("row decode updated_at: {e}")))?;
    // `last_retrieved_at` is intentionally not in the projection — always None.
    let effective_date: Option<String> = row
        .try_get("effective_date")
        .map_err(|e| Error::engine(format!("row decode effective_date: {e}")))?;
    let effective_date_source_str: Option<String> = row
        .try_get("effective_date_source")
        .map_err(|e| Error::engine(format!("row decode effective_date_source: {e}")))?;
    let effective_date_source = effective_date_source_str
        .as_deref()
        .map(decode_effective_date_source)
        .transpose()?;
    let import_filename: Option<String> = row
        .try_get("import_filename")
        .map_err(|e| Error::engine(format!("row decode import_filename: {e}")))?;
    let salience_touched_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>> = row
        .try_get("salience_touched_at")
        .map_err(|e| Error::engine(format!("row decode salience_touched_at: {e}")))?;
    let salience_score: Option<f64> = row
        .try_get("salience_score")
        .map_err(|e| Error::engine(format!("row decode salience_score: {e}")))?;
    let generation_i64: i64 = row
        .try_get("generation")
        .map_err(|e| Error::engine(format!("row decode generation: {e}")))?;
    // Engine type for `generation` is `i64` (matches PG `BIGINT` directly).
    let generation = generation_i64;
    // `embedding` is intentionally not in the projection — always None.
    let chunker_version_i32: Option<i32> = row
        .try_get("chunker_version")
        .map_err(|e| Error::engine(format!("row decode chunker_version: {e}")))?;
    // Engine type for `chunker_version` is `i32` (matches PG `INTEGER`
    // directly). Default to 1 when NULL — libsql parity.
    let chunker_version = chunker_version_i32.unwrap_or(1);
    let source_path: Option<String> = row
        .try_get("source_path")
        .map_err(|e| Error::engine(format!("row decode source_path: {e}")))?;
    let source_kind: Option<String> = row
        .try_get("source_kind")
        .map_err(|e| Error::engine(format!("row decode source_kind: {e}")))?;
    let source_uri: Option<String> = row
        .try_get("source_uri")
        .map_err(|e| Error::engine(format!("row decode source_uri: {e}")))?;
    let ingested_via: Option<String> = row
        .try_get("ingested_via")
        .map_err(|e| Error::engine(format!("row decode ingested_via: {e}")))?;
    let ingested_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>> = row
        .try_get("ingested_at")
        .map_err(|e| Error::engine(format!("row decode ingested_at: {e}")))?;
    let cr_mode_str: Option<String> = row
        .try_get("contextual_retrieval_mode")
        .map_err(|e| Error::engine(format!("row decode contextual_retrieval_mode: {e}")))?;
    let contextual_retrieval_mode = cr_mode_str.as_deref().map(decode_cr_mode).transpose()?;
    let corpus_generation: Option<String> = row
        .try_get("corpus_generation")
        .map_err(|e| Error::engine(format!("row decode corpus_generation: {e}")))?;

    Ok(Page {
        id: id_u64,
        slug,
        page_type,
        page_kind,
        title,
        compiled_truth,
        timeline,
        frontmatter,
        content_hash,
        emotional_weight,
        created_at: created_at.to_rfc3339(),
        updated_at: updated_at.to_rfc3339(),
        deleted_at,
        last_retrieved_at: None,
        effective_date,
        effective_date_source,
        import_filename,
        salience_touched_at: salience_touched_at.map(|ts| ts.to_rfc3339()),
        salience_score,
        generation,
        embedding: None,
        chunker_version,
        source_path,
        source_id,
        source_kind,
        source_uri,
        ingested_via,
        ingested_at: ingested_at.map(|ts| ts.to_rfc3339()),
        contextual_retrieval_mode,
        corpus_generation,
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

/// Encode [`PageKind`] to its PG wire value (`lowercase` TEXT).
/// Inverse of [`decode_page_kind`]. Kept private and intentionally
/// duplicated with `libsql.rs` so that each backend owns its own enum
/// mapping until/unless the column moves into `types.rs`.
fn encode_page_kind(kind: PageKind) -> &'static str {
    match kind {
        PageKind::Markdown => "markdown",
        PageKind::Code => "code",
        PageKind::Image => "image",
    }
}

/// Encode [`EffectiveDateSource`] to its PG wire value (`snake_case` TEXT).
/// Inverse of [`decode_effective_date_source`].
fn encode_effective_date_source(src: EffectiveDateSource) -> &'static str {
    match src {
        EffectiveDateSource::EventDate => "event_date",
        EffectiveDateSource::Date => "date",
        EffectiveDateSource::Published => "published",
        EffectiveDateSource::Filename => "filename",
        EffectiveDateSource::Fallback => "fallback",
    }
}

/// Decode the PG `effective_date_source` TEXT column.
fn decode_effective_date_source(value: &str) -> Result<EffectiveDateSource> {
    match value {
        "event_date" => Ok(EffectiveDateSource::EventDate),
        "date" => Ok(EffectiveDateSource::Date),
        "published" => Ok(EffectiveDateSource::Published),
        "filename" => Ok(EffectiveDateSource::Filename),
        "fallback" => Ok(EffectiveDateSource::Fallback),
        other => Err(Error::engine(format!(
            "unknown effective_date_source value {other:?}"
        ))),
    }
}

/// Decode the PG `contextual_retrieval_mode` TEXT column.
fn decode_cr_mode(value: &str) -> Result<CRMode> {
    match value {
        "none" => Ok(CRMode::None),
        "title" => Ok(CRMode::Title),
        "per_chunk_synopsis" => Ok(CRMode::PerChunkSynopsis),
        other => Err(Error::engine(format!(
            "unknown contextual_retrieval_mode value {other:?}"
        ))),
    }
}

/// Parse an optional RFC3339 timestamp string into a sqlx-bindable
/// `DateTime<Utc>`. Used by `put_page` to translate engine-level
/// `Option<String>` timestamps (matching libsql's TEXT storage) into the
/// PG TIMESTAMPTZ wire type.
fn parse_rfc3339_opt(
    value: Option<&str>,
    field: &str,
) -> Result<Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>> {
    match value {
        None => Ok(None),
        Some(s) => sqlx::types::chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| Some(dt.with_timezone(&sqlx::types::chrono::Utc)))
            .map_err(|e| Error::engine(format!("{field}: invalid RFC3339 {s:?}: {e}"))),
    }
}
