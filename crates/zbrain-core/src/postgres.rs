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

use std::sync::{LazyLock, OnceLock};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use crate::engine::{
    page_sort_sql, BrainEngine, EngineConfig, EngineKind, GetPageOpts, Page, PageFilters,
    PageInput, PageSort, ResolveSlugsOpts,
};

/// Split migration SQL into individual statements for Postgres.
///
/// Postgres sqlx::query doesn't support multiple semicolon-separated statements
/// in a single query. This function splits SQL into individual statements,
/// correctly handling Postgres dollar-quoted string literals (e.g., $func$ ... $func$).
fn split_migration_sql(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote: Option<String> = None;
    let mut in_single_quote = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        
        // Handle dollar-quoted strings: $tag$ ... $tag$
        if c == '$' && !in_single_quote && !in_line_comment && !in_block_comment {
            // Collect the tag between $ signs
            let mut tag = String::new();
            while let Some(&next_c) = chars.peek() {
                if next_c == '$' {
                    chars.next();
                    current.push('$');
                    
                    // Check if we're entering or exiting a dollar quote
                    match &in_dollar_quote {
                        None => {
                            // Entering dollar quote with this tag
                            in_dollar_quote = Some(tag.clone());
                        }
                        Some(current_tag) if current_tag == &tag => {
                            // Exiting dollar quote - tag matches
                            in_dollar_quote = None;
                        }
                        _ => {
                            // Nested dollar quote with a different tag - do nothing
                        }
                    }
                    break;
                } else if next_c.is_alphanumeric() || next_c == '_' {
                    tag.push(next_c);
                    current.push(next_c);
                    chars.next();
                } else {
                    // Not a dollar quote start/end (e.g., just a single $ in the text)
                    break;
                }
            }
            continue;
        }
        
        // Handle single quotes (but not inside dollar quotes)
        if c == '\'' && in_dollar_quote.is_none() && !in_line_comment && !in_block_comment {
            in_single_quote = !in_single_quote;
            continue;
        }
        
        // Handle line comments --
        if c == '-' && !in_single_quote && in_dollar_quote.is_none() && !in_block_comment {
            if let Some(&next_c) = chars.peek() {
                if next_c == '-' {
                    in_line_comment = true;
                }
            }
        }
        if c == '\n' {
            in_line_comment = false;
        }
        
        // Handle block comments /* ... */
        if c == '/' && !in_single_quote && in_dollar_quote.is_none() && !in_line_comment {
            if let Some(&next_c) = chars.peek() {
                if next_c == '*' {
                    in_block_comment = true;
                    current.push(next_c);
                    chars.next();
                }
            }
        }
        if c == '*' && in_block_comment {
            if let Some(&next_c) = chars.peek() {
                if next_c == '/' {
                    in_block_comment = false;
                    current.push(next_c);
                    chars.next();
                }
            }
        }
        
        // Only split on semicolon if we're NOT inside any quote or comment
        if c == ';'
            && in_dollar_quote.is_none()
            && !in_single_quote
            && !in_line_comment
            && !in_block_comment
        {
            let stmt = current.trim().to_string();
            if !stmt.is_empty() {
                statements.push(stmt);
            }
            current.clear();
        }
    }
    
    // Add any remaining content (last statement without trailing semicolon)
    let remaining = current.trim();
    if !remaining.is_empty() {
        statements.push(remaining.to_string());
    }
    
    statements
}
use crate::error::{Error, Result};
use crate::migration::{Migration, MigrationRegistry};
use crate::types::FindDuplicatePageOpts;
use crate::types::OrphanPage;
use crate::types::PageRef;
use crate::types::PurgeResult;
use crate::types::RefreshPageBodyArgs;
use crate::types::{
    CRMode, DuplicatePage, EffectiveDateSource, FileRow, FileSpec, PageKind, PageVersion, RawData,
    UpsertFileResult,
};

/// Postgres-specific migration implementation. Wraps raw SQL from
/// migrations/ files and implements the Migration trait.
#[derive(Debug, Clone)]
pub struct PostgresMigration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

impl Migration for PostgresMigration {
    fn version(&self) -> i64 {
        self.version
    }

    fn name(&self) -> &str {
        self.name
    }

    fn sql(&self) -> &str {
        self.sql
    }
}

/// Embedded SQL migrations, baked into the binary at compile time.
/// Rust is now single source of truth for Postgres migrations.
const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_pages_deleted_at.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_pages_full_columns.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_pages_pg_align_ts.sql");
const MIGRATION_0005: &str = include_str!("../migrations/0005_page_tags.sql");
const MIGRATION_0006: &str = include_str!("../migrations/0006_links.sql");
const MIGRATION_0007: &str = include_str!("../migrations/0007_takes_min.sql");
const MIGRATION_0008: &str = include_str!("../migrations/0008_files.sql");
const MIGRATION_0009: &str = include_str!("../migrations/0009_raw_data_and_page_versions.sql");

/// Global migration registry for Postgres backend. Built once at runtime first use.
/// All 9 existing migrations ported with zero SQL changes.
pub static POSTGRES_MIGRATIONS: LazyLock<MigrationRegistry> = LazyLock::new(|| {
    let mut registry = MigrationRegistry::new();

    registry.add(Box::new(PostgresMigration {
        version: 1,
        name: "init",
        sql: MIGRATION_0001,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 2,
        name: "pages_full_columns",
        sql: MIGRATION_0002,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 3,
        name: "salience_and_full_generation_trigger",
        sql: MIGRATION_0003,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 4,
        name: "page_tags",
        sql: MIGRATION_0004,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 5,
        name: "takes_min",
        sql: MIGRATION_0005,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 6,
        name: "links",
        sql: MIGRATION_0006,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 7,
        name: "files",
        sql: MIGRATION_0007,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 8,
        name: "raw_data_and_page_versions",
        sql: MIGRATION_0008,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 9,
        name: "raw_data_and_page_versions_pg",
        sql: MIGRATION_0009,
    }));

    registry
});

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

    /// Read current migration version from rust_schema_version table.
    async fn read_rust_schema_version(pool: &PgPool) -> Result<i64> {
        let row = sqlx::query("SELECT version FROM rust_schema_version LIMIT 1")
            .fetch_one(pool)
            .await
            .map_err(|e| Error::engine(format!("read rust_schema_version failed: {e}")))?;
        row.try_get(0)
            .map_err(|e| Error::engine(format!("decode rust_schema_version failed: {e}")))
    }

    /// Update rust_schema_version table to the given version number.
    async fn set_rust_schema_version(pool: &PgPool, ver: i64) -> Result<()> {
        sqlx::query("UPDATE rust_schema_version SET version = $1")
            .bind(ver)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("set rust_schema_version = {ver} failed: {e}")))?;
        Ok(())
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

fn build_list_pages_sql(filters: &PageFilters) -> String {
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
    let source_ids_filter = filters.source_ids.as_ref().filter(|ids| !ids.is_empty());
    push_filter_clause(
        &mut sql,
        &mut param_idx,
        source_ids_filter.is_none() && filters.source_id.is_some(),
        "p.source_id =",
    );
    push_filter_clause(
        &mut sql,
        &mut param_idx,
        source_ids_filter.is_some(),
        "p.source_id = ANY(",
    );
    if source_ids_filter.is_some() {
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
    sql
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

        // Step 1: Bootstrap the version tracking table. Hard cutover from
        // sqlx::migrate!() era; no backward compatibility with _sqlx_migrations.
        // Split CREATE TABLE and INSERT into separate queries for Postgres compatibility
        // (Postgres doesn't support multiple semicolon-separated statements in a single query).
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS rust_schema_version (
                version BIGINT PRIMARY KEY NOT NULL DEFAULT 0,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("rust_schema_version table create failed: {e}")))?;

        sqlx::query(
            r#"
            INSERT INTO rust_schema_version (version)
            VALUES (0)
            ON CONFLICT (version) DO NOTHING;
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("rust_schema_version insert failed: {e}")))?;

        // Step 2: Read current version from the table
        let current = Self::read_rust_schema_version(pool).await?;
        let latest = POSTGRES_MIGRATIONS.latest_version();
        if current >= latest {
            return Ok(());
        }

        // Step 3: Apply all migrations in a SINGLE transaction (all-or-nothing).
        // Same pattern as libsql for symmetry per 1-2-3-4 Q2 decision.
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| Error::engine(format!("migration tx BEGIN failed: {e}")))?;

        let mut applied_any = false;
        for migration in POSTGRES_MIGRATIONS.iter() {
            let ver = migration.version();
            if ver <= current {
                continue;
            }

            applied_any = true;
            // Postgres sqlx::query doesn't support multiple semicolon-separated statements
            // in a single query. Split migration SQL into individual statements.
            for stmt in split_migration_sql(migration.sql()) {
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                sqlx::query(stmt)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        // ROLLBACK is automatic on Drop if we return before commit
                        Error::engine(format!("migration {ver} failed at statement: {e}"))
                    })?;
            }
        }

        // Version number updated once at the end (single transaction mode)
        if applied_any {
            let latest = POSTGRES_MIGRATIONS.latest_version();
            Self::set_rust_schema_version(pool, latest).await?;
        }

        tx.commit()
            .await
            .map_err(|e| Error::engine(format!("migration tx COMMIT failed: {e}")))?;

        Ok(())
    }

    // ── Page CRUD — slice 4b ──────────────────────────────────────────────
    // Hand-rolled SQL against the 4a schema. We use `sqlx::query` (not the
    // compile-time-checked `query!` macro) so the crate builds without a
    // live database at compile time — the integration suite still exercises
    // the real round-trip when `ZBRAIN_TEST_PG_URL` is set.

    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> Result<Option<Page>> {
        // - `opts.source_id = None` is an unscoped slug lookup, matching TS
        //   `getPage(slug)` semantics; explicit sources stay scoped.
        // - `(deleted_at IS NULL OR $3)` — default hides soft-deleted rows;
        //   `include_deleted=true` returns them. Mirrors libsql get_page semantics.
        // Slice #110-b: SELECT projection widened to the full 30-column shape
        //   so every `Page` field round-trips faithfully.
        let pool = self.pool()?;
        let source_id = opts.source_id.as_deref();
        let sql = format!(
            "SELECT {FULL_PAGE_PROJECTION} \
             FROM pages \
             WHERE slug = $1 \
               AND ($2::text IS NULL OR source_id = $2) \
               AND (deleted_at IS NULL OR $3) \
             ORDER BY source_id ASC \
             LIMIT 1"
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

    async fn upsert_file(&self, spec: &FileSpec) -> Result<UpsertFileResult> {
        let pool = self.pool()?;
        let source_id = spec.source_id.as_deref().unwrap_or("default");
        let metadata = spec.metadata.clone().unwrap_or_else(|| json!({}));
        let page_id = spec.page_id.map(|id| id as i64);
        let row = sqlx::query(
            "WITH existing AS ( \
                 SELECT id FROM files WHERE storage_path = $5 \
             ), upserted AS ( \
                 INSERT INTO files (source_id, page_slug, page_id, filename, storage_path, mime_type, size_bytes, content_hash, metadata) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
                 ON CONFLICT(storage_path) DO UPDATE SET \
                   source_id = EXCLUDED.source_id, \
                   page_slug = EXCLUDED.page_slug, \
                   page_id = EXCLUDED.page_id, \
                   filename = EXCLUDED.filename, \
                   mime_type = EXCLUDED.mime_type, \
                   size_bytes = EXCLUDED.size_bytes, \
                   content_hash = EXCLUDED.content_hash, \
                   metadata = EXCLUDED.metadata \
                 RETURNING id \
             ) \
             SELECT upserted.id, NOT EXISTS (SELECT 1 FROM existing) AS created \
             FROM upserted",
        )
        .bind(source_id)
        .bind(spec.page_slug.as_deref())
        .bind(page_id)
        .bind(&spec.filename)
        .bind(&spec.storage_path)
        .bind(spec.mime_type.as_deref())
        .bind(spec.size_bytes)
        .bind(&spec.content_hash)
        .bind(metadata)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("upsert_file failed: {e}")))?;
        let id: i64 = row
            .try_get("id")
            .map_err(|e| Error::engine(format!("upsert_file decode id failed: {e}")))?;
        let created: bool = row
            .try_get("created")
            .map_err(|e| Error::engine(format!("upsert_file decode created failed: {e}")))?;
        Ok(UpsertFileResult {
            id: id as u64,
            created,
        })
    }

    async fn get_file(&self, source_id: &str, storage_path: &str) -> Result<Option<FileRow>> {
        let pool = self.pool()?;
        let row = sqlx::query(
            "SELECT id, source_id, page_slug, page_id, filename, storage_path, mime_type, size_bytes, content_hash, metadata, created_at \
             FROM files \
             WHERE source_id = $1 AND storage_path = $2 \
             LIMIT 1",
        )
        .bind(source_id)
        .bind(storage_path)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("get_file query failed: {e}")))?;
        row.as_ref().map(pg_row_to_file).transpose()
    }

    async fn list_files_for_page(&self, page_id: u64) -> Result<Vec<FileRow>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT id, source_id, page_slug, page_id, filename, storage_path, mime_type, size_bytes, content_hash, metadata, created_at \
             FROM files \
             WHERE page_id = $1 \
             ORDER BY id ASC",
        )
        .bind(page_id as i64)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("list_files_for_page query failed: {e}")))?;
        rows.iter().map(pg_row_to_file).collect()
    }

    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> Result<Option<DuplicatePage>> {
        // PG mirror of libsql `find_duplicate_page` (slice 6a-pg).
        // Reverse-mapped per 14-plan §2 dialect table:
        //   - `?N`           → `$N`
        //   - `json_extract(frontmatter, '$.id')` → `frontmatter->>'id'`
        //     (PG JSONB native operator; column is JSONB on this side).
        // Behavior contract: within a single `source_id`, ignore soft-deleted
        // rows, then return the lowest-`id` row whose `content_hash` matches OR
        // (when supplied) whose `frontmatter.id` matches. Mirrors the TS return
        // shape by selecting only `id, slug`, not the full page projection.
        let pool = self.pool()?;
        let row = sqlx::query(
            "SELECT id, slug \
             FROM pages \
             WHERE source_id = $1 \
               AND deleted_at IS NULL \
               AND (content_hash = $2 \
                    OR ($3::text IS NOT NULL AND (frontmatter->>'id') = $3)) \
             ORDER BY id ASC \
             LIMIT 1",
        )
        .bind(source_id)
        .bind(&opts.content_hash)
        .bind(opts.frontmatter_id.as_deref())
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("find_duplicate_page query failed: {e}")))?;

        row.map(|row| {
            let id: i64 = row
                .try_get("id")
                .map_err(|e| Error::engine(format!("find_duplicate_page decode id: {e}")))?;
            let slug: String = row
                .try_get("slug")
                .map_err(|e| Error::engine(format!("find_duplicate_page decode slug: {e}")))?;
            Ok(DuplicatePage {
                slug,
                id: u64::try_from(id).map_err(|e| {
                    Error::engine(format!("find_duplicate_page decode id range: {e}"))
                })?,
            })
        })
        .transpose()
    }

    async fn soft_delete_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> Result<Option<String>> {
        // PG mirror of libsql `soft_delete_page` (slice 6a-pg).
        // TS source-of-truth: zbrain/src/core/pglite-engine.ts:900 —
        // `UPDATE pages SET deleted_at = now() WHERE slug = $1 AND
        //  deleted_at IS NULL AND (source_id = $X)? RETURNING slug`,
        // returning `Some(slug)` only when a live row was hit. Already-
        // soft-deleted and missing rows both return `None` (idempotent).
        //
        // Reverse-mapped per 14-plan §2:
        //   - `?N`                 → `$N`
        //   - `CURRENT_TIMESTAMP`  → `now()`
        //   - `source_id` filter normalised via the
        //     `$N::text IS NULL OR source_id = $N` R-guard so the same SQL
        //     covers both "scoped" and "any source" callers without dynamic
        //     `WHERE` stitching. Mirrors the TS conditional `where[]` push at
        //     pglite-engine.ts:903-908.
        let pool = self.pool()?;
        let row: Option<(String,)> = sqlx::query_as(
            "UPDATE pages SET deleted_at = now() \
             WHERE slug = $1 AND deleted_at IS NULL \
               AND ($2::text IS NULL OR source_id = $2) \
             RETURNING slug",
        )
        .bind(slug)
        .bind(source_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("soft_delete_page failed: {e}")))?;

        Ok(row.map(|(s,)| s))
    }

    async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> Result<bool> {
        // PG mirror of libsql `restore_page` (slice 6a-pg).
        // TS source-of-truth: zbrain/src/core/pglite-engine.ts:918 —
        // `UPDATE pages SET deleted_at = NULL WHERE slug = $1 AND
        //  deleted_at IS NOT NULL AND (source_id = $X)? RETURNING slug`,
        // returning `true` iff a soft-deleted row was matched. Live and
        // missing rows return `false` — no row count diff is exposed.
        //
        // Same R-guard pattern as `soft_delete_page`; the optional
        // `source_id` collapses into a single SQL string.
        let pool = self.pool()?;
        let row: Option<(String,)> = sqlx::query_as(
            "UPDATE pages SET deleted_at = NULL \
             WHERE slug = $1 AND deleted_at IS NOT NULL \
               AND ($2::text IS NULL OR source_id = $2) \
             RETURNING slug",
        )
        .bind(slug)
        .bind(source_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("restore_page failed: {e}")))?;

        Ok(row.is_some())
    }

    async fn purge_deleted_pages(&self, older_than_hours: u32) -> Result<PurgeResult> {
        // PG mirror of libsql `purge_deleted_pages` (slice 6a-pg).
        // TS source-of-truth: zbrain/src/core/pglite-engine.ts:933 —
        // `DELETE FROM pages WHERE deleted_at IS NOT NULL AND
        //  deleted_at < now() - ($1 || ' hours')::interval RETURNING slug`,
        // bundling the returned slugs plus a count into `{ slugs, count }`.
        // FK cascades (chunks / links) follow the `pages` row drop, matching
        // TS schema constraints.
        //
        // TS clamp `Math.max(0, Math.floor(olderThanHours))` is encoded in
        // the Rust signature via `u32` (no negatives, integral). The string
        // concatenation form is preferred over `make_interval(hours => $1)`
        // because TS uses the same shape — keeps both backends auditable
        // against one SQL template.
        let pool = self.pool()?;
        let rows: Vec<(String,)> = sqlx::query_as(
            "DELETE FROM pages \
             WHERE deleted_at IS NOT NULL \
               AND deleted_at < now() - ($1 || ' hours')::interval \
             RETURNING slug",
        )
        .bind(i64::from(older_than_hours).to_string())
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("purge_deleted_pages failed: {e}")))?;

        let slugs: Vec<String> = rows.into_iter().map(|(s,)| s).collect();
        let count = slugs.len() as u64;
        Ok(PurgeResult { slugs, count })
    }

    async fn delete_page(&self, slug: &str, source_id: Option<&str>) -> Result<()> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");
        // No-op on missing slug/source pair, matching the TS engine +
        // InMemoryEngine contract. `DELETE` returns affected-row count which
        // we ignore on purpose — callers that want a "did it exist" probe
        // should `get_page` first.
        sqlx::query("DELETE FROM pages WHERE slug = $1 AND source_id = $2")
            .bind(slug)
            .bind(source_id)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("delete_page failed: {e}")))?;
        Ok(())
    }

    async fn list_pages(&self, filters: &PageFilters) -> Result<Vec<Page>> {
        let pool = self.pool()?;
        let sql = build_list_pages_sql(filters);
        let mut query = sqlx::query(&sql);
        let source_ids_filter = filters.source_ids.as_ref().filter(|ids| !ids.is_empty());

        // ORDER CONTRACT: bind order must match `param_idx` advancement in
        // `build_list_pages_sql`: page_type → source_ids/source_id precedence →
        // slug_prefix → updated_after → tag → limit → offset. Reordering either
        // side silently misbinds PG `$N`.
        if let Some(pt) = filters.page_type.as_deref() {
            query = query.bind(pt);
        }
        if let Some(ids) = source_ids_filter {
            query = query.bind(ids.as_slice());
        } else if let Some(sid) = filters.source_id.as_deref() {
            query = query.bind(sid);
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

    async fn resolve_slugs(&self, partial: &str, opts: &ResolveSlugsOpts) -> Result<Vec<String>> {
        let pool = self.pool()?;
        let exact_rows = match opts.source_ids.as_ref().filter(|ids| !ids.is_empty()) {
            Some(source_ids) => {
                sqlx::query(
                    "SELECT slug FROM pages \
                     WHERE slug = $1 AND deleted_at IS NULL AND source_id = ANY($2::text[]) \
                     ORDER BY slug ASC",
                )
                .bind(partial)
                .bind(source_ids)
                .fetch_all(pool)
                .await
            }
            None => match opts.source_id.as_ref() {
                Some(source_id) => {
                    sqlx::query(
                        "SELECT slug FROM pages \
                         WHERE slug = $1 AND deleted_at IS NULL AND source_id = $2 \
                         ORDER BY slug ASC",
                    )
                    .bind(partial)
                    .bind(source_id)
                    .fetch_all(pool)
                    .await
                }
                None => {
                    sqlx::query(
                        "SELECT slug FROM pages \
                         WHERE slug = $1 AND deleted_at IS NULL \
                         ORDER BY slug ASC",
                    )
                    .bind(partial)
                    .fetch_all(pool)
                    .await
                }
            },
        }
        .map_err(|e| Error::engine(format!("resolve_slugs exact query failed: {e}")))?;
        let exact = collect_pg_slug_rows(exact_rows)?;
        if !exact.is_empty() {
            return Ok(exact);
        }

        let like = format!("%{partial}%");
        let fuzzy_rows = match opts.source_ids.as_ref().filter(|ids| !ids.is_empty()) {
            Some(source_ids) => {
                sqlx::query(
                    "SELECT slug FROM pages \
                     WHERE deleted_at IS NULL AND slug ILIKE $1 AND source_id = ANY($2::text[]) \
                     ORDER BY slug ASC \
                     LIMIT 5",
                )
                .bind(&like)
                .bind(source_ids)
                .fetch_all(pool)
                .await
            }
            None => match opts.source_id.as_ref() {
                Some(source_id) => {
                    sqlx::query(
                        "SELECT slug FROM pages \
                         WHERE deleted_at IS NULL AND slug ILIKE $1 AND source_id = $2 \
                         ORDER BY slug ASC \
                         LIMIT 5",
                    )
                    .bind(&like)
                    .bind(source_id)
                    .fetch_all(pool)
                    .await
                }
                None => {
                    sqlx::query(
                        "SELECT slug FROM pages \
                         WHERE deleted_at IS NULL AND slug ILIKE $1 \
                         ORDER BY slug ASC \
                         LIMIT 5",
                    )
                    .bind(&like)
                    .fetch_all(pool)
                    .await
                }
            },
        }
        .map_err(|e| Error::engine(format!("resolve_slugs fuzzy query failed: {e}")))?;
        collect_pg_slug_rows(fuzzy_rows)
    }

    // ─── PG-advanced-writes overrides ────────────────────────────────────────

    /// Narrow body refresh for one live `(source_id, slug)` row. Mirrors TS
    /// `refreshPageBody`: soft-deleted rows are skipped and missing rows no-op.
    async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> Result<()> {
        let pool = self.pool()?;
        let timeline = args.timeline.to_string();

        sqlx::query(
            "UPDATE pages \
             SET compiled_truth = $1, \
                 timeline = $2, \
                 content_hash = $3, \
                 updated_at = NOW() \
             WHERE source_id = $4 \
               AND slug = $5 \
               AND deleted_at IS NULL",
        )
        .bind(&args.compiled_truth)
        .bind(timeline)
        .bind(&args.content_hash)
        .bind(&args.source_id)
        .bind(&args.slug)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("refresh_page_body failed: {e}")))?;

        Ok(())
    }

    /// Narrow CR-state refresh for one live `(source_id, slug)` row. Mirrors TS
    /// `updatePageContextualRetrievalState`: soft-deleted rows are skipped and
    /// missing rows no-op.
    async fn update_page_contextual_retrieval_state(
        &self,
        slug: &str,
        source_id: &str,
        mode: &str,
        corpus_generation: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool()?;

        sqlx::query(
            "UPDATE pages \
             SET contextual_retrieval_mode = $1, \
                 corpus_generation = $2, \
                 updated_at = NOW() \
             WHERE source_id = $3 \
               AND slug = $4 \
               AND deleted_at IS NULL",
        )
        .bind(mode)
        .bind(corpus_generation)
        .bind(source_id)
        .bind(slug)
        .execute(pool)
        .await
        .map_err(|e| {
            Error::engine(format!(
                "update_page_contextual_retrieval_state failed: {e}"
            ))
        })?;

        Ok(())
    }

    // ─── Raw data / versions / slug rewrite (7) ─────────────────────────────
    // Slice #22: full Postgres behavior replacing the #19 unimplemented!() stubs.
    // Migration 0009_raw_data_and_page_versions.sql is applied by sqlx::migrate!.

    async fn put_raw_data(
        &self,
        slug: &str,
        source: &str,
        data: &Value,
        source_id: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");

        // Resolve page_id first.
        let page_row = sqlx::query(
            "SELECT id FROM pages WHERE slug = $1 AND source_id = $2 AND deleted_at IS NULL",
        )
        .bind(slug)
        .bind(source_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("put_raw_data page lookup failed: {e}")))?;
        let Some(page_row) = page_row else {
            return Err(Error::page_not_found(slug, Some(source_id)));
        };
        let page_id: i64 = page_row
            .try_get("id")
            .map_err(|e| Error::engine(format!("put_raw_data decode page_id: {e}")))?;

        sqlx::query(
            "INSERT INTO raw_data (page_id, source, data) \
             VALUES ($1, $2, $3) \
             ON CONFLICT(page_id, source) DO UPDATE SET \
               data = EXCLUDED.data, \
               fetched_at = now()",
        )
        .bind(page_id)
        .bind(source)
        .bind(data)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("put_raw_data upsert failed: {e}")))?;

        Ok(())
    }

    async fn get_raw_data(
        &self,
        slug: &str,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> Result<Vec<RawData>> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");

        // Resolve page_id.
        let page_row = sqlx::query("SELECT id FROM pages WHERE slug = $1 AND source_id = $2")
            .bind(slug)
            .bind(source_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::engine(format!("get_raw_data page lookup failed: {e}")))?;
        let Some(page_row) = page_row else {
            return Ok(vec![]);
        };
        let page_id: i64 = page_row
            .try_get("id")
            .map_err(|e| Error::engine(format!("get_raw_data decode page_id: {e}")))?;

        let rows = match source {
            Some(s) => {
                sqlx::query(
                    "SELECT source, data, fetched_at \
                     FROM raw_data \
                     WHERE page_id = $1 AND source = $2",
                )
                .bind(page_id)
                .bind(s)
                .fetch_all(pool)
                .await
            }
            None => {
                sqlx::query(
                    "SELECT source, data, fetched_at \
                     FROM raw_data \
                     WHERE page_id = $1",
                )
                .bind(page_id)
                .fetch_all(pool)
                .await
            }
        }
        .map_err(|e| Error::engine(format!("get_raw_data query failed: {e}")))?;

        let mut results = Vec::new();
        for row in &rows {
            let src: String = row
                .try_get("source")
                .map_err(|e| Error::engine(format!("get_raw_data decode source: {e}")))?;
            let data: Value = row
                .try_get("data")
                .map_err(|e| Error::engine(format!("get_raw_data decode data: {e}")))?;
            let fetched_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = row
                .try_get("fetched_at")
                .map_err(|e| Error::engine(format!("get_raw_data decode fetched_at: {e}")))?;
            results.push(RawData {
                source: src,
                data,
                fetched_at: fetched_at.to_rfc3339(),
            });
        }
        Ok(results)
    }

    async fn create_version(&self, slug: &str, source_id: Option<&str>) -> Result<PageVersion> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");

        // Snapshot current page state.
        let page_row = sqlx::query(
            "SELECT id, compiled_truth, frontmatter \
             FROM pages \
             WHERE slug = $1 AND source_id = $2 AND deleted_at IS NULL",
        )
        .bind(slug)
        .bind(source_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("create_version page lookup failed: {e}")))?
        .ok_or_else(|| Error::page_not_found(slug, Some(source_id)))?;
        let page_id: i64 = page_row
            .try_get("id")
            .map_err(|e| Error::engine(format!("create_version decode page_id: {e}")))?;
        let compiled_truth: String = page_row
            .try_get("compiled_truth")
            .map_err(|e| Error::engine(format!("create_version decode compiled_truth: {e}")))?;
        let frontmatter: Value = page_row
            .try_get("frontmatter")
            .map_err(|e| Error::engine(format!("create_version decode frontmatter: {e}")))?;

        let row = sqlx::query(
            "INSERT INTO page_versions (page_id, compiled_truth, frontmatter) \
             VALUES ($1, $2, $3) \
             RETURNING id, page_id, compiled_truth, frontmatter, snapshot_at",
        )
        .bind(page_id)
        .bind(&compiled_truth)
        .bind(&frontmatter)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("create_version insert failed: {e}")))?;

        let id: i64 = row
            .try_get("id")
            .map_err(|e| Error::engine(format!("create_version decode id: {e}")))?;
        let returned_page_id: i64 = row
            .try_get("page_id")
            .map_err(|e| Error::engine(format!("create_version decode page_id: {e}")))?;
        let returned_truth: String = row
            .try_get("compiled_truth")
            .map_err(|e| Error::engine(format!("create_version decode truth: {e}")))?;
        let returned_fm: Value = row
            .try_get("frontmatter")
            .map_err(|e| Error::engine(format!("create_version decode frontmatter: {e}")))?;
        let snapshot_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = row
            .try_get("snapshot_at")
            .map_err(|e| Error::engine(format!("create_version decode snapshot_at: {e}")))?;

        Ok(PageVersion {
            id: id as u64,
            page_id: returned_page_id as u64,
            compiled_truth: returned_truth,
            frontmatter: returned_fm,
            snapshot_at: snapshot_at.to_rfc3339(),
        })
    }

    async fn get_versions(&self, slug: &str, source_id: Option<&str>) -> Result<Vec<PageVersion>> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");

        // Resolve page_id via subquery.
        let rows = sqlx::query(
            "SELECT pv.id, pv.page_id, pv.compiled_truth, pv.frontmatter, pv.snapshot_at \
             FROM page_versions pv \
             JOIN pages p ON p.id = pv.page_id \
             WHERE p.slug = $1 AND p.source_id = $2 \
             ORDER BY pv.snapshot_at DESC, pv.id DESC",
        )
        .bind(slug)
        .bind(source_id)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_versions query failed: {e}")))?;

        let mut results = Vec::new();
        for row in &rows {
            let id: i64 = row
                .try_get("id")
                .map_err(|e| Error::engine(format!("get_versions decode id: {e}")))?;
            let page_id: i64 = row
                .try_get("page_id")
                .map_err(|e| Error::engine(format!("get_versions decode page_id: {e}")))?;
            let compiled_truth: String = row
                .try_get("compiled_truth")
                .map_err(|e| Error::engine(format!("get_versions decode truth: {e}")))?;
            let frontmatter: Value = row
                .try_get("frontmatter")
                .map_err(|e| Error::engine(format!("get_versions decode frontmatter: {e}")))?;
            let snapshot_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = row
                .try_get("snapshot_at")
                .map_err(|e| Error::engine(format!("get_versions decode snapshot_at: {e}")))?;
            results.push(PageVersion {
                id: id as u64,
                page_id: page_id as u64,
                compiled_truth,
                frontmatter,
                snapshot_at: snapshot_at.to_rfc3339(),
            });
        }
        Ok(results)
    }

    async fn revert_to_version(
        &self,
        slug: &str,
        version_id: u64,
        source_id: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");

        // Read the version snapshot.
        let ver_row =
            sqlx::query("SELECT compiled_truth, frontmatter FROM page_versions WHERE id = $1")
                .bind(version_id as i64)
                .fetch_optional(pool)
                .await
                .map_err(|e| Error::engine(format!("revert_to_version lookup failed: {e}")))?;
        let Some(ver_row) = ver_row else {
            return Err(Error::engine(format!("version {version_id} not found")));
        };
        let compiled_truth: String = ver_row
            .try_get("compiled_truth")
            .map_err(|e| Error::engine(format!("revert_to_version decode truth: {e}")))?;
        let frontmatter: Value = ver_row
            .try_get("frontmatter")
            .map_err(|e| Error::engine(format!("revert_to_version decode frontmatter: {e}")))?;

        let affected = sqlx::query(
            "UPDATE pages SET compiled_truth = $1, frontmatter = $2, updated_at = now() \
             WHERE slug = $3 AND source_id = $4 AND deleted_at IS NULL",
        )
        .bind(&compiled_truth)
        .bind(&frontmatter)
        .bind(slug)
        .bind(source_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("revert_to_version update failed: {e}")))?;

        if affected.rows_affected() == 0 {
            return Err(Error::page_not_found(slug, Some(source_id)));
        }
        Ok(())
    }

    async fn update_slug(
        &self,
        old_slug: &str,
        new_slug: &str,
        source_id: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool()?;
        let source_id = source_id.unwrap_or("default");

        // Conflict check.
        let conflict = sqlx::query("SELECT 1 FROM pages WHERE slug = $1 AND source_id = $2")
            .bind(new_slug)
            .bind(source_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::engine(format!("update_slug conflict check failed: {e}")))?;
        if conflict.is_some() {
            return Err(Error::engine(format!(
                "slug '{new_slug}' already exists in source '{source_id}'"
            )));
        }

        let affected = sqlx::query(
            "UPDATE pages SET slug = $1, updated_at = now() \
             WHERE slug = $2 AND source_id = $3 AND deleted_at IS NULL",
        )
        .bind(new_slug)
        .bind(old_slug)
        .bind(source_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("update_slug failed: {e}")))?;

        if affected.rows_affected() == 0 {
            return Err(Error::page_not_found(old_slug, Some(source_id)));
        }
        Ok(())
    }

    /// Explicit no-op — Postgres page rows use integer `page_id` foreign keys
    /// so there are no embedded slug strings to rewrite. Returns `Ok(())`.
    async fn rewrite_links(&self, _old_slug: &str, _new_slug: &str) -> Result<()> {
        Ok(())
    }

    // ─── Advanced reads overrides (PG parity) ────────────────────────────────
    // Five read-only methods promoted from trait-default `Unsupported` to
    // real SQL on PostgresEngine. libsql implements the same trait surface in
    // the 6a-libsql advanced reads slice. Contracts: see
    // docs/plans/20260526/14-slice-6a-pg-plan.md §11.1.

    /// Return all live slugs, optionally scoped to `source_id`.
    /// §11.1 R1: does NOT filter `deleted_at` (mirrors TS `getAllSlugs`).
    async fn get_all_slugs(
        &self,
        source_id: Option<&str>,
    ) -> Result<std::collections::HashSet<String>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT slug FROM pages \
             WHERE $1::text IS NULL OR source_id = $1",
        )
        .bind(source_id)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_all_slugs failed: {e}")))?;

        rows.into_iter()
            .map(|r| {
                r.try_get::<String, _>("slug")
                    .map_err(|e| Error::engine(format!("get_all_slugs decode failed: {e}")))
            })
            .collect()
    }

    /// Return every live `(slug, source_id)` pair, ordered by
    /// `(source_id, slug)` ascending. Mirrors TS `listAllPageRefs`.
    async fn list_all_page_refs(&self) -> Result<Vec<PageRef>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT slug, source_id FROM pages \
             WHERE deleted_at IS NULL \
             ORDER BY source_id ASC, slug ASC",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("list_all_page_refs failed: {e}")))?;

        rows.into_iter()
            .map(|r| {
                let slug = r
                    .try_get::<String, _>("slug")
                    .map_err(|e| Error::engine(format!("list_all_page_refs decode slug: {e}")))?;
                let source_id = r.try_get::<String, _>("source_id").map_err(|e| {
                    Error::engine(format!("list_all_page_refs decode source_id: {e}"))
                })?;
                Ok(PageRef { slug, source_id })
            })
            .collect()
    }

    /// Resolve `slug` → `COALESCE(updated_at, created_at)` for many slugs.
    /// Missing slugs are omitted. Mirrors TS `getPageTimestamps`, including
    /// its deleted-row visibility: the TS query does not filter `deleted_at`.
    async fn get_page_timestamps(
        &self,
        slugs: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT slug, COALESCE(updated_at, created_at)::text AS ts \
             FROM pages \
             WHERE slug = ANY($1::text[])",
        )
        .bind(slugs)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_page_timestamps failed: {e}")))?;

        let mut out = std::collections::HashMap::with_capacity(rows.len());
        for r in rows {
            let slug = r
                .try_get::<String, _>("slug")
                .map_err(|e| Error::engine(format!("get_page_timestamps decode slug: {e}")))?;
            let ts = r
                .try_get::<String, _>("ts")
                .map_err(|e| Error::engine(format!("get_page_timestamps decode ts: {e}")))?;
            out.insert(slug, ts);
        }
        Ok(out)
    }

    /// Resolve `(slug, source_id)` → `COALESCE(effective_date, updated_at, created_at)`.
    /// Key format: `"{source_id}::{slug}"`. Mirrors TS `getEffectiveDates`.
    async fn get_effective_dates(
        &self,
        refs: &[PageRef],
    ) -> Result<std::collections::HashMap<String, String>> {
        let pool = self.pool()?;
        let slugs: Vec<String> = refs.iter().map(|r| r.slug.clone()).collect();
        let source_ids: Vec<String> = refs.iter().map(|r| r.source_id.clone()).collect();

        let rows = sqlx::query(
            "SELECT p.slug, p.source_id, \
                    COALESCE(p.effective_date, p.updated_at::text, p.created_at::text) AS ts \
             FROM pages p \
             JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id) \
               ON p.slug = u.slug AND p.source_id = u.source_id \
             WHERE p.deleted_at IS NULL",
        )
        .bind(slugs.as_slice())
        .bind(source_ids.as_slice())
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_effective_dates failed: {e}")))?;

        let mut out = std::collections::HashMap::with_capacity(rows.len());
        for r in rows {
            let slug = r
                .try_get::<String, _>("slug")
                .map_err(|e| Error::engine(format!("get_effective_dates decode slug: {e}")))?;
            let source_id = r
                .try_get::<String, _>("source_id")
                .map_err(|e| Error::engine(format!("get_effective_dates decode source_id: {e}")))?;
            let ts = r
                .try_get::<String, _>("ts")
                .map_err(|e| Error::engine(format!("get_effective_dates decode ts: {e}")))?;
            out.insert(format!("{source_id}::{slug}"), ts);
        }
        Ok(out)
    }

    /// Compute salience scores. §11.1 R5 + slice 6c-takes-salience.
    ///
    /// Full TS formula (mirroring `pglite-engine.ts` L2596-2617):
    ///
    /// ```text
    /// score = COALESCE(p.emotional_weight, 0) * 5
    ///       + ln(1 + COUNT(DISTINCT t.id) WHERE t.active = TRUE)
    /// ```
    ///
    /// The `takes` table was added by migration 0007 (minimal 3-col subset:
    /// `id` / `page_id` / `active`). `LEFT JOIN` preserves pages with zero active
    /// takes (COUNT = 0 → ln(1+0) = 0).
    async fn get_salience_scores(
        &self,
        refs: &[PageRef],
    ) -> Result<std::collections::HashMap<String, f64>> {
        let pool = self.pool()?;
        let slugs: Vec<String> = refs.iter().map(|r| r.slug.clone()).collect();
        let source_ids: Vec<String> = refs.iter().map(|r| r.source_id.clone()).collect();

        let rows = sqlx::query(
            "SELECT p.slug, p.source_id, \
                    COALESCE(p.emotional_weight, 0.0) * 5.0 \
                    + ln(1 + COUNT(DISTINCT t.id)) AS score \
             FROM pages p \
             LEFT JOIN takes t ON t.page_id = p.id AND t.active = TRUE \
             JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id) \
               ON p.slug = u.slug AND p.source_id = u.source_id \
             WHERE p.deleted_at IS NULL \
             GROUP BY p.id, p.slug, p.source_id, p.emotional_weight",
        )
        .bind(slugs.as_slice())
        .bind(source_ids.as_slice())
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_salience_scores failed: {e}")))?;

        let mut out = std::collections::HashMap::with_capacity(rows.len());
        for r in rows {
            let slug = r
                .try_get::<String, _>("slug")
                .map_err(|e| Error::engine(format!("get_salience_scores decode slug: {e}")))?;
            let source_id = r
                .try_get::<String, _>("source_id")
                .map_err(|e| Error::engine(format!("get_salience_scores decode source_id: {e}")))?;
            let score = r
                .try_get::<f64, _>("score")
                .map_err(|e| Error::engine(format!("get_salience_scores decode score: {e}")))?;
            out.insert(format!("{source_id}::{slug}"), score);
        }
        Ok(out)
    }

    /// `find_orphan_pages` — return live pages that have no live inbound
    /// links. Mirrors TS `pglite-engine.ts` `findOrphanPages` (v0.26.5):
    ///
    /// * candidate side: `pages.deleted_at IS NULL`
    /// * inbound source side: `pages.deleted_at IS NULL` (C11 — links
    ///   originating from soft-deleted pages do NOT count as inbound)
    /// * title: `COALESCE(title, slug)` — defensive only. TS schema declares
    ///   `title TEXT NOT NULL` and `putPage` binds `page.title` verbatim, so
    ///   empty titles remain empty strings rather than falling back to slug.
    ///   The COALESCE guards against any future NULL drift.
    /// * domain: `frontmatter->>'domain'` extracted as text, `None` when
    ///   the JSON key is absent
    /// * order: `ORDER BY p.slug` for deterministic output
    async fn find_orphan_pages(&self) -> Result<Vec<OrphanPage>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT p.slug, COALESCE(p.title, p.slug) AS title, \
                    p.frontmatter->>'domain' AS domain \
             FROM pages p \
             WHERE p.deleted_at IS NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM links l \
                 JOIN pages src ON src.id = l.from_page_id \
                 WHERE l.to_page_id = p.id AND src.deleted_at IS NULL \
               ) \
             ORDER BY p.slug",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("find_orphan_pages failed: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let slug: String = r
                .try_get("slug")
                .map_err(|e| Error::engine(format!("find_orphan_pages decode slug: {e}")))?;
            let title: String = r
                .try_get("title")
                .map_err(|e| Error::engine(format!("find_orphan_pages decode title: {e}")))?;
            let domain: Option<String> = r
                .try_get("domain")
                .map_err(|e| Error::engine(format!("find_orphan_pages decode domain: {e}")))?;
            out.push(OrphanPage {
                slug,
                title,
                domain,
            });
        }
        Ok(out)
    }
}

fn pg_row_to_file(row: &sqlx::postgres::PgRow) -> Result<FileRow> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| Error::engine(format!("file row decode id: {e}")))?;
    let page_id: Option<i64> = row
        .try_get("page_id")
        .map_err(|e| Error::engine(format!("file row decode page_id: {e}")))?;
    Ok(FileRow {
        id: id as u64,
        source_id: row
            .try_get("source_id")
            .map_err(|e| Error::engine(format!("file row decode source_id: {e}")))?,
        page_slug: row
            .try_get("page_slug")
            .map_err(|e| Error::engine(format!("file row decode page_slug: {e}")))?,
        page_id: page_id.map(|value| value as u64),
        filename: row
            .try_get("filename")
            .map_err(|e| Error::engine(format!("file row decode filename: {e}")))?,
        storage_path: row
            .try_get("storage_path")
            .map_err(|e| Error::engine(format!("file row decode storage_path: {e}")))?,
        mime_type: row
            .try_get("mime_type")
            .map_err(|e| Error::engine(format!("file row decode mime_type: {e}")))?,
        size_bytes: row
            .try_get("size_bytes")
            .map_err(|e| Error::engine(format!("file row decode size_bytes: {e}")))?,
        content_hash: row
            .try_get("content_hash")
            .map_err(|e| Error::engine(format!("file row decode content_hash: {e}")))?,
        metadata: row
            .try_get::<Value, _>("metadata")
            .map_err(|e| Error::engine(format!("file row decode metadata: {e}")))?,
        created_at: row
            .try_get::<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>, _>("created_at")
            .map_err(|e| Error::engine(format!("file row decode created_at: {e}")))?
            .to_rfc3339(),
    })
}

fn collect_pg_slug_rows(rows: Vec<sqlx::postgres::PgRow>) -> Result<Vec<String>> {
    rows.into_iter()
        .map(|r| {
            r.try_get::<String, _>("slug")
                .map_err(|e| Error::engine(format!("resolve_slugs decode failed: {e}")))
        })
        .collect()
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
