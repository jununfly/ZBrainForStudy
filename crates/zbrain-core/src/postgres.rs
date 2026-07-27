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
//!   - `put_page` writes 20 columns. `embedding` (BYTEA, f32-LE) is written
//!     as of G24 (page-level vector write path) and COALESCE-preserved on
//!     upsert; `get_page`/`list_pages`/`search_pages` project it back.
//!     `last_retrieved_at` is still owned by the retrieval-tracker path;
//!     `put_page` never writes it and `get_page` returns `None` for it.
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
use sqlx::{QueryBuilder, PgPool, Row};

use crate::engine::{
    page_sort_sql, BrainEngine, CreateSourceInput, EngineConfig, EngineKind, GetPageOpts, Page,
    PageFilters, PageInput, PageSort, ResolveSlugsOpts, SearchOpts, SearchResult, SourceRow,
    UpdateSourceInput, fuse_and_boost, is_valid_source_id,
};
use crate::calibration_queries::{
    aggregate_calibration_curve, aggregate_scorecard, CalibrationBucket, CalibrationCurveQuery,
    CalibrationProfileRow, CalibrationQueries, CalibrationRow, PatternDetail, ScorecardQuery,
    ScorecardRow, TakesScorecard,
};
use crate::oauth_queries::{
    ExchangeTokens, OAuthClientInfo, OAuthQueries, RegisterClientRequest,
    RegisterClientResponse, RevokeClientResponse, UpdateClientTtlResponse,
};
use crate::scope::{has_scope, parse_scope_string};
use crate::token_queries::{AuthInfo, TokenError, TokenQueries};

#[derive(Debug, sqlx::FromRow)]
struct CalibrationProfileRowDb {
    id: i64,
    source_id: String,
    holder: String,
    wave_version: String,
    generated_at: String,
    published: bool,
    total_resolved: i32,
    brier: Option<f64>,
    accuracy: Option<f64>,
    partial_rate: Option<f64>,
    grade_completion: f64,
    domain_scorecards: serde_json::Value,
    pattern_statements: Vec<String>,
    voice_gate_passed: bool,
    voice_gate_attempts: i32,
    active_bias_tags: Vec<String>,
    model_id: String,
    cost_usd: Option<f64>,
    judge_model_agreement: Option<f64>,
}

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
use crate::time::current_utc_iso8601;
use crate::types::{
    AdjacencyRow, CRMode, DuplicatePage, EffectiveDateSource, EntityCount, FactInsertStatus,
    FactKind, FactListOpts, FactRow, FactVisibility, FactsHealth, FileRow, FileSpec, GraphPath, Link,
    LinkBatchInput, NewFact, PageKind, PageVersion, RawData, SearchTakesOpts, Take,
    TakeHit, TakeInput, TakesListOpts, UpsertFileResult, UpsertTakesResult,
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
const MIGRATION_0010: &str = include_str!("../migrations/0010_oauth_tables.sql");
const MIGRATION_0011: &str = include_str!("../migrations/0011_sources_full_columns.sql");
const MIGRATION_0012: &str = include_str!("../migrations/0012_takes_full_columns.sql");
const MIGRATION_0013: &str = include_str!("../migrations/0013_facts.sql");
const MIGRATION_0014: &str = include_str!("../migrations/0014_minion_jobs.sql");
const MIGRATION_0015: &str = include_str!("../migrations/0015_minion_inbox.sql");
const MIGRATION_0016: &str = include_str!("../migrations/0016_minion_attachments.sql");
const MIGRATION_0017: &str = include_str!("../migrations/0017_minion_budget.sql");
const MIGRATION_0018: &str = include_str!("../migrations/0018_rate_leases.sql");
/// 1-6-7-5: content chunks read side for the `get_chunks` op.
const MIGRATION_0019: &str = include_str!("../migrations/0019_content_chunks.sql");
/// 1-6-7-5: ingest log for the `log_ingest` / `get_ingest_log` ops.
const MIGRATION_0020: &str = include_str!("../migrations/0020_ingest_log.sql");
/// 1-6-7-10-1: code-graph edge storage (write side for code-intel ops).
const MIGRATION_0021: &str = include_str!("../migrations/0021_code_edges.sql");
/// 1-6-7-11: search_by_image — image-search spend log table for daily budget tracking.
const MIGRATION_0022: &str = include_str!("../migrations/0022_image_search_spend_log.sql");

/// FNV-1a 64-bit hash of a lease key, mapped to a signed int64 for
/// `pg_advisory_xact_lock`. Matches the TS implementation bit-for-bit.
fn fnv1a_64(key: &str) -> i64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & 0x7fff_ffff_ffff_ffff) as i64
}

/// Global migration registry for Postgres backend. Built once at runtime first use.
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
    registry.add(Box::new(PostgresMigration {
        version: 10,
        name: "oauth_tables",
        sql: MIGRATION_0010,
    }));

    registry.add(Box::new(PostgresMigration {
        version: 11,
        name: "sources_full_columns",
        sql: MIGRATION_0011,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 12,
        name: "takes_full_columns",
        sql: MIGRATION_0012,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 13,
        name: "facts",
        sql: MIGRATION_0013,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 14,
        name: "minion_jobs",
        sql: MIGRATION_0014,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 15,
        name: "minion_inbox",
        sql: MIGRATION_0015,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 16,
        name: "minion_attachments",
        sql: MIGRATION_0016,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 17,
        name: "minion_budget",
        sql: MIGRATION_0017,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 18,
        name: "rate_leases",
        sql: MIGRATION_0018,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 19,
        name: "content_chunks",
        sql: MIGRATION_0019,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 20,
        name: "ingest_log",
        sql: MIGRATION_0020,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 21,
        name: "code_edges",
        sql: MIGRATION_0021,
    }));
    registry.add(Box::new(PostgresMigration {
        version: 22,
        name: "mcp_spend_log",
        sql: MIGRATION_0022,
    }));

    registry
});

/// Full 29-column projection used by every read path (`get_page`,
/// `list_pages`, and the `RETURNING` clause of `put_page`). Centralised so
/// `row_to_page` and SQL stay in lock-step.
///
/// `embedding` (BYTEA, f32-LE blob) is included as of G24: `put_page` now has
/// a write path for it, so read paths must project it back. `last_retrieved_at`
/// remains intentionally absent — it is owned by the retrieval-tracker code
/// path and `get_page` always reports `None` for it.
const FULL_PAGE_PROJECTION: &str = "id, slug, type, page_kind, title, compiled_truth, timeline, \
     frontmatter, content_hash, emotional_weight, created_at, updated_at, deleted_at, \
     effective_date, effective_date_source, import_filename, \
     salience_touched_at, salience_score, generation, chunker_version, \
     source_path, source_id, source_kind, source_uri, ingested_via, ingested_at, \
     contextual_retrieval_mode, corpus_generation, embedding";

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

    async fn get_source_by_github_repo(
        &self,
        github_repo: &str,
    ) -> Result<Option<SourceRow>> {
        let pool = self.pool()?;
        let row = sqlx::query_as::<_, (String, String, serde_json::Value)>(
            "SELECT id, name, config FROM sources WHERE config->>'github_repo' = $1 LIMIT 1"
        )
        .bind(github_repo)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("source lookup failed: {e}")))?;
        Ok(row.map(|(id, name, config)| SourceRow {
            id,
            name,
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config,
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        }))
    }

    async fn list_sources(&self, _include_archived: bool) -> Result<Vec<SourceRow>> {
        let pool = self.pool()?;
        let rows = sqlx::query_as::<_, (String, String, serde_json::Value)>(
            "SELECT id, name, config FROM sources"
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("list_sources failed: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|(id, name, config)| SourceRow {
                id,
                name,
                local_path: None,
                last_commit: None,
                last_sync_at: None,
                config,
                created_at: None,
                chunker_version: None,
                archived: false,
                archived_at: None,
                archive_expires_at: None,
                contextual_retrieval_mode: None,
                trust_frontmatter_overrides: false,
            })
            .collect())
    }

    async fn get_source(&self, id: &str) -> Result<Option<SourceRow>> {
        let pool = self.pool()?;
        let row = sqlx::query_as::<_, (String, String, serde_json::Value)>(
            "SELECT id, name, config FROM sources WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("get_source failed: {e}")))?;
        Ok(row.map(|(id, name, config)| SourceRow {
            id,
            name,
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config,
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
                trust_frontmatter_overrides: false,
        }))
    }

    async fn source_sync_stats(
        &self,
    ) -> Result<Vec<crate::sync_status::SourceSyncStat>> {
        let pool = self.pool()?;

        let src_rows = sqlx::query_as::<_, (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            serde_json::Value,
        )>(
            "SELECT id, name, local_path, last_commit, last_sync_at, config FROM sources",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("source_sync_stats sources failed: {e}")))?;

        let page_rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT source_id, COUNT(*) AS pages FROM pages WHERE deleted_at IS NULL GROUP BY source_id",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("source_sync_stats pages failed: {e}")))?;
        let page_counts: std::collections::HashMap<String, u64> = page_rows
            .into_iter()
            .map(|(sid, p)| (sid, p.max(0) as u64))
            .collect();

        // Per-source chunk counts + unembedded (exclude deleted pages, mirroring
        // TS buildSyncStatusReport). `SUM(CASE WHEN ... IS NULL)` is portable.
        let chunk_rows = sqlx::query_as::<_, (String, i64, Option<i64>)>(
            "SELECT c.source_id, COUNT(*) AS chunks_total, \
             SUM(CASE WHEN c.embedding IS NULL THEN 1 ELSE 0 END) AS chunks_unembedded \
             FROM content_chunks c JOIN pages p ON p.id = c.page_id \
             WHERE p.deleted_at IS NULL GROUP BY c.source_id",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("source_sync_stats chunks failed: {e}")))?;
        let chunk_counts: std::collections::HashMap<String, (u64, u64)> = chunk_rows
            .into_iter()
            .map(|(sid, total, unembedded)| {
                (sid, (total.max(0) as u64, unembedded.unwrap_or(0).max(0) as u64))
            })
            .collect();

        let mut out = Vec::new();
        for (id, name, local_path, last_commit, last_sync_at, config) in src_rows {
            let sync_enabled =
                config.get("syncEnabled").and_then(|v| v.as_bool()) != Some(false);
            let pages = *page_counts.get(&id).unwrap_or(&0);
            let (chunks_total, chunks_unembedded) =
                *chunk_counts.get(&id).unwrap_or(&(0, 0));
            out.push(crate::sync_status::SourceSyncStat {
                source_id: id,
                name,
                local_path,
                sync_enabled,
                last_sync_at,
                last_commit,
                pages,
                chunks_total,
                chunks_unembedded,
            });
        }
        Ok(out)
    }

    async fn create_source(&self, input: &CreateSourceInput) -> Result<SourceRow> {
        if !is_valid_source_id(&input.id) {
            return Err(Error::engine(format!(
                "invalid source id: '{}'",
                input.id
            )));
        }
        let pool = self.pool()?;
        let config = input.config.clone().unwrap_or_default();
        let row = sqlx::query_as::<_, (String, String, serde_json::Value, String)>(
            "INSERT INTO sources (id, name, config) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO NOTHING \
             RETURNING id, name, config, created_at"
        )
        .bind(&input.id)
        .bind(&input.name)
        .bind(&config)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("create_source failed: {e}")))?
        .ok_or_else(|| Error::engine(format!("source id '{}' already exists", input.id)))?;
        Ok(SourceRow {
            id: row.0,
            name: row.1,
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config: row.2,
            created_at: Some(row.3),
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        })
    }

    async fn update_source(&self, id: &str, input: &UpdateSourceInput) -> Result<SourceRow> {
        let pool = self.pool()?;
        // Build dynamic UPDATE — only touch columns that are Some
        let mut sets = Vec::new();
        let mut idx: u32 = 0;
        if input.name.is_some() {
            idx += 1;
            sets.push(format!("name = ${idx}"));
        }
        if input.config.is_some() {
            idx += 1;
            sets.push(format!("config = ${idx}"));
        }
        if input.local_path.is_some() {
            idx += 1;
            sets.push(format!("local_path = ${idx}"));
        }
        if input.last_commit.is_some() {
            idx += 1;
            sets.push(format!("last_commit = ${idx}"));
        }
        if input.last_sync_at.is_some() {
            idx += 1;
            sets.push(format!("last_sync_at = ${idx}"));
        }
        if input.chunker_version.is_some() {
            idx += 1;
            sets.push(format!("chunker_version = ${idx}"));
        }
        if input.contextual_retrieval_mode.is_some() {
            idx += 1;
            sets.push(format!("contextual_retrieval_mode = ${idx}"));
        }
        if input.trust_frontmatter_overrides.is_some() {
            idx += 1;
            sets.push(format!("trust_frontmatter_overrides = ${idx}"));
        }
        if sets.is_empty() {
            // Nothing to update — just return current row
            return self
                .get_source(id)
                .await?
                .ok_or_else(|| Error::engine(format!("source '{}' not found", id)));
        }
        // WHERE clause parameter
        idx += 1;
        let where_clause = format!(" WHERE id = ${idx}");
        let returning_sql = " RETURNING id, name, config, local_path, last_commit, last_sync_at, \
                             created_at, chunker_version, archived, archived_at, \
                             archive_expires_at, contextual_retrieval_mode, \
                             trust_frontmatter_overrides";
        let sql = format!("UPDATE sources SET {} {}{}", sets.join(", "), where_clause, returning_sql);

        let mut query = sqlx::query_as::<_, (
            String, String, serde_json::Value,
            Option<String>, Option<String>, Option<String>,
            Option<String>, Option<String>,
            bool, Option<String>, Option<String>,
            Option<String>, bool,
        )>(&sql);

        if let Some(ref name) = input.name { query = query.bind(name); }
        if let Some(ref config) = input.config { query = query.bind(config); }
        if let Some(ref local_path) = input.local_path { query = query.bind(local_path); }
        if let Some(ref last_commit) = input.last_commit { query = query.bind(last_commit); }
        if let Some(ref last_sync_at) = input.last_sync_at { query = query.bind(last_sync_at); }
        if let Some(ref chunker_version) = input.chunker_version { query = query.bind(chunker_version); }
        if let Some(ref mode) = input.contextual_retrieval_mode { query = query.bind(mode); }
        if let Some(trust) = input.trust_frontmatter_overrides { query = query.bind(trust); }
        query = query.bind(id);

        let row = query
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::engine(format!("update_source failed: {e}")))?
            .ok_or_else(|| Error::engine(format!("source '{}' not found", id)))?;

        Ok(SourceRow {
            id: row.0,
            name: row.1,
            local_path: row.3,
            last_commit: row.4,
            last_sync_at: row.5,
            config: row.2,
            created_at: row.6,
            chunker_version: row.7,
            archived: row.8,
            archived_at: row.9,
            archive_expires_at: row.10,
            contextual_retrieval_mode: row.11,
            trust_frontmatter_overrides: row.12,
        })
    }

    async fn delete_source(&self, id: &str) -> Result<bool> {
        let pool = self.pool()?;
        let result = sqlx::query(
            "UPDATE sources SET archived = true, archived_at = now(), \
             archive_expires_at = now() + INTERVAL '72 hours' \
             WHERE id = $1 AND archived = false"
        )
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("delete_source failed: {e}")))?;
        Ok(result.rows_affected() > 0)
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

        // Step 4: Run handler and verify hooks for all migrations that were just applied
        // Hooks run OUTSIDE the transaction (application-level logic may need to query DB)
        if applied_any {
            for migration in POSTGRES_MIGRATIONS.iter() {
                let ver = migration.version();
                if ver <= current {
                    continue;
                }
                migration.handler(self)?;
                if !migration.verify(self)? {
                    return Err(Error::engine(format!(
                        "migration {ver} verify failed: verification returned false"
                    )));
                }
            }
        }

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

        // Slice #110-c + G24: 20-column INSERT mirroring TS `putPage` plus the
        // page-level `embedding` write path. `last_retrieved_at` is still NOT
        // written by `put_page` (owned by the retrieval-tracker path).
        //
        // `ingested_at` is server-stamped when any ingestion metadata
        // (`source_kind`, `source_uri`, `ingested_via`) is present and the
        // caller did not supply an explicit value — mirrors TS
        // pglite-engine.ts:849.
        //
        // ON CONFLICT keeps the original `id` (BIGSERIAL) stable across
        // re-puts within the same source. UPDATE overwrites the user-provided
        // columns unconditionally, except `embedding` which is COALESCE-preserved
        // (embedding=None on upsert keeps the previously stored vector, matching
        // PageInput.embedding doc + libsql behaviour).
        //
        // Server-managed columns NOT in this INSERT:
        //   id, created_at, updated_at, deleted_at, salience_touched_at,
        //   salience_score, generation (trigger-bumped),
        //   contextual_retrieval_mode, corpus_generation, last_retrieved_at.

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
                 ingested_at, embedding\
             ) VALUES (\
                 $1, $2, $3, $4, $5, $6, $7, $8::jsonb, \
                 $9, $10, $11, $12, \
                 $13, $14, $15, $16, $17, \
                 $18, $19\
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
                 embedding = COALESCE(EXCLUDED.embedding, pages.embedding), \
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
            .bind(input.embedding.as_deref()) // $19 embedding (BYTEA, f32-LE, G24)
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

    async fn get_calibration_profile(
        &self,
        holder: &str,
        source_id: Option<&str>,
        source_ids: Option<&[String]>,
    ) -> Result<Option<crate::calibration_queries::CalibrationProfileRow>> {
        crate::calibration_queries::CalibrationQueries::get_latest_profile(self, holder, source_id, source_ids).await
    }

    async fn get_scorecard(
        &self,
        query: &crate::calibration_queries::ScorecardQuery<'_>,
    ) -> Result<crate::calibration_queries::TakesScorecard> {
        crate::calibration_queries::CalibrationQueries::get_scorecard(self, query).await
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

    async fn list_stale_pages(&self) -> Result<Vec<Page>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT id, slug, type, page_kind, title, compiled_truth, timeline, \
                    frontmatter, content_hash, emotional_weight, created_at, updated_at, \
                    deleted_at, last_retrieved_at, effective_date, effective_date_source, \
                    import_filename, salience_touched_at, salience_score, generation, \
                    embedding, chunker_version, source_path, source_id, source_kind, \
                    source_uri, ingested_via, ingested_at, contextual_retrieval_mode, \
                    corpus_generation \
             FROM pages \
             WHERE deleted_at IS NULL AND embedding IS NULL \
             ORDER BY slug",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("list_stale_pages query: {e}")))?;
        rows.iter().map(row_to_page).collect()
    }

    async fn put_page_embedding(
        &self,
        slug: &str,
        source_id: &str,
        embedding: Vec<u8>,
    ) -> Result<()> {
        let pool = self.pool()?;
        sqlx::query(
            "UPDATE pages SET embedding = $1 \
             WHERE slug = $2 AND source_id = $3 AND deleted_at IS NULL",
        )
        .bind(embedding)
        .bind(slug)
        .bind(source_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("put_page_embedding failed: {e}")))?;
        Ok(())
    }

    async fn set_page_timeline(
        &self,
        slug: &str,
        source_id: &str,
        timeline: String,
    ) -> Result<()> {
        let pool = self.pool()?;
        sqlx::query(
            "UPDATE pages SET timeline = $1 \
             WHERE slug = $2 AND source_id = $3 AND deleted_at IS NULL",
        )
        .bind(timeline)
        .bind(slug)
        .bind(source_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("set_page_timeline failed: {e}")))?;
        Ok(())
    }

    async fn search_pages(&self, opts: &SearchOpts) -> Result<Vec<SearchResult>> {
        // G23: real Postgres `search_pages`, mirroring the libsql slice (1-3-2)
        // and InMemory pattern. Two halves:
        //   1. Backend-specific: materialize live (non-deleted), optionally
        //      source-scoped candidate pages via FULL_PAGE_PROJECTION (which now
        //      includes `embedding` after G24, so the vector half of fusion can
        //      actually score PG-stored vectors — not just lexical).
        //   2. Backend-agnostic: hand candidates to the shared `fuse_and_boost`
        //      core so PG/libsql/InMemory share a single scoring truth.
        //
        // No keyword pre-filter in SQL — fusion does lexical + vector scoring
        // over the full live corpus, matching the other backends. The
        // `($1::text IS NULL OR source_id = $1)` clause leaves `None` unscoped.
        let pool = self.pool()?;
        let sql = format!(
            "SELECT {FULL_PAGE_PROJECTION} FROM pages \
             WHERE deleted_at IS NULL \
               AND ($1::text IS NULL OR source_id = $1)"
        );
        let rows = sqlx::query(&sql)
            .bind(opts.source_id.as_deref())
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("search_pages candidate query failed: {e}")))?;

        let candidates: Vec<Page> = rows.iter().map(row_to_page).collect::<Result<Vec<_>>>()?;

        fuse_and_boost(self, &candidates, opts).await
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

    async fn image_search_daily_spend_cents(&self, client_id: &str) -> Result<i64> {
        let pool = self.pool()?;
        let row = sqlx::query(
            "SELECT COALESCE(SUM(amount_cents), 0)::bigint AS total \
             FROM image_search_spend_log \
             WHERE client_id = $1 \
               AND created_at >= date_trunc('day', now() AT TIME ZONE 'UTC')",
        )
        .bind(client_id)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("image_search_daily_spend_cents failed: {e}")))?;
        let total: i64 = row
            .try_get("total")
            .map_err(|e| Error::engine(format!("image_search_daily_spend_cents decode failed: {e}")))?;
        Ok(total)
    }

    async fn record_image_search_spend(
        &self,
        client_id: &str,
        amount_cents: i64,
        provider: &str,
        model: &str,
    ) -> Result<()> {
        let pool = self.pool()?;
        sqlx::query(
            "INSERT INTO image_search_spend_log (client_id, amount_cents, provider, model, created_at) \
             VALUES ($1, $2, $3, $4, NOW())",
        )
        .bind(client_id)
        .bind(amount_cents)
        .bind(provider)
        .bind(model)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("record_image_search_spend failed: {e}")))?;
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

    async fn touch_salience(&self, slug: &str, source_id: &str) -> Result<bool> {
        let pool = self.pool()?;
        let result = sqlx::query(
            "UPDATE pages SET salience_touched_at = NOW() \
             WHERE slug = $1 AND source_id = $2 AND deleted_at IS NULL",
        )
        .bind(slug)
        .bind(source_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("touch_salience execute failed: {e}")))?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_recent_salience(
        &self,
        days: u32,
        limit: u32,
        slug_prefix: Option<&str>,
    ) -> Result<Vec<crate::types::SalienceResult>> {
        let pool = self.pool()?;
        let now = chrono::Utc::now();
        let boundary = now - chrono::Duration::days(days as i64);
        let limit = limit.min(100) as i32;

        let (prefix_condition, escaped_prefix) = slug_prefix
            .map(|pfx| {
                let escaped = pfx.replace('%', "\\%").replace('_', "\\_") + "%";
                ("AND p.slug LIKE $3 ESCAPE '\\'", Some(escaped))
            })
            .unwrap_or(("", None));

        // Postgres has native ln() and EXTRACT, so score is computed in SQL.
        let sql = format!(
            "SELECT p.slug, p.source_id, p.title, p.type, p.updated_at, \
                    COALESCE(p.emotional_weight, 0.0) AS emotional_weight, \
                    COUNT(DISTINCT t.id)::bigint AS take_count, \
                    COALESCE(AVG(t.weight), 0.0) AS take_avg_weight, \
                    COALESCE(p.emotional_weight, 0.0) * 5.0 \
                    + ln(1 + COUNT(DISTINCT t.id)) \
                    + 1.0 / (1.0 + GREATEST(0, EXTRACT(EPOCH FROM (NOW() - p.updated_at)) / 86400.0)) \
                    AS score \
             FROM pages p \
             LEFT JOIN takes t ON t.page_id = p.id AND t.active = TRUE \
             WHERE p.deleted_at IS NULL \
               AND CASE WHEN p.salience_touched_at > p.updated_at \
                        THEN p.salience_touched_at \
                        ELSE p.updated_at END >= $1::timestamptz \
               {prefix_condition} \
             GROUP BY p.id, p.slug, p.source_id, p.title, p.type, p.updated_at, p.emotional_weight \
             ORDER BY score DESC \
             LIMIT $2"
        );

        let mut q = sqlx::query(&sql).bind(boundary).bind(limit);
        if let Some(escaped) = escaped_prefix {
            q = q.bind(escaped);
        }

        let rows = q
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("get_recent_salience failed: {e}")))?;

        let results: Vec<crate::types::SalienceResult> = rows
            .into_iter()
            .map(|r| {
                let slug: String = r.try_get("slug").unwrap_or_default();
                let source_id: String = r.try_get("source_id").unwrap_or_default();
                let title: String = r.try_get("title").unwrap_or_default();
                let page_type: String = r.try_get("type").unwrap_or_default();
                let updated_at_val: chrono::DateTime<chrono::Utc> = r.try_get("updated_at").unwrap_or_default();
                let updated_at = updated_at_val.to_rfc3339();
                let emotional_weight: f64 = r.try_get("emotional_weight").unwrap_or(0.0);
                let take_count: i64 = r.try_get("take_count").unwrap_or(0);
                #[allow(clippy::cast_sign_loss)]
                let take_count = take_count as u32;
                let take_avg_weight: f64 = r.try_get("take_avg_weight").unwrap_or(0.0);
                let score: f64 = r.try_get("score").unwrap_or(0.0);

                crate::types::SalienceResult {
                    slug,
                    source_id,
                    title,
                    page_type,
                    updated_at,
                    emotional_weight,
                    take_count,
                    take_avg_weight,
                    score,
                }
            })
            .collect();

        Ok(results)
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

    async fn find_anomalies(
        &self,
        opts: crate::anomaly::AnomaliesOpts,
    ) -> crate::Result<Vec<crate::anomaly::AnomalyResult>> {
        use crate::anomaly::{
            compute_anomalies_from_buckets, resolve_anomaly_windows, CohortDayRow, CohortKind,
            CohortTodayRow,
        };
        let pool = self.pool()?;
        let (baseline_from, baseline_to, _today_from, today_to, _window_days, sigma, limit) =
            resolve_anomaly_windows(&opts)?;

        // --- Tag cohort baseline (densified via generate_series CROSS JOIN) ---
        let tag_baseline_sql = "
            WITH days AS (
                SELECT day::date FROM generate_series(
                    $1::date, $2::date - interval '1 day', '1 day'::interval
                ) AS day
            ),
            cohort_keys AS (
                SELECT DISTINCT pt.tag FROM page_tags pt
                    JOIN pages p ON p.id = pt.page_id
                 WHERE p.updated_at >= $1::timestamptz
                   AND p.updated_at <  $2::timestamptz
            ),
            touched AS (
                SELECT pt.tag,
                       date_trunc('day', p.updated_at)::date AS day,
                       COUNT(DISTINCT p.id) AS cnt
                  FROM page_tags pt JOIN pages p ON p.id = pt.page_id
                 WHERE p.updated_at >= $1::timestamptz
                   AND p.updated_at <  $2::timestamptz
                 GROUP BY 1, 2
            )
            SELECT ck.tag AS cohort_value, d.day::text AS day,
                   COALESCE(t.cnt, 0)::int AS count
              FROM cohort_keys ck CROSS JOIN days d
              LEFT JOIN touched t ON t.tag = ck.tag AND t.day = d.day";
        let tb = sqlx::query(tag_baseline_sql)
            .bind(&baseline_from)
            .bind(&baseline_to)
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("find_anomalies tag baseline: {e}")))?;
        let mut baseline: Vec<CohortDayRow> = Vec::with_capacity(tb.len());
        for r in tb {
            let cohort_value: String = r
                .try_get("cohort_value")
                .map_err(|e| Error::engine(format!("tag baseline decode cohort_value: {e}")))?;
            let day: String = r
                .try_get("day")
                .map_err(|e| Error::engine(format!("tag baseline decode day: {e}")))?;
            let count: i32 = r
                .try_get("count")
                .map_err(|e| Error::engine(format!("tag baseline decode count: {e}")))?;
            baseline.push(CohortDayRow {
                cohort_kind: CohortKind::Tag,
                cohort_value,
                day,
                count: count as i64,
            });
        }

        // --- Type cohort baseline ---
        let type_baseline_sql = "
            WITH days AS (
                SELECT day::date FROM generate_series(
                    $1::date, $2::date - interval '1 day', '1 day'::interval
                ) AS day
            ),
            cohort_keys AS (
                SELECT DISTINCT p.type FROM pages p
                 WHERE p.updated_at >= $1::timestamptz
                   AND p.updated_at <  $2::timestamptz
            ),
            touched AS (
                SELECT p.type,
                       date_trunc('day', p.updated_at)::date AS day,
                       COUNT(DISTINCT p.id) AS cnt
                  FROM pages p
                 WHERE p.updated_at >= $1::timestamptz
                   AND p.updated_at <  $2::timestamptz
                 GROUP BY 1, 2
            )
            SELECT ck.type AS cohort_value, d.day::text AS day,
                   COALESCE(t.cnt, 0)::int AS count
              FROM cohort_keys ck CROSS JOIN days d
              LEFT JOIN touched t ON t.type = ck.type AND t.day = d.day";
        let tb2 = sqlx::query(type_baseline_sql)
            .bind(&baseline_from)
            .bind(&baseline_to)
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("find_anomalies type baseline: {e}")))?;
        for r in tb2 {
            let cohort_value: String = r
                .try_get("cohort_value")
                .map_err(|e| Error::engine(format!("type baseline decode cohort_value: {e}")))?;
            let day: String = r
                .try_get("day")
                .map_err(|e| Error::engine(format!("type baseline decode day: {e}")))?;
            let count: i32 = r
                .try_get("count")
                .map_err(|e| Error::engine(format!("type baseline decode count: {e}")))?;
            baseline.push(CohortDayRow {
                cohort_kind: CohortKind::Type,
                cohort_value,
                day,
                count: count as i64,
            });
        }

        // --- Today's window counts + slugs ---
        let tag_today_sql = "
            SELECT pt.tag AS cohort_value,
                   COUNT(DISTINCT p.id)::int AS count,
                   array_agg(DISTINCT p.slug) AS slugs
              FROM page_tags pt JOIN pages p ON p.id = pt.page_id
             WHERE p.updated_at >= $1::timestamptz
               AND p.updated_at <  $2::timestamptz
             GROUP BY 1";
        let tt = sqlx::query(tag_today_sql)
            .bind(&baseline_to)
            .bind(&today_to)
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("find_anomalies tag today: {e}")))?;
        let mut today: Vec<CohortTodayRow> = Vec::with_capacity(tt.len());
        for r in tt {
            let cohort_value: String = r
                .try_get("cohort_value")
                .map_err(|e| Error::engine(format!("tag today decode cohort_value: {e}")))?;
            let count: i32 = r
                .try_get("count")
                .map_err(|e| Error::engine(format!("tag today decode count: {e}")))?;
            let slugs: Option<Vec<String>> = r
                .try_get("slugs")
                .map_err(|e| Error::engine(format!("tag today decode slugs: {e}")))?;
            today.push(CohortTodayRow {
                cohort_kind: CohortKind::Tag,
                cohort_value,
                count: count as i64,
                page_slugs: slugs.unwrap_or_default(),
            });
        }

        let type_today_sql = "
            SELECT p.type AS cohort_value,
                   COUNT(DISTINCT p.id)::int AS count,
                   array_agg(DISTINCT p.slug) AS slugs
              FROM pages p
             WHERE p.updated_at >= $1::timestamptz
               AND p.updated_at <  $2::timestamptz
             GROUP BY 1";
        let tt2 = sqlx::query(type_today_sql)
            .bind(&baseline_to)
            .bind(&today_to)
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("find_anomalies type today: {e}")))?;
        for r in tt2 {
            let cohort_value: String = r
                .try_get("cohort_value")
                .map_err(|e| Error::engine(format!("type today decode cohort_value: {e}")))?;
            let count: i32 = r
                .try_get("count")
                .map_err(|e| Error::engine(format!("type today decode count: {e}")))?;
            let slugs: Option<Vec<String>> = r
                .try_get("slugs")
                .map_err(|e| Error::engine(format!("type today decode slugs: {e}")))?;
            today.push(CohortTodayRow {
                cohort_kind: CohortKind::Type,
                cohort_value,
                count: count as i64,
                page_slugs: slugs.unwrap_or_default(),
            });
        }

        Ok(compute_anomalies_from_buckets(&baseline, &today, sigma, limit))
    }

    // --- Phase 7A: Takes ---

    async fn get_takes_for_page(
        &self,
        page_id: u64,
        takes_holders_allow_list: Option<Vec<String>>,
    ) -> Result<Vec<Take>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT id, page_id, row_num, claim, kind, holder, weight, \
                    since_date, until_date, source, superseded_by, active, \
                    resolved_at, resolved_quality, resolved_outcome, \
                    resolved_evidence, resolved_value, resolved_unit, \
                    resolved_by, created_at, updated_at \
             FROM takes \
             WHERE page_id = $1 \
               AND ($2::text[] IS NULL OR holder = ANY($2::text[])) \
             ORDER BY row_num ASC",
        )
        .bind(page_id as i64)
        .bind(takes_holders_allow_list)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_takes_for_page: {e}")))?;

        rows.into_iter()
            .map(|r| take_from_row(&r))
            .collect()
    }

    async fn list_takes(&self, opts: &TakesListOpts) -> Result<Vec<Take>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT id, page_id, row_num, claim, kind, holder, weight, \
                    since_date, until_date, source, superseded_by, active, \
                    resolved_at, resolved_quality, resolved_outcome, \
                    resolved_evidence, resolved_value, resolved_unit, \
                    resolved_by, created_at, updated_at \
             FROM takes \
             WHERE ($1::bigint IS NULL OR page_id = $1::bigint) \
               AND ($2::text   IS NULL OR holder = $2::text) \
               AND ($3::text   IS NULL OR kind = $3::text) \
               AND ($4::boolean IS NULL OR active = $4::boolean) \
               AND ($5::boolean IS NULL \
                    OR ($5::boolean = true AND resolved_at IS NOT NULL) \
                    OR ($5::boolean = false AND resolved_at IS NULL)) \
               AND ($6::text[] IS NULL OR holder = ANY($6::text[])) \
             ORDER BY weight DESC \
             LIMIT $7 OFFSET $8",
        )
        .bind(opts.page_id.map(|v| v as i64))
        .bind(opts.holder.clone())
        .bind(opts.kind.clone())
        .bind(opts.active)
        .bind(opts.resolved)
        .bind(opts.takes_holders_allow_list.clone())
        .bind(opts.limit.unwrap_or(100) as i64)
        .bind(opts.offset.unwrap_or(0) as i64)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("list_takes: {e}")))?;

        rows.into_iter().map(|r| take_from_row(&r)).collect()
    }

    async fn search_takes(&self, query: &str, opts: &SearchTakesOpts) -> Result<Vec<TakeHit>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT t.id, t.page_id, p.slug, t.row_num, t.claim, t.kind, t.holder, t.weight \
             FROM takes t JOIN pages p ON p.id = t.page_id \
             WHERE t.active \
               AND t.claim ILIKE '%' || $1 || '%' \
               AND ($2::text[] IS NULL OR t.holder = ANY($2::text[])) \
             ORDER BY t.weight DESC \
             LIMIT $3",
        )
        .bind(query)
        .bind(opts.takes_holders_allow_list.clone())
        .bind(opts.limit.unwrap_or(30) as i64)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("search_takes: {e}")))?;

        let q = query.to_lowercase();
        Ok(rows
            .into_iter()
            .map(|r| {
                let claim: String = r.try_get("claim").unwrap_or_default();
                let weight: f64 = r.try_get("weight").unwrap_or(0.5);
                let score = if q.is_empty() {
                    0.0
                } else {
                    claim.to_lowercase().matches(&q).count() as f64 * (1.0 + weight)
                };
                TakeHit {
                    take_id: r.try_get::<i64, _>("id").map(|v| v as u64).unwrap_or(0),
                    page_id: r.try_get::<i64, _>("page_id").map(|v| v as u64).unwrap_or(0),
                    page_slug: r.try_get("slug").unwrap_or_default(),
                    row_num: r.try_get("row_num").unwrap_or(0),
                    claim,
                    kind: r.try_get("kind").unwrap_or_default(),
                    holder: r.try_get("holder").unwrap_or_default(),
                    weight,
                    score,
                }
            })
            .collect())
    }

    async fn add_takes_batch(
        &self,
        page_id: u64,
        takes: &[TakeInput],
    ) -> Result<UpsertTakesResult> {
        if takes.is_empty() {
            return Ok(UpsertTakesResult { upserted: 0, weight_clamped: 0 });
        }
        let pool = self.pool()?;
        let now = sqlx::types::chrono::Utc::now();
        let mut upserted = 0usize;
        let mut weight_clamped = 0usize;

        for input in takes {
            let weight = input.weight.clamp(0.0, 1.0);
            if (weight - input.weight).abs() > f64::EPSILON {
                weight_clamped += 1;
            }
            sqlx::query(
                "INSERT INTO takes (page_id, row_num, claim, kind, holder, weight, \
                        since_date, until_date, source, superseded_by, active, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)",
            )
            .bind(page_id as i64)
            .bind(input.row_num.unwrap_or(0))
            .bind(&input.claim)
            .bind(&input.kind)
            .bind(&input.holder)
            .bind(weight)
            .bind(&input.since_date)
            .bind(&input.until_date)
            .bind(&input.source)
            .bind(input.superseded_by)
            .bind(input.active.unwrap_or(true))
            .bind(now)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("add_takes_batch insert: {e}")))?;
            upserted += 1;
        }

        Ok(UpsertTakesResult { upserted, weight_clamped })
    }

    async fn resolve_take(
        &self,
        page_id: u64,
        row_num: i32,
        resolution: &crate::types::TakeResolution,
    ) -> Result<()> {
        let pool = self.pool()?;
        let now = sqlx::types::chrono::Utc::now();
        // Existence check first, matching canonical TS ordering: TAKE_ROW_NOT_FOUND
        // is thrown before deriveResolutionTuple validates the resolution.
        let existing: Option<(i64,)> = sqlx::query_as(
            "SELECT 1::bigint FROM takes WHERE page_id = $1 AND row_num = $2",
        )
        .bind(page_id as i64)
        .bind(row_num)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("resolve_take existence check: {e}")))?;
        if existing.is_none() {
            return Err(crate::error::StructuredError::new(
                "Not Found",
                "not_found",
                format!("no take found for page_id={page_id} row_num={row_num}"),
            ));
        }
        // Canonical (resolved_quality, resolved_outcome) derivation — parity
        // with TS `deriveResolutionTuple`; errors on invalid/contradictory input.
        let (resolved_quality, resolved_outcome) = resolution.derive_quality_outcome()?;
        sqlx::query(
            "UPDATE takes SET \
                    resolved_at = $1, resolved_quality = $2, resolved_outcome = $3, \
                    resolved_evidence = $4, resolved_value = $5, resolved_unit = $6, \
                    resolved_by = $7, updated_at = $8 \
             WHERE page_id = $9 AND row_num = $10",
        )
        .bind(now)
        .bind(&resolved_quality)
        .bind(resolved_outcome)
        .bind(&resolution.evidence)
        .bind(resolution.value)
        .bind(&resolution.unit)
        .bind(&resolution.by)
        .bind(now)
        .bind(page_id as i64)
        .bind(row_num)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("resolve_take: {e}")))?;
        Ok(())
    }

    // ── Links (Phase 7B) ──────────────────────────────────────────────────

    async fn add_links_batch(&self, links: &[LinkBatchInput]) -> Result<usize> {
        if links.is_empty() {
            return Ok(0);
        }
        let pool = self.pool()?;

        // Use PostgreSQL unnest() pattern — same shape as TS addLinksBatch.
        // 10 array-typed parameters regardless of batch size.
        let from_slugs: Vec<&str> = links.iter().map(|l| l.from_slug.as_str()).collect();
        let to_slugs: Vec<&str> = links.iter().map(|l| l.to_slug.as_str()).collect();
        let link_types: Vec<&str> = links.iter().map(|l| l.link_type.as_deref().unwrap_or("")).collect();
        let contexts: Vec<&str> = links.iter().map(|l| l.context.as_deref().unwrap_or("")).collect();
        let link_sources: Vec<&str> = links.iter().map(|l| l.link_source.as_deref().unwrap_or("markdown")).collect();
        let origin_slugs: Vec<Option<&str>> = links.iter().map(|l| l.origin_slug.as_deref()).collect();
        let origin_fields: Vec<Option<&str>> = links.iter().map(|l| l.origin_field.as_deref()).collect();
        let from_source_ids: Vec<&str> = links.iter().map(|l| l.from_source_id.as_deref().unwrap_or("default")).collect();
        let to_source_ids: Vec<&str> = links.iter().map(|l| l.to_source_id.as_deref().unwrap_or("default")).collect();
        let origin_source_ids: Vec<&str> = links.iter().map(|l| l.origin_source_id.as_deref().unwrap_or("default")).collect();

        let result = sqlx::query(
            "INSERT INTO links (from_page_id, to_page_id, link_type, context, link_source, \
                    origin_page_id, origin_field) \
             SELECT f.id, t.id, v.link_type, v.context, v.link_source, o.id, v.origin_field \
             FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], \
                         $6::text[], $7::text[], $8::text[], $9::text[], $10::text[]) \
               AS v(from_slug, to_slug, link_type, context, link_source, \
                     origin_slug, origin_field, from_source_id, to_source_id, origin_source_id) \
             JOIN pages f ON f.slug = v.from_slug AND f.source_id = v.from_source_id \
             JOIN pages t ON t.slug = v.to_slug AND t.source_id = v.to_source_id \
             LEFT JOIN pages o ON o.slug = v.origin_slug AND o.source_id = v.origin_source_id \
             ON CONFLICT (from_page_id, to_page_id, link_type, link_source, origin_page_id) \
             DO NOTHING",
        )
        .bind(&from_slugs)
        .bind(&to_slugs)
        .bind(&link_types)
        .bind(&contexts)
        .bind(&link_sources)
        .bind(&origin_slugs)
        .bind(&origin_fields)
        .bind(&from_source_ids)
        .bind(&to_source_ids)
        .bind(&origin_source_ids)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("add_links_batch: {e}")))?;

        Ok(result.rows_affected() as usize)
    }

    async fn remove_link(
        &self,
        from: &str,
        to: &str,
        link_type: Option<&str>,
        link_source: Option<&str>,
        from_source_id: Option<&str>,
        to_source_id: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool()?;
        let from_sid = from_source_id.unwrap_or("default");
        let to_sid = to_source_id.unwrap_or("default");

        sqlx::query(
            "DELETE FROM links l \
             USING pages f, pages t \
             WHERE l.from_page_id = f.id \
               AND l.to_page_id = t.id \
               AND f.slug = $1 AND f.source_id = $3 \
               AND t.slug = $2 AND t.source_id = $4 \
               AND ($5::text IS NULL OR l.link_type = $5) \
               AND ($6::text IS NULL OR l.link_source = $6)",
        )
        .bind(from)
        .bind(to)
        .bind(from_sid)
        .bind(to_sid)
        .bind(link_type)
        .bind(link_source)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("remove_link: {e}")))?;

        Ok(())
    }

    async fn get_links(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> Result<Vec<Link>> {
        let pool = self.pool()?;
        let sid = source_id.unwrap_or("default");

        let rows = sqlx::query(
            "SELECT f.slug AS from_slug, t.slug AS to_slug, l.link_type, l.context, \
                    l.link_source, o.slug AS origin_slug, l.origin_field \
             FROM links l \
             JOIN pages f ON f.id = l.from_page_id \
             JOIN pages t ON t.id = l.to_page_id \
             LEFT JOIN pages o ON o.id = l.origin_page_id \
             WHERE f.slug = $1 AND f.source_id = $2 \
               AND f.deleted_at IS NULL AND t.deleted_at IS NULL \
             ORDER BY l.link_type, t.slug",
        )
        .bind(slug)
        .bind(sid)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_links: {e}")))?;

        rows.iter()
            .map(|row| {
                Ok(Link {
                    from_slug: row.try_get("from_slug").map_err(|e| Error::engine(format!("decode from_slug: {e}")))?,
                    to_slug: row.try_get("to_slug").map_err(|e| Error::engine(format!("decode to_slug: {e}")))?,
                    link_type: row.try_get("link_type").map_err(|e| Error::engine(format!("decode link_type: {e}")))?,
                    context: row.try_get("context").map_err(|e| Error::engine(format!("decode context: {e}")))?,
                    link_source: row.try_get("link_source").map_err(|e| Error::engine(format!("decode link_source: {e}")))?,
                    origin_slug: row.try_get("origin_slug").map_err(|e| Error::engine(format!("decode origin_slug: {e}")))?,
                    origin_field: row.try_get("origin_field").map_err(|e| Error::engine(format!("decode origin_field: {e}")))?,
                })
            })
            .collect()
    }

    async fn get_backlinks(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> Result<Vec<Link>> {
        let pool = self.pool()?;
        let sid = source_id.unwrap_or("default");

        let rows = sqlx::query(
            "SELECT f.slug AS from_slug, t.slug AS to_slug, l.link_type, l.context, \
                    l.link_source, o.slug AS origin_slug, l.origin_field \
             FROM links l \
             JOIN pages f ON f.id = l.from_page_id \
             JOIN pages t ON t.id = l.to_page_id \
             LEFT JOIN pages o ON o.id = l.origin_page_id \
             WHERE t.slug = $1 AND t.source_id = $2 \
               AND f.deleted_at IS NULL AND t.deleted_at IS NULL \
             ORDER BY l.link_type, f.slug",
        )
        .bind(slug)
        .bind(sid)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_backlinks: {e}")))?;

        rows.iter()
            .map(|row| {
                Ok(Link {
                    from_slug: row.try_get("from_slug").map_err(|e| Error::engine(format!("decode from_slug: {e}")))?,
                    to_slug: row.try_get("to_slug").map_err(|e| Error::engine(format!("decode to_slug: {e}")))?,
                    link_type: row.try_get("link_type").map_err(|e| Error::engine(format!("decode link_type: {e}")))?,
                    context: row.try_get("context").map_err(|e| Error::engine(format!("decode context: {e}")))?,
                    link_source: row.try_get("link_source").map_err(|e| Error::engine(format!("decode link_source: {e}")))?,
                    origin_slug: row.try_get("origin_slug").map_err(|e| Error::engine(format!("decode origin_slug: {e}")))?,
                    origin_field: row.try_get("origin_field").map_err(|e| Error::engine(format!("decode origin_field: {e}")))?,
                })
            })
            .collect()
    }

    // ── 1-6-7-10-2: code-graph write + query methods (Postgres) ──────────

    async fn add_code_edges(
        &self,
        edges: &[crate::import::CodeEdgeInput],
    ) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let pool = self.pool()?;
        for e in edges {
            // Mirror TS: edge_metadata defaults to {} when null.
            let meta = if e.edge_metadata.is_null() {
                serde_json::json!({})
            } else {
                e.edge_metadata.clone()
            };
            match e.to_chunk_id {
                Some(to_chunk_id) => {
                    sqlx::query(
                        "INSERT INTO code_edges_chunk \
                         (from_chunk_id, to_chunk_id, from_symbol_qualified, to_symbol_qualified, edge_type, edge_metadata, source_id) \
                         VALUES ($1,$2,$3,$4,$5,$6,$7) \
                         ON CONFLICT (from_chunk_id, to_chunk_id, edge_type) DO NOTHING",
                    )
                    .bind(e.from_chunk_id)
                    .bind(to_chunk_id)
                    .bind(&e.from_symbol_qualified)
                    .bind(&e.to_symbol_qualified)
                    .bind(&e.edge_type)
                    .bind(&meta)
                    .bind(e.source_id.clone())
                    .execute(pool)
                    .await
                    .map_err(|err| Error::engine(format!("add_code_edges (chunk) insert failed: {err}")))?;
                }
                None => {
                    sqlx::query(
                        "INSERT INTO code_edges_symbol \
                         (from_chunk_id, from_symbol_qualified, to_symbol_qualified, edge_type, edge_metadata, source_id) \
                         VALUES ($1,$2,$3,$4,$5,$6) \
                         ON CONFLICT (from_chunk_id, to_symbol_qualified, edge_type) DO NOTHING",
                    )
                    .bind(e.from_chunk_id)
                    .bind(&e.from_symbol_qualified)
                    .bind(&e.to_symbol_qualified)
                    .bind(&e.edge_type)
                    .bind(&meta)
                    .bind(e.source_id.clone())
                    .execute(pool)
                    .await
                    .map_err(|err| Error::engine(format!("add_code_edges (symbol) insert failed: {err}")))?;
                }
            }
        }
        Ok(())
    }

    async fn delete_code_edges_for_chunks(
        &self,
        chunk_ids: &[i64],
    ) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        let pool = self.pool()?;
        for &cid in chunk_ids {
            sqlx::query("DELETE FROM code_edges_chunk WHERE from_chunk_id = $1 OR to_chunk_id = $1")
                .bind(cid)
                .execute(pool)
                .await
                .map_err(|err| Error::engine(format!("delete_code_edges_for_chunks (chunk) failed: {err}")))?;
            sqlx::query("DELETE FROM code_edges_symbol WHERE from_chunk_id = $1")
                .bind(cid)
                .execute(pool)
                .await
                .map_err(|err| Error::engine(format!("delete_code_edges_for_chunks (symbol) failed: {err}")))?;
        }
        Ok(())
    }

    async fn get_callers_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> Result<Vec<crate::import::CodeEdgeResult>> {
        code_edge_symbol_query_pg(self, "to_symbol_qualified", qualified_name, opts).await
    }

    async fn get_callees_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> Result<Vec<crate::import::CodeEdgeResult>> {
        code_edge_symbol_query_pg(self, "from_symbol_qualified", qualified_name, opts).await
    }

    async fn get_edges_by_chunk(
        &self,
        chunk_id: i64,
        opts: &crate::import::CodeEdgeByChunkOpts,
    ) -> Result<Vec<crate::import::CodeEdgeResult>> {
        let pool = self.pool()?;
        let limit = (opts.limit.unwrap_or(50) as i64).min(200);

        let chunk_filter = match opts.direction {
            crate::import::CodeEdgeDirection::In => " WHERE to_chunk_id = $1",
            crate::import::CodeEdgeDirection::Out => " WHERE from_chunk_id = $1",
            crate::import::CodeEdgeDirection::Both => " WHERE (from_chunk_id = $1 OR to_chunk_id = $1)",
        };
        let sym_filter = match opts.direction {
            crate::import::CodeEdgeDirection::In => None,
            crate::import::CodeEdgeDirection::Out | crate::import::CodeEdgeDirection::Both => {
                Some(" WHERE from_chunk_id = $1")
            }
        };

        let mut sql = format!(
            "SELECT id, from_chunk_id, to_chunk_id, from_symbol_qualified, to_symbol_qualified, \
                    edge_type, edge_metadata, source_id, true AS resolved \
               FROM code_edges_chunk{chunk_filter}",
        );
        if let Some(sf) = sym_filter {
            sql.push_str(&format!(
                " UNION ALL SELECT id, from_chunk_id, NULL AS to_chunk_id, from_symbol_qualified, \
                        to_symbol_qualified, edge_type, edge_metadata, source_id, false AS resolved \
                   FROM code_edges_symbol{sf}",
            ));
        }
        let has_edge_type = opts.edge_type.is_some();
        if has_edge_type {
            sql.push_str(" AND edge_type = $2");
            if sym_filter.is_some() {
                sql.push_str(" AND edge_type = $2");
            }
        }
        let limit_ph = if has_edge_type { 3 } else { 2 };
        sql.push_str(&format!(" LIMIT ${limit_ph}"));

        let mut q = sqlx::query(&sql).bind(chunk_id);
        if has_edge_type {
            q = q.bind(opts.edge_type.clone().unwrap());
        }
        q = q.bind(limit);
        let rows = q
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("get_edges_by_chunk query failed: {e}")))?;
        rows.iter().map(code_edge_row_to_result_pg).collect()
    }

    async fn find_code_def(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> Result<Vec<crate::import::CodeDefResult>> {
        let pool = self.pool()?;
        code_def_query_pg(pool, symbol, opts).await
    }

    async fn find_code_refs(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> Result<Vec<crate::import::CodeRefResult>> {
        let pool = self.pool()?;
        code_ref_query_pg(pool, symbol, opts).await
    }

    async fn disambiguate_symbol(
        &self,
        bare: &str,
        source_id: &str,
    ) -> Result<crate::import::SymbolDisambiguation> {
        let pool = self.pool()?;
        code_disambiguate_query_pg(pool, bare, source_id).await
    }

    async fn recursive_walk(
        &self,
        symbol: &str,
        opts: &crate::import::RecursiveWalkOpts,
    ) -> Result<crate::import::RecursiveWalkResult> {
        let pool = self.pool()?;
        code_recursive_walk_query_pg(self, pool, symbol, opts).await
    }

    async fn get_backlink_counts(
        &self,
        slugs: &[String],
    ) -> Result<std::collections::HashMap<String, u64>> {
        if slugs.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let pool = self.pool()?;

        let rows = sqlx::query(
            "SELECT t.slug, COUNT(*)::bigint \
             FROM links l \
             JOIN pages t ON t.id = l.to_page_id \
             JOIN pages f ON f.id = l.from_page_id \
             WHERE t.slug = ANY($1) \
               AND f.deleted_at IS NULL AND t.deleted_at IS NULL \
             GROUP BY t.slug",
        )
        .bind(slugs)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_backlink_counts: {e}")))?;

        let mut counts: std::collections::HashMap<String, u64> =
            slugs.iter().map(|s| (s.clone(), 0u64)).collect();

        for row in &rows {
            let slug: String = row.try_get("slug")
                .map_err(|e| Error::engine(format!("decode slug: {e}")))?;
            let count: i64 = row.try_get("count")
                .map_err(|e| Error::engine(format!("decode count: {e}")))?;
            counts.insert(slug, count as u64);
        }
        Ok(counts)
    }

    async fn get_adjacency_boosts(
        &self,
        page_ids: &[u64],
    ) -> Result<std::collections::HashMap<u64, AdjacencyRow>> {
        if page_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let pool = self.pool()?;

        let ids: Vec<i64> = page_ids.iter().map(|&id| id as i64).collect();

        let rows = sqlx::query(
            "WITH targets AS ( \
               SELECT id, COALESCE(source_id, 'default') AS source_id \
               FROM pages \
               WHERE id = ANY($1) \
                 AND deleted_at IS NULL \
             ) \
             SELECT \
               l.to_page_id, \
               COUNT(DISTINCT l.from_page_id)::int AS hits, \
               COUNT(DISTINCT \
                 CASE WHEN COALESCE(p.source_id, 'default') <> t.source_id \
                      THEN COALESCE(p.source_id, 'default') END \
               )::int AS cross_source_hits \
             FROM links l \
             JOIN pages p ON p.id = l.from_page_id AND p.deleted_at IS NULL \
             JOIN targets t ON t.id = l.to_page_id \
             WHERE l.from_page_id = ANY($1) \
               AND l.to_page_id = ANY($1) \
             GROUP BY l.to_page_id \
             HAVING COUNT(DISTINCT l.from_page_id) >= 1",
        )
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_adjacency_boosts: {e}")))?;

        let mut result: std::collections::HashMap<u64, AdjacencyRow> = std::collections::HashMap::new();

        for row in &rows {
            let to_page_id: i64 = row
                .try_get("to_page_id")
                .map_err(|e| Error::engine(format!("decode to_page_id: {e}")))?;
            let hits: i32 = row
                .try_get("hits")
                .map_err(|e| Error::engine(format!("decode hits: {e}")))?;
            let cross_source_hits: i32 = row
                .try_get("cross_source_hits")
                .map_err(|e| Error::engine(format!("decode cross_source_hits: {e}")))?;

            result.insert(
                to_page_id as u64,
                AdjacencyRow {
                    hits: hits as u32,
                    cross_source_hits: cross_source_hits as u32,
                },
            );
        }

        Ok(result)
    }

    // ─── Facts ───────────────────────────────────────────────────────────

    async fn insert_fact(
        &self,
        source_id: &str,
        entity_slug: &str,
        input: &NewFact,
    ) -> Result<FactInsertStatus> {
        let now = current_utc_iso8601();
        let pool = self.pool()?;

        let kind = input
            .kind
            .as_ref()
            .map(|k| k.to_string())
            .unwrap_or_else(|| "fact".to_string());
        let visibility = input
            .visibility
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "private".to_string());
        let notability = input.notability.as_deref().unwrap_or("medium");
        let valid_from = input.valid_from.clone().unwrap_or_else(|| now.clone());
        let confidence = input.confidence.unwrap_or(1.0);

        // Single CTE: dup_check → supersede_target → insert → update_old
        // Postgres CTEs are evaluated in source order for data-modifying statements;
        // the WHERE NOT EXISTS guard prevents INSERT when a duplicate exists.
        let row = sqlx::query(
            "WITH dup_check AS ( \
                SELECT id FROM facts \
                WHERE source_id = $1 AND entity_slug = $2 AND fact = $3 \
                  AND kind = $4 AND expired_at IS NULL AND superseded_by IS NULL \
                LIMIT 1 \
             ), \
             supersede_target AS ( \
                SELECT id FROM facts \
                WHERE source_id = $1 AND entity_slug = $2 AND kind = $4 \
                  AND expired_at IS NULL AND superseded_by IS NULL \
                  AND $12 > 0.9 \
                LIMIT 1 \
             ), \
             inserted AS ( \
                INSERT INTO facts \
                    (source_id, entity_slug, fact, kind, visibility, notability, \
                     context, valid_from, valid_until, source, source_session, confidence, \
                     claim_metric, claim_value, claim_unit, claim_period, event_type) \
                SELECT $1, $2, $3, $4, $5, $6, $7, $8::timestamptz, $9::timestamptz, $10, $11, $12, \
                       $13, $14, $15, $16, $17 \
                WHERE NOT EXISTS (SELECT 1 FROM dup_check) \
                RETURNING id \
             ), \
             updated AS ( \
                UPDATE facts SET superseded_by = (SELECT id FROM inserted) \
                WHERE id = (SELECT id FROM supersede_target) \
                  AND EXISTS (SELECT 1 FROM inserted) \
             ) \
             SELECT \
                CASE WHEN EXISTS(SELECT 1 FROM dup_check) THEN 'duplicate' \
                     WHEN EXISTS(SELECT 1 FROM supersede_target) THEN 'superseded' \
                     ELSE 'inserted' \
                END AS status, \
                COALESCE((SELECT id FROM inserted), 0) AS new_id",
        )
        .bind(source_id)
        .bind(entity_slug)
        .bind(input.fact.as_str())
        .bind(kind.as_str())
        .bind(visibility.as_str())
        .bind(notability)
        .bind(input.context.as_deref().unwrap_or(""))
        .bind(valid_from.as_str())
        .bind(input.valid_until.as_deref())
        .bind(input.source.as_str())
        .bind(input.source_session.as_deref().unwrap_or(""))
        .bind(confidence)
        .bind(input.claim_metric.clone())
        .bind(input.claim_value)
        .bind(input.claim_unit.clone())
        .bind(input.claim_period.clone())
        .bind(input.event_type.clone())
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("insert_fact: {e}")))?;

        let status: String = row
            .try_get("status")
            .map_err(|e| Error::engine(format!("insert_fact status decode: {e}")))?;

        match status.as_str() {
            "duplicate" => Ok(FactInsertStatus::Duplicate),
            "superseded" => Ok(FactInsertStatus::Superseded),
            _ => Ok(FactInsertStatus::Inserted),
        }
    }

    async fn list_facts_by_entity(
        &self,
        source_id: &str,
        entity_slug: &str,
        opts: &FactListOpts,
    ) -> Result<Vec<FactRow>> {
        let pool = self.pool()?;

        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from::text AS valid_from, \
                    valid_until::text AS valid_until, expired_at::text AS expired_at, \
                    superseded_by::bigint AS superseded_by, \
                    consolidated_at::text AS consolidated_at, \
                    consolidated_into::bigint AS consolidated_into, source, \
                    source_session, confidence, created_at::text AS created_at \
             FROM facts \
             WHERE source_id = ",
        );
        builder.push_bind(source_id);
        builder.push(" AND entity_slug = ");
        builder.push_bind(entity_slug);

        if opts.active_only.unwrap_or(false) {
            builder.push(" AND expired_at IS NULL AND superseded_by IS NULL");
        }
        if let Some(ref kinds) = opts.kinds {
            if !kinds.is_empty() {
                builder.push(" AND kind IN (");
                let mut separated = builder.separated(", ");
                for k in kinds {
                    separated.push_bind(k.to_string());
                }
                separated.push_unseparated(")");
            }
        }
        if let Some(ref vs) = opts.visibility {
            if !vs.is_empty() {
                builder.push(" AND visibility IN (");
                let mut separated = builder.separated(", ");
                for v in vs {
                    separated.push_bind(v.to_string());
                }
                separated.push_unseparated(")");
            }
        }

        builder.push(" ORDER BY created_at DESC");

        if let Some(ref limit) = opts.limit {
            builder.push(" LIMIT ");
            builder.push_bind(*limit);
        }
        if let Some(ref offset) = opts.offset {
            builder.push(" OFFSET ");
            builder.push_bind(*offset);
        }

        let rows = builder
            .build()
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("list_facts_by_entity: {e}")))?;

        rows.iter().map(|r| pg_row_to_fact(r)).collect()
    }

    async fn find_trajectory(
        &self,
        opts: &crate::types::TrajectoryOpts,
    ) -> Result<Vec<crate::types::TrajectoryPoint>> {
        let pool = self.pool()?;

        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, valid_from::text AS valid_from, claim_metric, claim_value, \
                    claim_unit, claim_period, event_type, fact, source_session, \
                    source_markdown_slug, embedding::text AS embedding \
             FROM facts \
             WHERE ",
        );

        match &opts.source_ids {
            Some(ids) if !ids.is_empty() => {
                builder.push("source_id = ANY(");
                builder.push_bind(ids.clone());
                builder.push("::text[])");
            }
            _ => {
                let sid = opts.source_id.clone().unwrap_or_else(|| "default".to_string());
                builder.push("source_id = ");
                builder.push_bind(sid);
            }
        }

        builder.push(" AND entity_slug = ");
        builder.push_bind(opts.entity_slug.clone());
        builder.push(" AND expired_at IS NULL");

        if opts.remote {
            builder.push(" AND visibility = 'world'");
        }
        if let Some(ref metric) = opts.metric {
            builder.push(" AND claim_metric = ");
            builder.push_bind(metric.clone());
        }
        match opts.kind {
            crate::types::TrajectoryKind::Metric => {
                builder.push(" AND claim_metric IS NOT NULL");
            }
            crate::types::TrajectoryKind::Event => {
                builder.push(" AND event_type IS NOT NULL");
            }
            crate::types::TrajectoryKind::All => {}
        }
        if let Some(ref since) = opts.since {
            builder.push(" AND valid_from >= ");
            builder.push_bind(since.clone());
        }
        if let Some(ref until) = opts.until {
            builder.push(" AND valid_from <= ");
            builder.push_bind(until.clone());
        }

        builder.push(" ORDER BY valid_from ASC, id ASC");

        let limit = (opts.limit.unwrap_or(100) as i64).clamp(1, 500);
        builder.push(" LIMIT ");
        builder.push_bind(limit);

        let rows = builder
            .build()
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("find_trajectory: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let valid_from: Option<String> = row.try_get("valid_from").ok().flatten();
            let embedding = crate::trajectory_stats::parse_embedding_text(
                row.try_get::<Option<String>, _>("embedding").ok().flatten(),
            );
            out.push(crate::types::TrajectoryPoint {
                fact_id: row.try_get("id").map_err(|e| Error::engine(format!("ft id: {e}")))?,
                valid_from: valid_from.map(|s| crate::trajectory_stats::iso_date_prefix(&s)),
                metric: row.try_get("claim_metric").ok().flatten(),
                value: row.try_get("claim_value").ok().flatten(),
                unit: row.try_get("claim_unit").ok().flatten(),
                period: row.try_get("claim_period").ok().flatten(),
                event_type: row.try_get("event_type").ok().flatten(),
                text: row.try_get::<String, _>("fact").unwrap_or_default(),
                source_session: row.try_get("source_session").ok().flatten(),
                source_markdown_slug: row.try_get("source_markdown_slug").ok().flatten(),
                embedding,
            });
        }
        Ok(out)
    }

    async fn list_facts_since(
        &self,
        source_id: &str,
        since_iso: &str,
        opts: &FactListOpts,
    ) -> Result<Vec<FactRow>> {
        let pool = self.pool()?;
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from::text AS valid_from, \
                    valid_until::text AS valid_until, expired_at::text AS expired_at, \
                    superseded_by::bigint AS superseded_by, \
                    consolidated_at::text AS consolidated_at, \
                    consolidated_into::bigint AS consolidated_into, source, \
                    source_session, confidence, created_at::text AS created_at \
             FROM facts \
             WHERE source_id = ",
        );
        builder.push_bind(source_id);
        builder.push(" AND created_at >= ");
        builder.push_bind(since_iso);
        if opts.active_only.unwrap_or(false) {
            builder.push(" AND expired_at IS NULL AND superseded_by IS NULL");
        }
        if let Some(ref kinds) = opts.kinds {
            if !kinds.is_empty() {
                builder.push(" AND kind IN (");
                let mut separated = builder.separated(", ");
                for k in kinds {
                    separated.push_bind(k.to_string());
                }
                separated.push_unseparated(")");
            }
        }
        if let Some(ref vs) = opts.visibility {
            if !vs.is_empty() {
                builder.push(" AND visibility IN (");
                let mut separated = builder.separated(", ");
                for v in vs {
                    separated.push_bind(v.to_string());
                }
                separated.push_unseparated(")");
            }
        }
        builder.push(" ORDER BY created_at DESC, id DESC");
        if let Some(ref limit) = opts.limit {
            builder.push(" LIMIT ");
            builder.push_bind(*limit);
        }
        let rows = builder
            .build()
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("list_facts_since: {e}")))?;
        rows.iter().map(|r| pg_row_to_fact(r)).collect()
    }

    async fn list_facts_by_session(
        &self,
        source_id: &str,
        session_id: &str,
        opts: &FactListOpts,
    ) -> Result<Vec<FactRow>> {
        let pool = self.pool()?;
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from::text AS valid_from, \
                    valid_until::text AS valid_until, expired_at::text AS expired_at, \
                    superseded_by::bigint AS superseded_by, \
                    consolidated_at::text AS consolidated_at, \
                    consolidated_into::bigint AS consolidated_into, source, \
                    source_session, confidence, created_at::text AS created_at \
             FROM facts \
             WHERE source_id = ",
        );
        builder.push_bind(source_id);
        builder.push(" AND source_session = ");
        builder.push_bind(session_id);
        if opts.active_only.unwrap_or(false) {
            builder.push(" AND expired_at IS NULL AND superseded_by IS NULL");
        }
        if let Some(ref kinds) = opts.kinds {
            if !kinds.is_empty() {
                builder.push(" AND kind IN (");
                let mut separated = builder.separated(", ");
                for k in kinds {
                    separated.push_bind(k.to_string());
                }
                separated.push_unseparated(")");
            }
        }
        if let Some(ref vs) = opts.visibility {
            if !vs.is_empty() {
                builder.push(" AND visibility IN (");
                let mut separated = builder.separated(", ");
                for v in vs {
                    separated.push_bind(v.to_string());
                }
                separated.push_unseparated(")");
            }
        }
        builder.push(" ORDER BY created_at DESC, id DESC");
        if let Some(ref limit) = opts.limit {
            builder.push(" LIMIT ");
            builder.push_bind(*limit);
        }
        let rows = builder
            .build()
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("list_facts_by_session: {e}")))?;
        rows.iter().map(|r| pg_row_to_fact(r)).collect()
    }

    async fn list_supersessions(
        &self,
        source_id: &str,
        opts: &crate::types::SupersessionOpts,
    ) -> Result<Vec<FactRow>> {
        let pool = self.pool()?;
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from::text AS valid_from, \
                    valid_until::text AS valid_until, expired_at::text AS expired_at, \
                    superseded_by::bigint AS superseded_by, \
                    consolidated_at::text AS consolidated_at, \
                    consolidated_into::bigint AS consolidated_into, source, \
                    source_session, confidence, created_at::text AS created_at \
             FROM facts \
             WHERE source_id = ",
        );
        builder.push_bind(source_id);
        builder.push(" AND expired_at IS NOT NULL AND superseded_by IS NOT NULL");
        if let Some(ref since) = opts.since {
            builder.push(" AND expired_at >= ");
            builder.push_bind(since.clone());
        }
        builder.push(" ORDER BY expired_at DESC, id DESC");
        if let Some(ref limit) = opts.limit {
            builder.push(" LIMIT ");
            builder.push_bind(*limit);
        }
        let rows = builder
            .build()
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("list_supersessions: {e}")))?;
        rows.iter().map(|r| pg_row_to_fact(r)).collect()
    }

    async fn count_unconsolidated_facts(&self, source_id: &str) -> Result<i64> {
        let pool = self.pool()?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM facts \
             WHERE source_id = $1 AND consolidated_at IS NULL AND expired_at IS NULL",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("count_unconsolidated_facts: {e}")))?;
        Ok(count)
    }

    async fn get_facts_health(&self, source_id: &str) -> Result<FactsHealth> {
        let pool = self.pool()?;
        let now = current_utc_iso8601();
        let today_prefix = &now[..10];

        let total_active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM facts \
             WHERE source_id = $1 AND expired_at IS NULL AND superseded_by IS NULL",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_facts_health active: {e}")))?;

        let total_today: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM facts \
             WHERE source_id = $1 AND created_at >= $2::timestamptz",
        )
        .bind(source_id)
        .bind(today_prefix)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_facts_health today: {e}")))?;

        let total_week: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM facts \
             WHERE source_id = $1 AND created_at >= (NOW() - INTERVAL '7 days')",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_facts_health week: {e}")))?;

        let total_expired: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM facts \
             WHERE source_id = $1 AND expired_at IS NOT NULL",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_facts_health expired: {e}")))?;

        let total_consolidated: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM facts \
             WHERE source_id = $1 AND consolidated_at IS NOT NULL",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_facts_health consolidated: {e}")))?;

        // Top entities by fact count
        let top_rows = sqlx::query(
            "SELECT entity_slug, COUNT(*)::bigint AS cnt \
             FROM facts \
             WHERE source_id = $1 AND entity_slug IS NOT NULL \
             GROUP BY entity_slug \
             ORDER BY cnt DESC \
             LIMIT 10",
        )
        .bind(source_id)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_facts_health top: {e}")))?;

        let mut top_entities = Vec::new();
        for row in &top_rows {
            let slug: String = row
                .try_get("entity_slug")
                .map_err(|e| Error::engine(format!("get_facts_health top slug: {e}")))?;
            let count: i64 = row
                .try_get("cnt")
                .map_err(|e| Error::engine(format!("get_facts_health top cnt: {e}")))?;
            top_entities.push(EntityCount {
                entity_slug: slug,
                count,
            });
        }

        Ok(FactsHealth {
            source_id: source_id.to_string(),
            total_active,
            total_today,
            total_week,
            total_expired,
            total_consolidated,
            top_entities,
        })
    }

    async fn expire_fact(&self, source_id: &str, fact_id: i64) -> Result<bool> {
        let pool = self.pool()?;
        let now = current_utc_iso8601();

        let result = sqlx::query(
            "UPDATE facts SET expired_at = $1::timestamptz \
             WHERE id = $2 AND source_id = $3 AND expired_at IS NULL",
        )
        .bind(now.as_str())
        .bind(fact_id)
        .bind(source_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("expire_fact: {e}")))?;

        Ok(result.rows_affected() > 0)
    }

    async fn traverse_paths(
        &self,
        slug: &str,
        depth: Option<u32>,
        link_type: Option<&str>,
        direction: Option<&str>,
        source_id: Option<&str>,
        _source_ids: Option<&[String]>,
    ) -> Result<Vec<GraphPath>> {
        use std::collections::{HashMap, HashSet, VecDeque};

        let pool = self.pool()?;
        let max_depth = depth.unwrap_or(1);
        let dir = direction.unwrap_or("out");

        // ── Fetch all non-deleted pages → build id→slug map + find start ──
        let page_rows = sqlx::query(
            "SELECT id, slug, source_id FROM pages WHERE deleted_at IS NULL",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("traverse_paths pages: {e}")))?;

        let mut id_to_slug: HashMap<u64, String> = HashMap::new();
        let mut start_page_id: Option<u64> = None;

        for row in &page_rows {
            let id: i64 = row.get("id");
            let slug_str: String = row.get("slug");
            let source_id_str: String = row.get("source_id");
            let id_u64 = u64::try_from(id)
                .map_err(|_| Error::engine("traverse_paths: page id out of u64 range"))?;

            id_to_slug.insert(id_u64, slug_str.clone());

            if slug_str == slug
                && (source_id.is_none()
                    || source_id_str == source_id.unwrap_or(""))
            {
                start_page_id = Some(id_u64);
            }
        }

        let Some(start_id) = start_page_id else {
            return Ok(Vec::new());
        };

        // ── Fetch all links, keep only edges between non-deleted pages ────
        let link_rows = sqlx::query(
            "SELECT from_page_id, to_page_id, link_type, context FROM links",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("traverse_paths links: {e}")))?;

        // (from_id, to_id, link_type, context)
        let mut edges: Vec<(u64, u64, String, String)> = Vec::new();
        for row in &link_rows {
            let from_id: i64 = row.get("from_page_id");
            let to_id: i64 = row.get("to_page_id");
            let lt: String = row.get("link_type");
            let ctx: String = row.get("context");
            let from_u64 = u64::try_from(from_id)
                .map_err(|_| Error::engine("traverse_paths: from_page_id out of u64 range"))?;
            let to_u64 = u64::try_from(to_id)
                .map_err(|_| Error::engine("traverse_paths: to_page_id out of u64 range"))?;

            if id_to_slug.contains_key(&from_u64) && id_to_slug.contains_key(&to_u64) {
                edges.push((from_u64, to_u64, lt, ctx));
            }
        }

        // ── BFS traversal ─────────────────────────────────────────────────
        let mut result: Vec<GraphPath> = Vec::new();
        let mut visited: HashSet<u64> = HashSet::new();
        let mut queue: VecDeque<(u64, u32)> = VecDeque::new();
        visited.insert(start_id);
        queue.push_back((start_id, 0));

        while let Some((current_id, current_depth)) = queue.pop_front() {
            if current_depth >= max_depth {
                continue;
            }

            for (from_id, to_id, lt, ctx) in &edges {
                // Direction filter
                let is_match = match dir {
                    "out" => *from_id == current_id,
                    "in" => *to_id == current_id,
                    "both" => *from_id == current_id || *to_id == current_id,
                    _ => false,
                };
                if !is_match {
                    continue;
                }
                // Link type filter
                if link_type.is_some() && lt != link_type.unwrap_or("") {
                    continue;
                }

                // Determine the neighbour (the "other" side)
                let neighbor_id = if dir == "in" {
                    *from_id
                } else if dir == "both" && *to_id == current_id {
                    *from_id
                } else {
                    *to_id
                };

                let (Some(from_slug), Some(to_slug)) =
                    (id_to_slug.get(from_id), id_to_slug.get(to_id))
                else {
                    continue;
                };

                result.push(GraphPath {
                    from_slug: from_slug.clone(),
                    to_slug: to_slug.clone(),
                    link_type: lt.clone(),
                    context: ctx.clone(),
                    depth: current_depth + 1,
                });

                if !visited.contains(&neighbor_id) {
                    visited.insert(neighbor_id);
                    queue.push_back((neighbor_id, current_depth + 1));
                }
            }
        }

        Ok(result)
    }

    // MINION_JOB_METHODS_ANCHOR

    async fn enqueue_job(
        &self,
        input: &crate::minions::types::MinionJobInput,
    ) -> Result<crate::minions::types::MinionJob> {
        use crate::minions::types::MinionJobStatus;

        let pool = self.pool()?;

        // Idempotency fast path: a matching non-null key returns the existing
        // row (the unique partial index guarantees at most one).
        if let Some(ref key) = input.idempotency_key {
            let existing = sqlx::query(&format!(
                "SELECT {MINION_JOB_SELECT} FROM minion_jobs WHERE idempotency_key = $1"
            ))
            .bind(key.as_str())
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::engine(format!("enqueue_job idempotency SELECT: {e}")))?;
            if let Some(row) = existing {
                return pg_row_to_job(&row);
            }
        }

        let delay_ms = input.delay.filter(|d| *d > 0);
        let status = if delay_ms.is_some() {
            MinionJobStatus::Delayed
        } else {
            MinionJobStatus::Waiting
        };
        let max_stalled = input.max_stalled.map_or(5, |v| v.clamp(1, 100));
        let backoff_type = input
            .backoff_type
            .unwrap_or(crate::minions::types::BackoffType::Exponential);
        let on_child_fail = input
            .on_child_fail
            .unwrap_or(crate::minions::types::ChildFailPolicy::FailParent);
        let data_json = input.data.clone().unwrap_or_else(|| serde_json::json!({}));

        // D-layer: spawning under a parent must validate depth + max_children
        // and flip the parent to waiting-children atomically with the child
        // insert. A txn with `SELECT ... FOR UPDATE` on the parent row serializes
        // concurrent spawns (the PG analogue of the SQLite BEGIN IMMEDIATE).
        const MAX_SPAWN_DEPTH: i32 = 5;

        let insert_sql = format!(
            "INSERT INTO minion_jobs \
                (name, queue, status, priority, data, max_attempts, backoff_type, \
                 backoff_delay, backoff_jitter, max_stalled, delay_until, on_child_fail, \
                 max_children, timeout_ms, remove_on_complete, remove_on_fail, \
                 idempotency_key, parent_job_id, depth) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                     CASE WHEN $11::bigint IS NULL THEN NULL \
                          ELSE now() + ($11::double precision * interval '1 millisecond') END, \
                     $12, $13::int, $14, $15, $16, $17, $18, $19) \
             RETURNING {MINION_JOB_SELECT}"
        );

        if let Some(parent_id) = input.parent_job_id {
            let mut tx = pool
                .begin()
                .await
                .map_err(|e| Error::engine(format!("enqueue_job(child) BEGIN: {e}")))?;

            // Lock the parent row for the depth + max_children check.
            let parent_row = sqlx::query(
                "SELECT depth, max_children FROM minion_jobs WHERE id = $1 FOR UPDATE",
            )
            .bind(parent_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| Error::engine(format!("enqueue_job(child) parent lock: {e}")))?;
            let Some(parent_row) = parent_row else {
                return Err(crate::error::StructuredError::new(
                    "InvalidInput",
                    "invalid_input",
                    format!("parent_job_id {parent_id} not found"),
                ));
            };
            let parent_depth: i32 = parent_row
                .try_get("depth")
                .map_err(|e| Error::engine(format!("enqueue_job(child) depth decode: {e}")))?;
            let parent_max_children: Option<i32> = parent_row
                .try_get("max_children")
                .map_err(|e| Error::engine(format!("enqueue_job(child) max_children: {e}")))?;
            let depth = parent_depth + 1;
            if depth > MAX_SPAWN_DEPTH {
                return Err(crate::error::StructuredError::new(
                    "InvalidInput",
                    "invalid_input",
                    format!("spawn depth {depth} exceeds maxSpawnDepth {MAX_SPAWN_DEPTH}"),
                ));
            }
            if let Some(cap) = parent_max_children {
                let live: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM minion_jobs WHERE parent_job_id = $1 \
                     AND status NOT IN ('completed','failed','dead','cancelled')",
                )
                .bind(parent_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| Error::engine(format!("enqueue_job(child) live count: {e}")))?;
                if live >= i64::from(cap) {
                    return Err(crate::error::StructuredError::new(
                        "InvalidInput",
                        "invalid_input",
                        format!(
                            "parent {parent_id} already has {live} live children (max_children={cap})"
                        ),
                    ));
                }
            }

            let row = sqlx::query(&insert_sql)
                .bind(&input.name)
                .bind(input.queue.clone().unwrap_or_else(|| "default".to_string()))
                .bind(status.as_str())
                .bind(input.priority.unwrap_or(0))
                .bind(data_json)
                .bind(input.max_attempts.unwrap_or(3))
                .bind(backoff_type.as_str())
                .bind(input.backoff_delay.unwrap_or(1000))
                .bind(input.backoff_jitter.unwrap_or(0.2))
                .bind(max_stalled)
                .bind(delay_ms)
                .bind(on_child_fail.as_str())
                .bind(input.max_children)
                .bind(input.timeout_ms)
                .bind(input.remove_on_complete.unwrap_or(false))
                .bind(input.remove_on_fail.unwrap_or(false))
                .bind(input.idempotency_key.as_deref())
                .bind(parent_id)
                .bind(depth)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| Error::engine(format!("enqueue_job(child) INSERT: {e}")))?;

            // Flip parent to waiting-children from a runnable state.
            sqlx::query(
                "UPDATE minion_jobs SET status = 'waiting-children', updated_at = now() \
                 WHERE id = $1 AND status IN ('waiting','active','delayed')",
            )
            .bind(parent_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::engine(format!("enqueue_job(child) parent flip: {e}")))?;

            tx.commit()
                .await
                .map_err(|e| Error::engine(format!("enqueue_job(child) COMMIT: {e}")))?;
            return pg_row_to_job(&row);
        }

        // delay_until is TIMESTAMPTZ; compute it as now() + ($N ms) when delayed.
        // $11 carries the delay in ms (NULL when not delayed) and the CASE turns
        // it into an interval, matching the SQLite epoch-ms arithmetic.
        let row = sqlx::query(&format!(
            "INSERT INTO minion_jobs \
                (name, queue, status, priority, data, max_attempts, backoff_type, \
                 backoff_delay, backoff_jitter, max_stalled, delay_until, on_child_fail, \
                 max_children, timeout_ms, remove_on_complete, remove_on_fail, idempotency_key) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                     CASE WHEN $11::bigint IS NULL THEN NULL \
                          ELSE now() + ($11::double precision * interval '1 millisecond') END, \
                     $12, $13, $14::int, $15, $16, $17) \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(&input.name)
        .bind(input.queue.clone().unwrap_or_else(|| "default".to_string()))
        .bind(status.as_str())
        .bind(input.priority.unwrap_or(0))
        .bind(data_json)
        .bind(input.max_attempts.unwrap_or(3))
        .bind(backoff_type.as_str())
        .bind(input.backoff_delay.unwrap_or(1000))
        .bind(input.backoff_jitter.unwrap_or(0.2))
        .bind(max_stalled)
        .bind(delay_ms)
        .bind(on_child_fail.as_str())
        .bind(input.max_children)
        .bind(input.timeout_ms)
        .bind(input.remove_on_complete.unwrap_or(false))
        .bind(input.remove_on_fail.unwrap_or(false))
        .bind(input.idempotency_key.as_deref())
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("enqueue_job INSERT: {e}")))?;

        pg_row_to_job(&row)
    }

    // MINION_JOB_METHODS_ANCHOR

    async fn get_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;
        let row = sqlx::query(&format!(
            "SELECT {MINION_JOB_SELECT} FROM minion_jobs WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("get_job SELECT: {e}")))?;
        match row {
            Some(r) => Ok(Some(pg_row_to_job(&r)?)),
            None => Ok(None),
        }
    }

    async fn get_jobs(
        &self,
        filters: &crate::minions::types::JobFilters,
    ) -> Result<Vec<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;

        // Build a dynamic WHERE with positional binds. `$idx` advances only for
        // supplied filters so limit/offset land on the correct trailing params.
        let mut sql = format!("SELECT {MINION_JOB_SELECT} FROM minion_jobs");
        let mut clauses: Vec<String> = Vec::new();
        let mut idx = 1;
        if filters.status.is_some() {
            clauses.push(format!("status = ${idx}"));
            idx += 1;
        }
        if filters.queue.is_some() {
            clauses.push(format!("queue = ${idx}"));
            idx += 1;
        }
        if filters.name.is_some() {
            clauses.push(format!("name = ${idx}"));
            idx += 1;
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(&format!(" ORDER BY id DESC LIMIT ${idx} OFFSET ${}", idx + 1));

        let mut query = sqlx::query(&sql);
        if let Some(status) = filters.status {
            query = query.bind(status.as_str());
        }
        if let Some(ref queue) = filters.queue {
            query = query.bind(queue.clone());
        }
        if let Some(ref name) = filters.name {
            query = query.bind(name.clone());
        }
        query = query.bind(filters.limit.unwrap_or(50).max(0));
        query = query.bind(filters.offset.unwrap_or(0).max(0));

        let rows = query
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("get_jobs SELECT: {e}")))?;
        rows.iter().map(pg_row_to_job).collect()
    }

    // MINION_JOB_METHODS_ANCHOR

    async fn claim_job(
        &self,
        lock_token: &str,
        lock_duration_ms: i64,
        queue: &str,
        registered_names: &[String],
    ) -> Result<Option<crate::minions::types::MinionJob>> {
        if registered_names.is_empty() {
            return Ok(None);
        }

        let pool = self.pool()?;

        // Atomic claim via a CTE: the inner SELECT locks exactly one waiting row
        // with `FOR UPDATE SKIP LOCKED` (Postgres-native equivalent of the
        // SQLite BEGIN IMMEDIATE single-writer claim), and the UPDATE flips it to
        // active in the same statement. lock_until / timeout_at use interval
        // arithmetic; the RETURNING projection is decoded like every other read.
        let sql = format!(
            "WITH claimed AS ( \
                SELECT id AS cid FROM minion_jobs \
                WHERE queue = $1 AND status = 'waiting' AND name = ANY($2::text[]) \
                ORDER BY priority ASC, created_at ASC, id ASC \
                LIMIT 1 \
                FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE minion_jobs m SET \
                status = 'active', \
                lock_token = $3, \
                lock_until = now() + ($4::double precision * interval '1 millisecond'), \
                timeout_at = CASE WHEN m.timeout_ms IS NOT NULL \
                    THEN now() + (m.timeout_ms::double precision * interval '1 millisecond') \
                    ELSE NULL END, \
                attempts_started = m.attempts_started + 1, \
                started_at = COALESCE(m.started_at, now()), \
                updated_at = now() \
             FROM claimed \
             WHERE m.id = claimed.cid \
             RETURNING {MINION_JOB_SELECT}"
        );

        let row = sqlx::query(&sql)
            .bind(queue)
            .bind(registered_names)
            .bind(lock_token)
            .bind(lock_duration_ms)
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::engine(format!("claim_job: {e}")))?;
        match row {
            Some(r) => Ok(Some(pg_row_to_job(&r)?)),
            None => Ok(None),
        }
    }

    // MINION_JOB_METHODS_ANCHOR

    async fn complete_job(
        &self,
        id: i64,
        lock_token: &str,
        result: Option<&serde_json::Value>,
    ) -> Result<Option<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;

        // Token-fenced completion inside a txn so the parent hook (token rollup,
        // child_done emit, resolve) commits atomically with the child flip.
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| Error::engine(format!("complete_job BEGIN: {e}")))?;

        let row = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'completed', result = $1, \
                finished_at = now(), lock_token = NULL, lock_until = NULL, updated_at = now() \
             WHERE id = $2 AND status = 'active' AND lock_token = $3 \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(result.cloned())
        .bind(id)
        .bind(lock_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| Error::engine(format!("complete_job UPDATE: {e}")))?;

        let Some(r) = row else {
            tx.rollback().await.ok();
            return Ok(None);
        };
        let job = pg_row_to_job(&r)?;

        // D-layer parent hook: roll up tokens, emit child_done, resolve.
        if let Some(parent_id) = job.parent_job_id {
            if job.tokens_input > 0 || job.tokens_output > 0 || job.tokens_cache_read > 0 {
                sqlx::query(
                    "UPDATE minion_jobs SET tokens_input = tokens_input + $1, \
                        tokens_output = tokens_output + $2, \
                        tokens_cache_read = tokens_cache_read + $3, updated_at = now() \
                     WHERE id = $4 AND status NOT IN \
                        ('completed','failed','dead','cancelled')",
                )
                .bind(job.tokens_input)
                .bind(job.tokens_output)
                .bind(job.tokens_cache_read)
                .bind(parent_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| Error::engine(format!("complete_job token rollup: {e}")))?;
            }
            pg_emit_child_done(
                &mut tx,
                parent_id,
                job.id,
                &job.name,
                result.cloned().unwrap_or(serde_json::Value::Null),
                crate::minions::types::ChildOutcome::Complete,
                None,
            )
            .await?;
            pg_resolve_parent(&mut tx, parent_id).await?;
        }

        if job.remove_on_complete {
            sqlx::query("DELETE FROM minion_jobs WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(|e| Error::engine(format!("complete_job remove_on_complete: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| Error::engine(format!("complete_job COMMIT: {e}")))?;
        Ok(Some(job))
    }

    async fn fail_job(
        &self,
        id: i64,
        lock_token: &str,
        error_text: &str,
        outcome: crate::minions::types::FailOutcome,
        backoff_ms: i64,
    ) -> Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::{ChildFailPolicy, FailOutcome};

        let pool = self.pool()?;
        let new_status = outcome.as_status();
        let stacktrace = serde_json::json!([error_text]);

        // Txn so the fail flip + child_done emit + on_child_fail policy commit
        // atomically. Delayed retry sets delay_until = now + backoff and leaves
        // finished_at NULL; terminal outcomes clear delay_until and stamp
        // finished_at. The CASE arms are driven by $4 (delay flag).
        let is_delayed = outcome == FailOutcome::Delayed;
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| Error::engine(format!("fail_job BEGIN: {e}")))?;

        let row = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = $1, error_text = $2, \
                attempts_made = attempts_made + 1, stacktrace = $3, \
                delay_until = CASE WHEN $4 THEN now() + ($5::double precision * interval '1 millisecond') ELSE NULL END, \
                finished_at = CASE WHEN $4 THEN NULL ELSE now() END, \
                lock_token = NULL, lock_until = NULL, updated_at = now() \
             WHERE id = $6 AND status = 'active' AND lock_token = $7 \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(new_status.as_str())
        .bind(error_text)
        .bind(stacktrace)
        .bind(is_delayed)
        .bind(backoff_ms)
        .bind(id)
        .bind(lock_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| Error::engine(format!("fail_job UPDATE: {e}")))?;

        let Some(r) = row else {
            tx.rollback().await.ok();
            return Ok(None);
        };
        let job = pg_row_to_job(&r)?;

        // D-layer parent hook on terminal failure. Emit child_done BEFORE any
        // parent-terminal flip (the EXISTS guard on emit would drop the message
        // once the parent is failed), then apply on_child_fail.
        if outcome.is_terminal() {
            if let Some(parent_id) = job.parent_job_id {
                let child_outcome = if outcome == FailOutcome::Dead {
                    crate::minions::types::ChildOutcome::Dead
                } else {
                    crate::minions::types::ChildOutcome::Failed
                };
                pg_emit_child_done(
                    &mut tx,
                    parent_id,
                    job.id,
                    &job.name,
                    serde_json::Value::Null,
                    child_outcome,
                    Some(error_text.to_string()),
                )
                .await?;

                match job.on_child_fail {
                    ChildFailPolicy::FailParent => {
                        sqlx::query(
                            "UPDATE minion_jobs SET status = 'failed', \
                                error_text = $1, finished_at = now(), updated_at = now() \
                             WHERE id = $2 AND status = 'waiting-children'",
                        )
                        .bind(format!("child job {} failed: {error_text}", job.id))
                        .bind(parent_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| Error::engine(format!("fail_job fail_parent: {e}")))?;
                    }
                    ChildFailPolicy::RemoveDep => {
                        sqlx::query(
                            "UPDATE minion_jobs SET parent_job_id = NULL, updated_at = now() \
                             WHERE id = $1",
                        )
                        .bind(job.id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| Error::engine(format!("fail_job remove_dep: {e}")))?;
                        pg_resolve_parent(&mut tx, parent_id).await?;
                    }
                    ChildFailPolicy::Ignore | ChildFailPolicy::Continue => {
                        pg_resolve_parent(&mut tx, parent_id).await?;
                    }
                }
            }
        }

        if outcome.is_terminal() && job.remove_on_fail {
            sqlx::query("DELETE FROM minion_jobs WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(|e| Error::engine(format!("fail_job remove_on_fail: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| Error::engine(format!("fail_job COMMIT: {e}")))?;
        Ok(Some(job))
    }

    // MINION_JOB_METHODS_ANCHOR

    async fn renew_job_lock(
        &self,
        id: i64,
        lock_token: &str,
        lock_duration_ms: i64,
    ) -> Result<bool> {
        let pool = self.pool()?;
        let result = sqlx::query(
            "UPDATE minion_jobs SET \
                lock_until = now() + ($1::double precision * interval '1 millisecond'), \
                updated_at = now() \
             WHERE id = $2 AND lock_token = $3 AND status = 'active'",
        )
        .bind(lock_duration_ms)
        .bind(id)
        .bind(lock_token)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("renew_job_lock UPDATE: {e}")))?;
        Ok(result.rows_affected() > 0)
    }

    async fn retry_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;
        let row = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'waiting', error_text = NULL, \
                lock_token = NULL, lock_until = NULL, delay_until = NULL, \
                finished_at = NULL, updated_at = now() \
             WHERE id = $1 AND status IN ('failed', 'dead') \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("retry_job UPDATE: {e}")))?;
        match row {
            Some(r) => Ok(Some(pg_row_to_job(&r)?)),
            None => Ok(None),
        }
    }

    // --- Ops: pause / resume (1-1-3-3) ---

    async fn pause_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;
        let row = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'paused', \
                lock_token = NULL, lock_until = NULL, updated_at = now() \
             WHERE id = $1 AND status IN ('waiting', 'active', 'delayed') \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("pause_job UPDATE: {e}")))?;
        match row {
            Some(r) => Ok(Some(pg_row_to_job(&r)?)),
            None => Ok(None),
        }
    }

    async fn resume_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;
        let row = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'waiting', \
                lock_token = NULL, lock_until = NULL, updated_at = now() \
             WHERE id = $1 AND status = 'paused' \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("resume_job UPDATE: {e}")))?;
        match row {
            Some(r) => Ok(Some(pg_row_to_job(&r)?)),
            None => Ok(None),
        }
    }

    async fn prune_jobs(
        &self,
        statuses: &[crate::minions::types::MinionJobStatus],
        older_than_rfc3339: &str,
    ) -> Result<i64> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let pool = self.pool()?;

        // `status = ANY($1::text[])` gates the terminal set; the cutoff parses
        // the RFC-3339 string to TIMESTAMPTZ and compares against `updated_at`.
        // Child rows (inbox, attachments) go via ON DELETE CASCADE. Count the
        // deletions with a CTE so the return matches the other backends.
        let status_strs: Vec<String> =
            statuses.iter().map(|s| s.as_str().to_string()).collect();
        let count: i64 = sqlx::query_scalar(
            "WITH pruned AS ( \
                 DELETE FROM minion_jobs \
                 WHERE status = ANY($1::text[]) AND updated_at < $2::timestamptz \
                 RETURNING id \
             ) SELECT count(*) FROM pruned",
        )
        .bind(&status_strs)
        .bind(older_than_rfc3339)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("prune_jobs DELETE: {e}")))?;
        Ok(count)
    }

    async fn get_stats(
        &self,
        since_rfc3339: &str,
    ) -> Result<crate::minions::types::QueueStats> {
        use crate::minions::types::{QueueHealth, QueueStats, QueueTypeStat};
        use std::collections::BTreeMap;

        let pool = self.pool()?;

        // by_status: all-time count per status.
        let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
        let status_rows =
            sqlx::query("SELECT status, count(*) AS count FROM minion_jobs GROUP BY status")
                .fetch_all(pool)
                .await
                .map_err(|e| Error::engine(format!("get_stats by_status: {e}")))?;
        for row in &status_rows {
            let status: String = row
                .try_get("status")
                .map_err(|e| Error::engine(format!("status decode: {e}")))?;
            let count: i64 = row
                .try_get("count")
                .map_err(|e| Error::engine(format!("count decode: {e}")))?;
            by_status.insert(status, count);
        }

        // by_type: per-name breakdown in the `since` window, using FILTER for
        // terminal counts and EXTRACT(EPOCH ...) * 1000 for mean runtime (ms).
        // Mirrors TS getStats. `created_at >= $1::timestamptz` bounds the window.
        let type_rows = sqlx::query(
            "SELECT name, \
                count(*) AS total, \
                count(*) FILTER (WHERE status = 'completed') AS completed, \
                count(*) FILTER (WHERE status = 'failed') AS failed, \
                count(*) FILTER (WHERE status = 'dead') AS dead, \
                (avg(EXTRACT(EPOCH FROM (finished_at - started_at)) * 1000) \
                    FILTER (WHERE finished_at IS NOT NULL AND started_at IS NOT NULL) \
                )::double precision AS avg_duration_ms \
             FROM minion_jobs WHERE created_at >= $1::timestamptz \
             GROUP BY name ORDER BY total DESC, name ASC",
        )
        .bind(since_rfc3339)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_stats by_type: {e}")))?;
        let mut by_type: Vec<QueueTypeStat> = Vec::with_capacity(type_rows.len());
        for row in &type_rows {
            // avg is double precision (or NULL); round to the nearest ms.
            let avg_duration_ms = row
                .try_get::<Option<f64>, _>("avg_duration_ms")
                .map_err(|e| Error::engine(format!("avg_duration decode: {e}")))?
                .map(|v| v.round() as i64);
            by_type.push(QueueTypeStat {
                name: row
                    .try_get("name")
                    .map_err(|e| Error::engine(format!("name decode: {e}")))?,
                total: row
                    .try_get("total")
                    .map_err(|e| Error::engine(format!("total decode: {e}")))?,
                completed: row
                    .try_get("completed")
                    .map_err(|e| Error::engine(format!("completed decode: {e}")))?,
                failed: row
                    .try_get("failed")
                    .map_err(|e| Error::engine(format!("failed decode: {e}")))?,
                dead: row
                    .try_get("dead")
                    .map_err(|e| Error::engine(format!("dead decode: {e}")))?,
                avg_duration_ms,
            });
        }

        // queue_health: stalled = active jobs with an expired lease.
        let stalled: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM minion_jobs \
             WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < now()",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_stats stalled: {e}")))?;

        Ok(QueueStats {
            queue_health: QueueHealth {
                waiting: by_status.get("waiting").copied().unwrap_or(0),
                active: by_status.get("active").copied().unwrap_or(0),
                stalled,
            },
            by_status,
            by_type,
        })
    }

    async fn get_brain_stats(&self) -> Result<crate::admin_queries::BrainStats> {
        use crate::admin_queries::BrainStats;
        use std::collections::BTreeMap;

        let pool = self.pool()?;

        let page_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pages WHERE deleted_at IS NULL")
                .fetch_one(pool)
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats page_count: {e}")))?;

        // No content_chunks table in Rust — approximate chunk_count as live
        // pages carrying non-empty compiled_truth. Registered in
        // docs/plans/KNOWN-GAPS.md (G46).
        let chunk_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pages \
             WHERE compiled_truth IS NOT NULL AND compiled_truth != '' AND deleted_at IS NULL",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_brain_stats chunk_count: {e}")))?;

        // embedded_count: page-level embedding (G24) — live pages with a vector.
        let embedded_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pages WHERE embedding IS NOT NULL AND deleted_at IS NULL",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_brain_stats embedded_count: {e}")))?;

        let link_count: i64 = sqlx::query_scalar("SELECT count(*) FROM links")
            .fetch_one(pool)
            .await
            .map_err(|e| Error::engine(format!("get_brain_stats link_count: {e}")))?;

        let tag_count: i64 =
            sqlx::query_scalar("SELECT count(DISTINCT tag) FROM page_tags")
                .fetch_one(pool)
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats tag_count: {e}")))?;

        // timeline is a JSON-array text column; sum array lengths on the Rust
        // side (no timeline_entries table).
        let timeline_rows =
            sqlx::query_scalar::<_, String>("SELECT timeline FROM pages WHERE deleted_at IS NULL")
                .fetch_all(pool)
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats timeline: {e}")))?;
        let mut timeline_entry_count = 0i64;
        for tl in &timeline_rows {
            if let Ok(serde_json::Value::Array(arr)) =
                serde_json::from_str::<serde_json::Value>(tl)
            {
                timeline_entry_count += arr.len() as i64;
            }
        }

        // pages_by_type mirrors TS: grouped over ALL pages (no soft-delete
        // filter). Only page_count above excludes soft-deleted.
        let type_rows = sqlx::query("SELECT type, count(*) AS count FROM pages GROUP BY type")
            .fetch_all(pool)
            .await
            .map_err(|e| Error::engine(format!("get_brain_stats pages_by_type: {e}")))?;
        let mut pages_by_type: BTreeMap<String, i64> = BTreeMap::new();
        for row in &type_rows {
            let ty: String = row
                .try_get("type")
                .map_err(|e| Error::engine(format!("get_brain_stats type decode: {e}")))?;
            let cnt: i64 = row
                .try_get("count")
                .map_err(|e| Error::engine(format!("get_brain_stats type count decode: {e}")))?;
            pages_by_type.insert(ty, cnt);
        }

        Ok(BrainStats {
            page_count,
            chunk_count,
            embedded_count,
            link_count,
            tag_count,
            timeline_entry_count,
            pages_by_type,
        })
    }

    async fn get_health(&self) -> Result<crate::autopilot::brain_score::BrainHealth> {
        use crate::autopilot::brain_score::{BrainHealth, MostConnectedEntry};

        let pool = self.pool()?;

        // Backend-model note (see BrainStats docs / KNOWN-GAPS G24, G46):
        // no content_chunks / timeline_entries tables — embedding coverage is
        // page-level (one BLOB per page, G24), timeline is a JSON-array text
        // column parsed Rust-side. Soft-deleted pages excluded (deleted_at IS
        // NULL), matching InMemory `live_pages`; dead_links is deleted-aware.
        // orphan_pages = islanded (no inbound AND no outbound link). Mirrors
        // the libsql `get_health` and InMemory engine.rs semantics.

        let page_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pages WHERE deleted_at IS NULL")
                .fetch_one(pool)
                .await
                .map_err(|e| Error::engine(format!("get_health page_count: {e}")))?;

        let missing_embeddings: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pages WHERE deleted_at IS NULL AND embedding IS NULL",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_health missing_embeddings: {e}")))?;

        let link_count: i64 = sqlx::query_scalar("SELECT count(*) FROM links")
            .fetch_one(pool)
            .await
            .map_err(|e| Error::engine(format!("get_health link_count: {e}")))?;

        let dead_links: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM links l \
             WHERE NOT EXISTS (SELECT 1 FROM pages p \
               WHERE p.id = l.to_page_id AND p.deleted_at IS NULL)",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_health dead_links: {e}")))?;

        let orphan_pages: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pages p WHERE p.deleted_at IS NULL \
               AND NOT EXISTS (SELECT 1 FROM links l WHERE l.to_page_id = p.id) \
               AND NOT EXISTS (SELECT 1 FROM links l WHERE l.from_page_id = p.id)",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_health orphan_pages: {e}")))?;

        let entity_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pages \
             WHERE deleted_at IS NULL AND type IN ('person', 'company')",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_health entity_count: {e}")))?;

        let entities_with_inbound: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pages e WHERE e.deleted_at IS NULL \
               AND e.type IN ('person', 'company') \
               AND EXISTS (SELECT 1 FROM links l WHERE l.to_page_id = e.id)",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("get_health entities_with_inbound: {e}")))?;

        // Timeline (Rust-side JSON parse): a page "has timeline" iff its
        // JSON-array string column is a non-empty array.
        let timeline_rows = sqlx::query_as::<_, (String, String)>(
            "SELECT type, timeline FROM pages WHERE deleted_at IS NULL",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_health timeline: {e}")))?;
        let mut pages_with_timeline = 0i64;
        let mut entities_with_timeline = 0i64;
        for (ty, tl) in &timeline_rows {
            let has_tl = matches!(
                serde_json::from_str::<serde_json::Value>(tl),
                Ok(serde_json::Value::Array(ref a)) if !a.is_empty()
            );
            if has_tl {
                pages_with_timeline += 1;
                if ty == "person" || ty == "company" {
                    entities_with_timeline += 1;
                }
            }
        }

        // most_connected: top 5 entities by (in + out) link count, excluding
        // zero-link entities. Deterministic tie-break by slug.
        let connected_rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT slug, lc FROM ( \
               SELECT p.slug AS slug, \
                      (SELECT count(*) FROM links l \
                         WHERE l.from_page_id = p.id OR l.to_page_id = p.id) AS lc \
               FROM pages p \
               WHERE p.deleted_at IS NULL AND p.type IN ('person', 'company') \
             ) sub WHERE lc > 0 \
             ORDER BY lc DESC, slug ASC LIMIT 5",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_health most_connected: {e}")))?;
        let most_connected: Vec<MostConnectedEntry> = connected_rows
            .into_iter()
            .map(|(slug, lc)| MostConnectedEntry {
                slug,
                link_count: lc.max(0) as usize,
            })
            .collect();

        // ── Derived ratios ────────────────────────────────────────────────
        let embedded_pages = (page_count - missing_embeddings).max(0);
        let embed_coverage = if page_count > 0 {
            embedded_pages as f64 / page_count as f64
        } else {
            1.0
        };
        let link_coverage = if entity_count > 0 {
            entities_with_inbound as f64 / entity_count as f64
        } else {
            0.0
        };
        let timeline_coverage = if entity_count > 0 {
            entities_with_timeline as f64 / entity_count as f64
        } else {
            0.0
        };

        // ── Score computation (mirrors InMemory / libsql) ─────────────────
        let (
            embed_coverage_score,
            link_density_score,
            timeline_coverage_score,
            no_orphans_score,
            no_dead_links_score,
        ) = if page_count == 0 {
            (35u32, 25u32, 15u32, 15u32, 10u32)
        } else {
            let pc = page_count as f64;
            let link_density = (link_count as f64 / pc).min(1.0);
            let timeline_density = (pages_with_timeline as f64 / pc).min(1.0);
            let no_orphans = 1.0 - (orphan_pages as f64 / pc);
            let no_dead = 1.0 - (dead_links as f64 / pc).min(1.0);
            (
                (embed_coverage * 35.0).round() as u32,
                (link_density * 25.0).round() as u32,
                (timeline_density * 15.0).round() as u32,
                (no_orphans * 15.0).round() as u32,
                (no_dead * 10.0).round() as u32,
            )
        };
        let brain_score = BrainHealth::compute_brain_score(
            embed_coverage_score,
            link_density_score,
            timeline_coverage_score,
            no_orphans_score,
            no_dead_links_score,
        );

        Ok(BrainHealth {
            page_count: page_count.max(0) as usize,
            embed_coverage,
            stale_pages: 0,
            orphan_pages: orphan_pages.max(0) as usize,
            missing_embeddings: missing_embeddings.max(0) as usize,
            brain_score,
            dead_links: dead_links.max(0) as usize,
            link_coverage,
            timeline_coverage,
            most_connected,
            embed_coverage_score,
            link_density_score,
            timeline_coverage_score,
            no_orphans_score,
            no_dead_links_score,
        })
    }

    async fn health_check(
        &self,
    ) -> Result<crate::minions::types::SupervisorHealth> {
        use crate::minions::types::SupervisorHealth;

        let pool = self.pool()?;

        // stalled: active jobs with expired lease (lock_until < now).
        let stalled_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM minion_jobs \
             WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < now()",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("health_check stalled: {e}")))?;

        // waiting: jobs in waiting status (same as queue_health.waiting).
        let waiting_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM minion_jobs WHERE status = 'waiting'",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("health_check waiting: {e}")))?;

        // last_completed_at: finished_at of most recently completed job.
        // TIMESTAMPTZ cast to RFC-3339 text.
        let last_completed_at: Option<String> = sqlx::query_scalar(
            "SELECT to_char(finished_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM minion_jobs \
             WHERE status = 'completed' AND finished_at IS NOT NULL \
             ORDER BY finished_at DESC LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("health_check last_completed: {e}")))?;

        Ok(SupervisorHealth {
            stalled_count,
            waiting_count,
            last_completed_at,
        })
    }


    //
    // Each sweep is a single `UPDATE ... RETURNING` against the TIMESTAMPTZ
    // scheduling columns, compared to `now()`. Pure C-layer state transitions
    // only: no inbox insert / parent unblock (D-layer, 1-1-3).

    async fn promote_delayed(&self) -> Result<Vec<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;
        let rows = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'waiting', delay_until = NULL, \
                lock_token = NULL, lock_until = NULL, updated_at = now() \
             WHERE status = 'delayed' AND delay_until IS NOT NULL AND delay_until <= now() \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("promote_delayed UPDATE: {e}")))?;
        rows.iter().map(pg_row_to_job).collect()
    }

    async fn handle_timeouts(&self) -> Result<Vec<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;
        // Active, per-job timeout elapsed, lease still held. A stalled job with
        // an expired lease (lock_until < now) is left for handle_stalled.
        let rows = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'dead', error_text = 'timeout exceeded', \
                lock_token = NULL, lock_until = NULL, finished_at = now(), updated_at = now() \
             WHERE status = 'active' AND timeout_at IS NOT NULL AND timeout_at < now() \
               AND lock_until IS NOT NULL AND lock_until > now() \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("handle_timeouts UPDATE: {e}")))?;
        rows.iter().map(pg_row_to_job).collect()
    }

    async fn handle_stalled(&self) -> Result<crate::minions::types::StalledSweep> {
        use crate::minions::types::StalledSweep;

        let pool = self.pool()?;

        // Stalled candidates = active with an expired lease. Partition on
        // `stalled_counter + 1 < max_stalled` (requeue, bump + waiting) vs
        // `>=` (dead-letter, bump + dead + reason). Two UPDATE ... RETURNING
        // statements mirror the InMemory/libsql grouping; the WHERE guards are
        // disjoint so a job lands in exactly one arm within a single sweep.
        let requeued = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'waiting', \
                stalled_counter = stalled_counter + 1, \
                lock_token = NULL, lock_until = NULL, updated_at = now() \
             WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < now() \
               AND stalled_counter + 1 < max_stalled \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("handle_stalled requeue UPDATE: {e}")))?;

        let dead = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'dead', \
                stalled_counter = stalled_counter + 1, \
                error_text = 'max stalled count exceeded', \
                lock_token = NULL, lock_until = NULL, \
                finished_at = now(), updated_at = now() \
             WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < now() \
               AND stalled_counter + 1 >= max_stalled \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("handle_stalled dead UPDATE: {e}")))?;

        Ok(StalledSweep {
            requeued: requeued
                .iter()
                .map(pg_row_to_job)
                .collect::<Result<Vec<_>>>()?,
            dead: dead.iter().map(pg_row_to_job).collect::<Result<Vec<_>>>()?,
        })
    }

    async fn handle_wall_clock_timeouts(
        &self,
        lock_duration_ms: i64,
    ) -> Result<Vec<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;
        // Threshold computed in SQL. EXTRACT(EPOCH FROM (now()-started_at))*1000
        // = elapsed ms. CASE: timeout_ms present -> timeout_ms*2; else
        // lock_duration_ms*2*GREATEST(max_stalled, 1). Ignores lease state —
        // catches jobs wedged while holding a DB resource.
        let rows = sqlx::query(&format!(
            "UPDATE minion_jobs SET status = 'dead', \
                error_text = 'wall-clock timeout exceeded', \
                lock_token = NULL, lock_until = NULL, finished_at = now(), updated_at = now() \
             WHERE status = 'active' AND started_at IS NOT NULL \
               AND EXTRACT(EPOCH FROM (now() - started_at)) * 1000 > \
                   CASE WHEN timeout_ms IS NOT NULL THEN timeout_ms * 2 \
                        ELSE $1::double precision * 2 * GREATEST(max_stalled, 1) END \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(lock_duration_ms)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("handle_wall_clock_timeouts UPDATE: {e}")))?;
        rows.iter().map(pg_row_to_job).collect()
    }

    async fn set_started_at_for_test(&self, id: i64, started_at_rfc3339: &str) -> Result<()> {
        let pool = self.pool()?;
        sqlx::query("UPDATE minion_jobs SET started_at = $1::timestamptz WHERE id = $2")
            .bind(started_at_rfc3339)
            .bind(id as i32)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("set_started_at_for_test UPDATE: {e}")))?;
        Ok(())
    }

    async fn set_timeout_at_for_test(&self, id: i64, timeout_at_ms: i64) -> Result<()> {
        let pool = self.pool()?;
        // epoch-ms bigint -> TIMESTAMPTZ via to_timestamp(seconds).
        sqlx::query(
            "UPDATE minion_jobs SET timeout_at = to_timestamp($1::double precision / 1000.0) \
             WHERE id = $2",
        )
        .bind(timeout_at_ms)
        .bind(id as i32)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("set_timeout_at_for_test UPDATE: {e}")))?;
        Ok(())
    }

    // ─── D-layer: cancellation + inbox (parent/child coordination) ──────────

    async fn cancel_job(
        &self,
        id: i64,
    ) -> Result<Option<crate::minions::types::MinionJob>> {
        let pool = self.pool()?;

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| Error::engine(format!("cancel_job BEGIN: {e}")))?;

        // Cancel the whole descendant subtree in one recursive CTE, returning
        // the rows that actually transitioned (only non-terminal ones). The
        // depth guard (lvl < 100) bounds pathological cycles.
        let rows = sqlx::query(&format!(
            "WITH RECURSIVE subtree(id, lvl) AS ( \
                SELECT id, 0 FROM minion_jobs WHERE id = $1 \
                UNION ALL \
                SELECT j.id, s.lvl + 1 FROM minion_jobs j \
                JOIN subtree s ON j.parent_job_id = s.id \
                WHERE s.lvl < 100 \
             ) \
             UPDATE minion_jobs SET status = 'cancelled', \
                lock_token = NULL, lock_until = NULL, \
                finished_at = now(), updated_at = now() \
             WHERE id IN (SELECT id FROM subtree) \
               AND status NOT IN ('completed','failed','dead','cancelled') \
             RETURNING {MINION_JOB_SELECT}"
        ))
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| Error::engine(format!("cancel_job UPDATE: {e}")))?;

        let cancelled: Vec<crate::minions::types::MinionJob> =
            rows.iter().map(pg_row_to_job).collect::<Result<_>>()?;

        // TS contract: return the root only if IT transitioned this call. An
        // already-terminal root (not in the RETURNING set) yields None.
        let Some(root) = cancelled.iter().find(|j| j.id == id).cloned() else {
            tx.rollback().await.ok();
            return Ok(None);
        };

        // Emit child_done(cancelled) to each affected parent, then resolve.
        let mut parent_ids: Vec<i64> = Vec::new();
        for job in &cancelled {
            if let Some(pid) = job.parent_job_id {
                pg_emit_child_done(
                    &mut tx,
                    pid,
                    job.id,
                    &job.name,
                    serde_json::Value::Null,
                    crate::minions::types::ChildOutcome::Cancelled,
                    Some("cancelled".to_string()),
                )
                .await?;
                if !parent_ids.contains(&pid) {
                    parent_ids.push(pid);
                }
            }
        }
        for pid in parent_ids {
            pg_resolve_parent(&mut tx, pid).await?;
        }

        tx.commit()
            .await
            .map_err(|e| Error::engine(format!("cancel_job COMMIT: {e}")))?;
        Ok(Some(root))
    }

    async fn send_message(
        &self,
        job_id: i64,
        payload: &serde_json::Value,
        sender: &str,
    ) -> Result<Option<crate::minions::types::InboxMessage>> {
        let pool = self.pool()?;

        // Target must exist and be non-terminal; sender must be 'admin' or the
        // job's parent id string.
        let Some(job) = self.get_job(job_id).await? else {
            return Ok(None);
        };
        if job.status.is_terminal() {
            return Ok(None);
        }
        let parent_str = job.parent_job_id.map(|p| p.to_string());
        if sender != "admin" && Some(sender.to_string()) != parent_str {
            return Ok(None);
        }

        let row = sqlx::query(
            "INSERT INTO minion_inbox (job_id, sender, payload) VALUES ($1, $2, $3) \
             RETURNING id::bigint AS id, sent_at::text AS sent_at",
        )
        .bind(job_id)
        .bind(sender)
        .bind(payload.clone())
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("send_message INSERT: {e}")))?;

        Ok(Some(crate::minions::types::InboxMessage {
            id: row
                .try_get("id")
                .map_err(|e| Error::engine(format!("send_message id decode: {e}")))?,
            job_id,
            sender: sender.to_string(),
            payload: payload.clone(),
            sent_at: row
                .try_get("sent_at")
                .map_err(|e| Error::engine(format!("send_message sent_at decode: {e}")))?,
            read_at: None,
        }))
    }

    async fn read_inbox(
        &self,
        job_id: i64,
        lock_token: &str,
    ) -> Result<Vec<crate::minions::types::InboxMessage>> {
        let pool = self.pool()?;

        // Token fence: caller must hold the active lease. A single statement
        // marks + returns the unread rows guarded by an EXISTS on the lease so
        // the fence and the consume are atomic.
        let rows = sqlx::query(
            "UPDATE minion_inbox SET read_at = now() \
             WHERE job_id = $1 AND read_at IS NULL \
               AND EXISTS (SELECT 1 FROM minion_jobs \
                   WHERE id = $1 AND status = 'active' AND lock_token = $2) \
             RETURNING id::bigint AS id, job_id::bigint AS job_id, sender, payload, \
                       sent_at::text AS sent_at, read_at::text AS read_at",
        )
        .bind(job_id)
        .bind(lock_token)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("read_inbox UPDATE: {e}")))?;

        // Preserve send order (RETURNING order is unspecified).
        let mut out: Vec<crate::minions::types::InboxMessage> =
            rows.iter().map(pg_row_to_inbox).collect::<Result<_>>()?;
        out.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    async fn read_child_completions(
        &self,
        parent_id: i64,
        lock_token: &str,
        since_rfc3339: Option<&str>,
    ) -> Result<Vec<crate::minions::types::ChildDoneMessage>> {
        let pool = self.pool()?;

        // Same token fence as read_inbox; no marking read. `since` filters on
        // sent_at (parsed to TIMESTAMPTZ). Ordered by send order.
        let rows = sqlx::query(
            "SELECT payload FROM minion_inbox \
             WHERE job_id = $1 AND payload->>'type' = 'child_done' \
               AND ($3::text IS NULL OR sent_at > $3::timestamptz) \
               AND EXISTS (SELECT 1 FROM minion_jobs \
                   WHERE id = $1 AND status = 'active' AND lock_token = $2) \
             ORDER BY sent_at, id",
        )
        .bind(parent_id)
        .bind(lock_token)
        .bind(since_rfc3339)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("read_child_completions SELECT: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let payload: serde_json::Value = row
                .try_get("payload")
                .map_err(|e| Error::engine(format!("read_child_completions decode: {e}")))?;
            if let Ok(msg) = serde_json::from_value(payload) {
                out.push(msg);
            }
        }
        Ok(out)
    }

    async fn update_tokens(
        &self,
        id: i64,
        lock_token: &str,
        tokens: &crate::minions::types::TokenUpdate,
    ) -> Result<bool> {
        let pool = self.pool()?;
        // Token-fenced: only an active job holding this lease accrues tokens.
        let affected = sqlx::query(
            "UPDATE minion_jobs SET tokens_input = tokens_input + $1, \
                tokens_output = tokens_output + $2, \
                tokens_cache_read = tokens_cache_read + $3, updated_at = now() \
             WHERE id = $4 AND status = 'active' AND lock_token = $5",
        )
        .bind(tokens.input.unwrap_or(0))
        .bind(tokens.output.unwrap_or(0))
        .bind(tokens.cache_read.unwrap_or(0))
        .bind(id)
        .bind(lock_token)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("update_tokens UPDATE: {e}")))?;
        Ok(affected.rows_affected() > 0)
    }

    async fn remove_child_dependency(&self, child_id: i64) -> Result<()> {
        let pool = self.pool()?;
        sqlx::query("UPDATE minion_jobs SET parent_job_id = NULL, updated_at = now() WHERE id = $1")
            .bind(child_id)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("remove_child_dependency UPDATE: {e}")))?;
        Ok(())
    }

    // ─── Minion attachments (1-1-3-2) ────────────────────────────────────────

    async fn insert_attachment(
        &self,
        job_id: i64,
        att: &crate::minions::types::NormalizedAttachment,
    ) -> Result<crate::minions::types::Attachment> {
        let pool = self.pool()?;

        // Verify the parent job exists (explicit clearer error than the FK).
        if self.get_job(job_id).await?.is_none() {
            return Err(Error::new(
                "NotFound",
                "not_found",
                format!("job {job_id} not found"),
            ));
        }

        // storage_uri left to its NULL default (inline content only). size_bytes
        // and id are INT4/SERIAL → cast ::bigint so try_get::<i64> matches;
        // created_at TIMESTAMPTZ → ::text for the RFC-3339 record string.
        // External-storage path registered in docs/plans/KNOWN-GAPS.md (G27).
        let row = sqlx::query(
            "INSERT INTO minion_attachments \
             (job_id, filename, content_type, content, size_bytes, sha256) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id::bigint AS id, created_at::text AS created_at",
        )
        .bind(job_id)
        .bind(&att.filename)
        .bind(&att.content_type)
        .bind(&att.bytes)
        .bind(att.size_bytes)
        .bind(&att.sha256)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::engine(format!("insert_attachment INSERT: {e}")))?;

        Ok(crate::minions::types::Attachment {
            id: row
                .try_get("id")
                .map_err(|e| Error::engine(format!("insert_attachment id decode: {e}")))?,
            job_id,
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            storage_uri: None,
            size_bytes: att.size_bytes,
            sha256: att.sha256.clone(),
            created_at: row
                .try_get("created_at")
                .map_err(|e| Error::engine(format!("insert_attachment created_at decode: {e}")))?,
        })
    }

    async fn list_attachment_filenames(&self, job_id: i64) -> Result<Vec<String>> {
        let pool = self.pool()?;
        let rows =
            sqlx::query("SELECT filename FROM minion_attachments WHERE job_id = $1")
                .bind(job_id)
                .fetch_all(pool)
                .await
                .map_err(|e| Error::engine(format!("list_attachment_filenames: {e}")))?;
        rows.iter()
            .map(|row| {
                row.try_get::<String, _>("filename")
                    .map_err(|e| Error::engine(format!("attachment filename decode: {e}")))
            })
            .collect()
    }

    async fn list_attachments(
        &self,
        job_id: i64,
    ) -> Result<Vec<crate::minions::types::Attachment>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT id::bigint AS id, job_id::bigint AS job_id, filename, content_type, \
                    storage_uri, size_bytes::bigint AS size_bytes, sha256, \
                    created_at::text AS created_at \
             FROM minion_attachments WHERE job_id = $1 ORDER BY created_at ASC, id ASC",
        )
        .bind(job_id)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("list_attachments: {e}")))?;
        rows.iter().map(pg_row_to_attachment).collect()
    }

    async fn get_attachment(
        &self,
        job_id: i64,
        filename: &str,
    ) -> Result<Option<(crate::minions::types::Attachment, Vec<u8>)>> {
        let pool = self.pool()?;
        let row = sqlx::query(
            "SELECT id::bigint AS id, job_id::bigint AS job_id, filename, content_type, \
                    storage_uri, size_bytes::bigint AS size_bytes, sha256, \
                    created_at::text AS created_at, content \
             FROM minion_attachments WHERE job_id = $1 AND filename = $2",
        )
        .bind(job_id)
        .bind(filename)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("get_attachment: {e}")))?;

        let Some(row) = row else {
            return Ok(None);
        };
        let meta = pg_row_to_attachment(&row)?;
        // content BYTEA → Vec<u8>; NULL (external-storage rows) → empty bytes.
        let bytes: Vec<u8> = row
            .try_get::<Option<Vec<u8>>, _>("content")
            .map_err(|e| Error::engine(format!("attachment content decode: {e}")))?
            .unwrap_or_default();
        Ok(Some((meta, bytes)))
    }

    async fn delete_attachment(&self, job_id: i64, filename: &str) -> Result<bool> {
        let pool = self.pool()?;
        let affected =
            sqlx::query("DELETE FROM minion_attachments WHERE job_id = $1 AND filename = $2")
                .bind(job_id)
                .bind(filename)
                .execute(pool)
                .await
                .map_err(|e| Error::engine(format!("delete_attachment: {e}")))?;
        Ok(affected.rows_affected() > 0)
    }

    // ─── Minion budget management (roadmap 1-3-2) ──────────────────────────

    async fn reserve_budget(
        &self,
        job_id: i64,
        amount_cents: i64,
        reason: &str,
    ) -> Result<crate::minions::types::ReservationOutcome> {
        use crate::minions::types::ReservationOutcome;
        let pool = self.pool()?;

        let row = sqlx::query(
            "SELECT budget_remaining_cents, budget_owner_job_id FROM minion_jobs WHERE id = $1",
        )
        .bind(job_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("reserve_budget query: {e}")))?;

        // PG stores these as INTEGER (INT4). Read as i32 then widen to i64
        // to match the trait signature.
        let (remaining_i32, owner_i32): (Option<i32>, Option<i32>) = match row {
            None => return Err(Error::engine(format!("reserve_budget: job {job_id} not found"))),
            Some(r) => (r.get(0), r.get(1)),
        };

        let remaining: Option<i64> = remaining_i32.map(i64::from);
        let owner_id: Option<i64> = owner_i32.map(i64::from);

        let remaining = match remaining {
            None => return Ok(ReservationOutcome::NoBudget),
            Some(r) => r,
        };

        if owner_id.is_none() {
            return Ok(ReservationOutcome::OwnerDeleted);
        }

        if remaining < amount_cents {
            return Ok(ReservationOutcome::Exhausted);
        }

        let affected = sqlx::query(
            "UPDATE minion_jobs \
             SET budget_remaining_cents = budget_remaining_cents - $1 \
             WHERE id = $2 AND budget_remaining_cents >= $1",
        )
        .bind(amount_cents)
        .bind(job_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("reserve_budget update: {e}")))?;

        if affected.rows_affected() == 0 {
            return Ok(ReservationOutcome::Exhausted);
        }

        if let Err(e) = self.log_budget_event(job_id, amount_cents, reason).await {
            tracing::warn!(job_id, amount_cents, reason, error = %e, "log_budget_event failed in reserve_budget");
        }

        Ok(ReservationOutcome::Reserved)
    }

    async fn refund_budget(
        &self,
        job_id: i64,
        amount_cents: i64,
        reason: &str,
    ) -> Result<()> {
        let pool = self.pool()?;

        sqlx::query(
            "UPDATE minion_jobs \
             SET budget_remaining_cents = budget_remaining_cents + $1 \
             WHERE id = $2 AND budget_remaining_cents IS NOT NULL",
        )
        .bind(amount_cents)
        .bind(job_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("refund_budget: {e}")))?;

        if let Err(e) = self.log_budget_event(job_id, -amount_cents, reason).await {
            tracing::warn!(job_id, amount_cents, reason, error = %e, "log_budget_event failed in refund_budget");
        }

        Ok(())
    }

    async fn set_owner_budget(
        &self,
        job_id: i64,
        budget_cents: i64,
    ) -> Result<()> {
        let pool = self.pool()?;

        sqlx::query(
            "UPDATE minion_jobs \
             SET budget_remaining_cents = $1, budget_owner_job_id = $2 \
             WHERE id = $2",
        )
        .bind(budget_cents)
        .bind(job_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("set_owner_budget: {e}")))?;

        Ok(())
    }

    async fn halt_budget_subtree(
        &self,
        owner_job_id: i64,
    ) -> Result<i64> {
        let pool = self.pool()?;

        let affected = sqlx::query(
            "UPDATE minion_jobs \
             SET budget_remaining_cents = NULL \
             WHERE budget_owner_job_id = $1",
        )
        .bind(owner_job_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("halt_budget_subtree: {e}")))?;

        Ok(affected.rows_affected() as i64)
    }

    async fn inherit_budget_owner(
        &self,
        job_id: i64,
        new_owner_job_id: i64,
    ) -> Result<()> {
        let pool = self.pool()?;

        sqlx::query(
            "UPDATE minion_jobs \
             SET budget_owner_job_id = $1 \
             WHERE id = $2 AND budget_owner_job_id IS NOT NULL",
        )
        .bind(new_owner_job_id)
        .bind(job_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("inherit_budget_owner: {e}")))?;

        Ok(())
    }

    async fn get_budget_owner(
        &self,
        job_id: i64,
    ) -> Result<Option<i64>> {
        let pool = self.pool()?;

        let owner_i32: Option<i32> = sqlx::query_scalar(
            "SELECT budget_owner_job_id FROM minion_jobs WHERE id = $1",
        )
        .bind(job_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("get_budget_owner: {e}")))?
        .flatten();

        Ok(owner_i32.map(i64::from))
    }

    async fn acquire_rate_lease(
        &self,
        key: &str,
        job_id: i64,
        max_concurrent: i32,
        ttl_ms: i64,
    ) -> Result<crate::minions::types::LeaseAcquireResult> {
        use crate::minions::types::LeaseAcquireResult;
        let pool = self.pool()?;
        let hash = fnv1a_64(key);

        let mut tx = pool.begin().await.map_err(|e| {
            Error::engine(format!("acquire_rate_lease: begin tx: {e}"))
        })?;

        // Advisory lock serialises concurrent acquires on the same key.
        // xact_lock means the lock is auto-released on commit/rollback.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(hash)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                Error::engine(format!("acquire_rate_lease: advisory lock: {e}"))
            })?;

        // Prune expired leases so crashed workers don't permanently occupy slots.
        sqlx::query(
            "DELETE FROM subagent_rate_leases WHERE key = $1 AND expires_at <= now()",
        )
        .bind(key)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            Error::engine(format!("acquire_rate_lease: delete expired: {e}"))
        })?;

        // Count active leases after pruning.
        let active_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM subagent_rate_leases WHERE key = $1",
        )
        .bind(key)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            Error::engine(format!("acquire_rate_lease: count: {e}"))
        })?;

        let active_count = active_count as i32;

        if active_count >= max_concurrent {
            tx.commit().await.map_err(|e| {
                Error::engine(format!("acquire_rate_lease: commit (full): {e}"))
            })?;
            return Ok(LeaseAcquireResult {
                acquired: false,
                lease_id: None,
                active_count,
                max_concurrent,
            });
        }

        // Grant the lease.
        let ttl_seconds = (ttl_ms as f64 / 1000.0).ceil() as i32;
        let lease_id: i64 = sqlx::query_scalar(
            "INSERT INTO subagent_rate_leases (key, owner_job_id, expires_at) \
             VALUES ($1, $2, now() + make_interval(secs => $3)) RETURNING id",
        )
        .bind(key)
        .bind(job_id)
        .bind(ttl_seconds)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            Error::engine(format!("acquire_rate_lease: insert: {e}"))
        })?;

        tx.commit().await.map_err(|e| {
            Error::engine(format!("acquire_rate_lease: commit: {e}"))
        })?;

        Ok(LeaseAcquireResult {
            acquired: true,
            lease_id: Some(lease_id),
            active_count: active_count + 1,
            max_concurrent,
        })
    }

    async fn renew_rate_lease(
        &self,
        lease_id: i64,
        ttl_ms: i64,
    ) -> Result<bool> {
        let pool = self.pool()?;
        let ttl_seconds = (ttl_ms as f64 / 1000.0).ceil() as i32;

        let updated: Option<i64> = sqlx::query_scalar(
            "UPDATE subagent_rate_leases \
             SET expires_at = now() + make_interval(secs => $1) \
             WHERE id = $2 RETURNING id",
        )
        .bind(ttl_seconds)
        .bind(lease_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("renew_rate_lease: {e}")))?;

        Ok(updated.is_some())
    }

    async fn release_rate_lease(
        &self,
        lease_id: i64,
    ) -> Result<()> {
        let pool = self.pool()?;

        sqlx::query("DELETE FROM subagent_rate_leases WHERE id = $1")
            .bind(lease_id)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("release_rate_lease: {e}")))?;

        Ok(())
    }
}

// ─── postgres minion job helpers ──────────────────────────────────────────

/// Column projection for `minion_jobs` reads. Record columns (created_at/…)
/// are cast `::text` so `try_get::<String>` decodes them as RFC-3339 strings
/// (matching the SQLite TEXT columns); scheduling columns (lock_until/…) are
/// converted to epoch-**ms** `bigint` so both backends yield `Option<i64>`.
/// JSONB columns decode directly to `serde_json::Value`.
const MINION_JOB_SELECT: &str = "id, name, queue, status, priority, data, \
    max_attempts, attempts_made, attempts_started, backoff_type, backoff_delay, \
    backoff_jitter, stalled_counter, max_stalled, lock_token, \
    (EXTRACT(EPOCH FROM lock_until) * 1000)::bigint AS lock_until, \
    (EXTRACT(EPOCH FROM delay_until) * 1000)::bigint AS delay_until, \
    parent_job_id, on_child_fail, tokens_input, tokens_output, tokens_cache_read, \
    depth, max_children, timeout_ms, \
    (EXTRACT(EPOCH FROM timeout_at) * 1000)::bigint AS timeout_at, \
    remove_on_complete, remove_on_fail, idempotency_key, quiet_hours, stagger_key, \
    result, progress, error_text, stacktrace, \
    created_at::text AS created_at, started_at::text AS started_at, \
    finished_at::text AS finished_at, updated_at::text AS updated_at";

/// Insert a `child_done` envelope into the parent's inbox — but only if the
/// parent is still non-terminal (the `WHERE EXISTS(... NOT IN terminal)`
/// guard). A no-op INSERT if the parent already finished, which is why callers
/// on the fail path must emit BEFORE flipping the parent terminal.
async fn pg_emit_child_done(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    parent_id: i64,
    child_id: i64,
    job_name: &str,
    result: serde_json::Value,
    outcome: crate::minions::types::ChildOutcome,
    error: Option<String>,
) -> Result<()> {
    let envelope =
        crate::minions::types::ChildDoneMessage::new(child_id, job_name, result, outcome, error);
    let payload = serde_json::to_value(&envelope)
        .map_err(|e| Error::engine(format!("child_done serialize: {e}")))?;
    sqlx::query(
        "INSERT INTO minion_inbox (job_id, sender, payload) \
         SELECT $1, 'minions', $2 \
         WHERE EXISTS (SELECT 1 FROM minion_jobs \
             WHERE id = $1 AND status NOT IN \
                ('completed','failed','dead','cancelled'))",
    )
    .bind(parent_id)
    .bind(payload)
    .execute(&mut **tx)
    .await
    .map_err(|e| Error::engine(format!("emit_child_done INSERT: {e}")))?;
    Ok(())
}

/// Flip a parent out of `waiting-children` back to `waiting` once none of its
/// children remain non-terminal. No-op unless the parent is waiting-children
/// and all its kids are terminal.
async fn pg_resolve_parent(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    parent_id: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE minion_jobs SET status = 'waiting', updated_at = now() \
         WHERE id = $1 AND status = 'waiting-children' \
           AND NOT EXISTS (SELECT 1 FROM minion_jobs child \
               WHERE child.parent_job_id = $1 AND child.status NOT IN \
                  ('completed','failed','dead','cancelled'))",
    )
    .bind(parent_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| Error::engine(format!("resolve_parent UPDATE: {e}")))?;
    Ok(())
}

/// Map a `takes` PgRow (21-column projection) to a [`Take`].
fn take_from_row(r: &sqlx::postgres::PgRow) -> Result<Take> {
    Ok(Take {
        id: r.try_get::<i64, _>("id")
            .map(|v| v as u64)
            .map_err(|e| Error::engine(format!("take id: {e}")))?,
        page_id: r
            .try_get::<i64, _>("page_id")
            .map(|v| v as u64)
            .map_err(|e| Error::engine(format!("take page_id: {e}")))?,
        row_num: r
            .try_get::<i32, _>("row_num")
            .map_err(|e| Error::engine(format!("take row_num: {e}")))?,
        claim: r.try_get("claim").unwrap_or_default(),
        kind: r.try_get("kind").unwrap_or_default(),
        holder: r.try_get("holder").unwrap_or_default(),
        weight: r.try_get("weight").unwrap_or(0.5),
        since_date: r.try_get("since_date").unwrap_or(None),
        until_date: r.try_get("until_date").unwrap_or(None),
        source: r.try_get("source").unwrap_or(None),
        superseded_by: r.try_get::<Option<i32>, _>("superseded_by").unwrap_or(None),
        active: r.try_get::<bool, _>("active").unwrap_or(true),
        resolved_at: {
            let dt: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>> =
                r.try_get("resolved_at").unwrap_or(None);
            dt.map(|ts| ts.to_rfc3339())
        },
        resolved_quality: r.try_get("resolved_quality").unwrap_or(None),
        resolved_outcome: r.try_get("resolved_outcome").unwrap_or(None),
        resolved_evidence: r.try_get("resolved_evidence").unwrap_or(None),
        resolved_value: r.try_get("resolved_value").unwrap_or(None),
        resolved_unit: r.try_get("resolved_unit").unwrap_or(None),
        resolved_by: r.try_get("resolved_by").unwrap_or(None),
        created_at: {
            let dt: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = r
                .try_get("created_at")
                .map_err(|e| Error::engine(format!("take created_at: {e}")))?;
            dt.to_rfc3339()
        },
        updated_at: {
            let dt: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = r
                .try_get("updated_at")
                .map_err(|e| Error::engine(format!("take updated_at: {e}")))?;
            dt.to_rfc3339()
        },
    })
}

/// Map a `minion_inbox` PgRow to an [`InboxMessage`]. Expects columns:
/// id, job_id, sender, payload, sent_at (::text), read_at (::text).
fn pg_row_to_inbox(row: &sqlx::postgres::PgRow) -> Result<crate::minions::types::InboxMessage> {
    Ok(crate::minions::types::InboxMessage {
        id: row
            .try_get("id")
            .map_err(|e| Error::engine(format!("inbox id decode: {e}")))?,
        job_id: row
            .try_get("job_id")
            .map_err(|e| Error::engine(format!("inbox job_id decode: {e}")))?,
        sender: row
            .try_get("sender")
            .map_err(|e| Error::engine(format!("inbox sender decode: {e}")))?,
        payload: row
            .try_get("payload")
            .map_err(|e| Error::engine(format!("inbox payload decode: {e}")))?,
        sent_at: row
            .try_get("sent_at")
            .map_err(|e| Error::engine(format!("inbox sent_at decode: {e}")))?,
        read_at: row
            .try_get("read_at")
            .map_err(|e| Error::engine(format!("inbox read_at decode: {e}")))?,
    })
}

/// Map a `minion_attachments` PgRow to an
/// [`Attachment`](crate::minions::types::Attachment). Column names/casts must
/// match the SELECTs in `list_attachments`/`get_attachment`: `id`/`job_id`/
/// `size_bytes` cast `::bigint`, `created_at` cast `::text`. An empty
/// `storage_uri` is normalized to `None` (mirrors TS `rowToAttachment`).
fn pg_row_to_attachment(
    row: &sqlx::postgres::PgRow,
) -> Result<crate::minions::types::Attachment> {
    let storage_uri = row
        .try_get::<Option<String>, _>("storage_uri")
        .map_err(|e| Error::engine(format!("attachment storage_uri decode: {e}")))?
        .filter(|s| !s.is_empty());
    Ok(crate::minions::types::Attachment {
        id: row
            .try_get("id")
            .map_err(|e| Error::engine(format!("attachment id decode: {e}")))?,
        job_id: row
            .try_get("job_id")
            .map_err(|e| Error::engine(format!("attachment job_id decode: {e}")))?,
        filename: row
            .try_get("filename")
            .map_err(|e| Error::engine(format!("attachment filename decode: {e}")))?,
        content_type: row
            .try_get("content_type")
            .map_err(|e| Error::engine(format!("attachment content_type decode: {e}")))?,
        storage_uri,
        size_bytes: row
            .try_get("size_bytes")
            .map_err(|e| Error::engine(format!("attachment size_bytes decode: {e}")))?,
        sha256: row
            .try_get("sha256")
            .map_err(|e| Error::engine(format!("attachment sha256 decode: {e}")))?,
        created_at: row
            .try_get("created_at")
            .map_err(|e| Error::engine(format!("attachment created_at decode: {e}")))?,
    })
}

/// Map a `minion_jobs` PgRow to a [`MinionJob`]. Column names must match the
/// aliases in [`MINION_JOB_SELECT`]. TIMESTAMPTZ record columns arrive as
/// `String` (cast `::text`), scheduling columns as `Option<i64>` epoch-ms, and
/// JSONB as `serde_json::Value`.
fn pg_row_to_job(row: &sqlx::postgres::PgRow) -> Result<crate::minions::types::MinionJob> {
    use crate::minions::types::{BackoffType, ChildFailPolicy, MinionJob, MinionJobStatus};

    macro_rules! get {
        ($name:literal, $ty:ty) => {
            row.try_get::<$ty, _>($name)
                .map_err(|e| Error::engine(format!(concat!("job decode ", $name, ": {}"), e)))?
        };
    }

    let status_str: String = get!("status", String);
    let backoff_str: String = get!("backoff_type", String);
    let on_child_fail_str: String = get!("on_child_fail", String);

    // stacktrace is JSONB (array of strings); tolerate NULL / non-array.
    let stacktrace: Vec<String> = match row.try_get::<Option<serde_json::Value>, _>("stacktrace") {
        Ok(Some(v)) => serde_json::from_value(v).unwrap_or_default(),
        _ => Vec::new(),
    };

    Ok(MinionJob {
        id: i64::from(get!("id", i32)),
        name: get!("name", String),
        queue: get!("queue", String),
        status: MinionJobStatus::parse(&status_str)
            .ok_or_else(|| Error::engine(format!("job decode status: unknown '{status_str}'")))?,
        priority: get!("priority", i32),
        data: get!("data", serde_json::Value),
        max_attempts: get!("max_attempts", i32),
        attempts_made: get!("attempts_made", i32),
        attempts_started: get!("attempts_started", i32),
        backoff_type: BackoffType::parse(&backoff_str).ok_or_else(|| {
            Error::engine(format!("job decode backoff_type: unknown '{backoff_str}'"))
        })?,
        backoff_delay: get!("backoff_delay", i32),
        backoff_jitter: get!("backoff_jitter", f64),
        stalled_counter: get!("stalled_counter", i32),
        max_stalled: get!("max_stalled", i32),
        lock_token: get!("lock_token", Option<String>),
        lock_until: get!("lock_until", Option<i64>),
        delay_until: get!("delay_until", Option<i64>),
        parent_job_id: get!("parent_job_id", Option<i32>).map(i64::from),
        on_child_fail: ChildFailPolicy::parse(&on_child_fail_str).ok_or_else(|| {
            Error::engine(format!(
                "job decode on_child_fail: unknown '{on_child_fail_str}'"
            ))
        })?,
        tokens_input: i64::from(get!("tokens_input", i32)),
        tokens_output: i64::from(get!("tokens_output", i32)),
        tokens_cache_read: i64::from(get!("tokens_cache_read", i32)),
        depth: get!("depth", i32),
        max_children: get!("max_children", Option<i32>),
        timeout_ms: get!("timeout_ms", Option<i32>).map(i64::from),
        timeout_at: get!("timeout_at", Option<i64>),
        remove_on_complete: get!("remove_on_complete", bool),
        remove_on_fail: get!("remove_on_fail", bool),
        idempotency_key: get!("idempotency_key", Option<String>),
        quiet_hours: get!("quiet_hours", Option<serde_json::Value>),
        stagger_key: get!("stagger_key", Option<String>),
        result: get!("result", Option<serde_json::Value>),
        progress: get!("progress", Option<serde_json::Value>),
        error_text: get!("error_text", Option<String>),
        stacktrace,
        created_at: get!("created_at", String),
        started_at: get!("started_at", Option<String>),
        finished_at: get!("finished_at", Option<String>),
        updated_at: get!("updated_at", String),
    })
}

// ─── postgres facts helper ────────────────────────────────────────────────

/// Map a Postgres PgRow to a FactRow. Column projection must match the
/// SELECT in `list_facts_by_entity`.
fn pg_row_to_fact(row: &sqlx::postgres::PgRow) -> Result<FactRow> {
    fn parse_kind(s: &str) -> FactKind {
        match s {
            "event" => FactKind::Event,
            "preference" => FactKind::Preference,
            "commitment" => FactKind::Commitment,
            "belief" => FactKind::Belief,
            _ => FactKind::Fact,
        }
    }
    fn parse_visibility(s: &str) -> FactVisibility {
        match s {
            "world" => FactVisibility::World,
            _ => FactVisibility::Private,
        }
    }

    Ok(FactRow {
        id: row
            .try_get("id")
            .map_err(|e| Error::engine(format!("fact id: {e}")))?,
        source_id: row
            .try_get("source_id")
            .map_err(|e| Error::engine(format!("fact source_id: {e}")))?,
        entity_slug: row
            .try_get("entity_slug")
            .map_err(|e| Error::engine(format!("fact entity_slug: {e}")))?,
        fact: row
            .try_get("fact")
            .map_err(|e| Error::engine(format!("fact fact: {e}")))?,
        kind: {
            let s: String = row
                .try_get("kind")
                .map_err(|e| Error::engine(format!("fact kind: {e}")))?;
            parse_kind(&s)
        },
        visibility: {
            let s: String = row
                .try_get("visibility")
                .map_err(|e| Error::engine(format!("fact visibility: {e}")))?;
            parse_visibility(&s)
        },
        notability: row
            .try_get("notability")
            .map_err(|e| Error::engine(format!("fact notability: {e}")))?,
        context: row
            .try_get("context")
            .map_err(|e| Error::engine(format!("fact context: {e}")))?,
        valid_from: row
            .try_get("valid_from")
            .map_err(|e| Error::engine(format!("fact valid_from: {e}")))?,
        valid_until: row
            .try_get("valid_until")
            .map_err(|e| Error::engine(format!("fact valid_until: {e}")))?,
        expired_at: row
            .try_get("expired_at")
            .map_err(|e| Error::engine(format!("fact expired_at: {e}")))?,
        superseded_by: row
            .try_get("superseded_by")
            .map_err(|e| Error::engine(format!("fact superseded_by: {e}")))?,
        consolidated_at: row
            .try_get("consolidated_at")
            .map_err(|e| Error::engine(format!("fact consolidated_at: {e}")))?,
        consolidated_into: row
            .try_get("consolidated_into")
            .map_err(|e| Error::engine(format!("fact consolidated_into: {e}")))?,
        source: row
            .try_get("source")
            .map_err(|e| Error::engine(format!("fact source: {e}")))?,
        source_session: row
            .try_get("source_session")
            .map_err(|e| Error::engine(format!("fact source_session: {e}")))?,
        confidence: row
            .try_get("confidence")
            .map_err(|e| Error::engine(format!("fact confidence: {e}")))?,
        created_at: {
            // Postgres TIMESTAMPTZ → Option<String> via try_get
            row.try_get("created_at")
                .map_err(|e| Error::engine(format!("fact created_at: {e}")))?
        },
    })
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
    // G24: `embedding` (BYTEA, f32-LE blob) is now in FULL_PAGE_PROJECTION and
    // written by put_page. NULL → None (vector path degrades to lexical-only).
    let embedding: Option<Vec<u8>> = row
        .try_get("embedding")
        .map_err(|e| Error::engine(format!("row decode embedding: {e}")))?;
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
        embedding,
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

// ── OAuthQueries PostgresEngine implementation ─────────────────────────────

/// SHA-256 hex digest of bytes — matches the libsql helper.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Current Unix timestamp in seconds (i64, compatible with BIGINT).
fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[async_trait]
impl OAuthQueries for PostgresEngine {
    async fn register_client(
        &self,
        req: RegisterClientRequest,
    ) -> Result<RegisterClientResponse> {
        let pool = self.pool()?;
        let client_id = uuid::Uuid::new_v4().to_string();
        let client_secret = uuid::Uuid::new_v4().to_string();
        let secret_hash = sha256_hex(client_secret.as_bytes());

        let grant_types_json = serde_json::to_string(&req.grant_types)
            .map_err(|e| Error::engine(format!("serialize grant_types: {e}")))?;
        let redirect_uris_json = serde_json::to_string(&req.redirect_uris)
            .map_err(|e| Error::engine(format!("serialize redirect_uris: {e}")))?;

        let federated_read = if req.federated_read.is_empty() {
            vec![req.source_id.clone()]
        } else {
            req.federated_read.clone()
        };
        let federated_read_json = serde_json::to_string(&federated_read)
            .map_err(|e| Error::engine(format!("serialize federated_read: {e}")))?;

        let now_secs = unix_now_secs();

        sqlx::query(
            "INSERT INTO oauth_clients \
             (client_id, client_secret_hash, client_name, redirect_uris, grant_types, scope, \
              token_endpoint_auth_method, token_ttl, client_id_issued_at, \
              source_id, federated_read) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&client_id)
        .bind(&secret_hash)
        .bind(&req.name)
        .bind(&redirect_uris_json)
        .bind(&grant_types_json)
        .bind(&req.scope)
        .bind(&req.token_endpoint_auth_method)
        .bind(req.token_ttl)
        .bind(now_secs)
        .bind(&req.source_id)
        .bind(&federated_read_json)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("register_client insert: {e}")))?;

        Ok(RegisterClientResponse {
            client_id,
            client_secret,
        })
    }

    async fn update_client_ttl(
        &self,
        client_id: &str,
        ttl: Option<i64>,
    ) -> Result<UpdateClientTtlResponse> {
        let pool = self.pool()?;
        let db_ttl = ttl.filter(|&v| v > 0);

        sqlx::query("UPDATE oauth_clients SET token_ttl = $1 WHERE client_id = $2")
            .bind(db_ttl)
            .bind(client_id)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("update_client_ttl: {e}")))?;

        Ok(UpdateClientTtlResponse {
            updated: true,
            token_ttl: db_ttl,
        })
    }

    async fn revoke_client(&self, client_id: &str) -> Result<RevokeClientResponse> {
        let pool = self.pool()?;

        // Soft-delete the client (only if not already deleted)
        sqlx::query(
            "UPDATE oauth_clients SET deleted_at = NOW() \
             WHERE client_id = $1 AND deleted_at IS NULL",
        )
        .bind(client_id)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("revoke_client soft-delete: {e}")))?;

        // Revoke all active tokens for this client
        sqlx::query("DELETE FROM oauth_tokens WHERE client_id = $1")
            .bind(client_id)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("revoke_client delete tokens: {e}")))?;

        Ok(RevokeClientResponse { revoked: true })
    }

    async fn get_client(&self, client_id: &str) -> Result<Option<OAuthClientInfo>> {
        let pool = self.pool()?;

        let row = sqlx::query(
            "SELECT client_id, client_secret_hash, client_name, redirect_uris, \
             grant_types, scope, token_endpoint_auth_method, \
             client_id_issued_at, client_secret_expires_at, token_ttl \
             FROM oauth_clients WHERE client_id = $1",
        )
        .bind(client_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("get_client query: {e}")))?;

        let row = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        let redirect_uris_json: String = row.try_get("redirect_uris").unwrap_or_default();
        let grant_types_json: String = row.try_get("grant_types").unwrap_or_default();
        let redirect_uris: Vec<String> =
            serde_json::from_str(&redirect_uris_json).unwrap_or_default();
        let grant_types: Vec<String> = serde_json::from_str(&grant_types_json)
            .unwrap_or_else(|_| vec!["client_credentials".to_string()]);

        Ok(Some(OAuthClientInfo {
            client_id: row.try_get("client_id").unwrap_or_default(),
            client_secret_hash: row.try_get("client_secret_hash").ok(),
            client_name: row.try_get("client_name").unwrap_or_default(),
            redirect_uris,
            grant_types,
            scope: row.try_get("scope").ok(),
            token_endpoint_auth_method: row.try_get("token_endpoint_auth_method").ok(),
            client_id_issued_at: row.try_get("client_id_issued_at").ok(),
            client_secret_expires_at: row.try_get("client_secret_expires_at").ok(),
            token_ttl: row.try_get("token_ttl").ok(),
        }))
    }

    async fn exchange_client_credentials(
        &self,
        client_id: &str,
        client_secret: &str,
        requested_scope: Option<&str>,
    ) -> Result<ExchangeTokens> {
        let client = self
            .get_client(client_id)
            .await?
            .ok_or_else(|| Error::engine("Client not found"))?;

        // Check revoked (soft-deleted).
        {
            let pool = self.pool()?;
            let row = sqlx::query(
                "SELECT 1 FROM oauth_clients WHERE client_id = $1 AND deleted_at IS NOT NULL",
            )
            .bind(client_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::engine(format!("check revoked: {e}")))?;
            if row.is_some() {
                return Err(Error::engine("Client has been revoked"));
            }
        }

        // Check grant type.
        if !client.grant_types.iter().any(|g| g == "client_credentials") {
            return Err(Error::engine(
                "Client credentials grant not authorized for this client",
            ));
        }

        // Verify secret.
        let presented_hash = sha256_hex(client_secret.as_bytes());
        let stored_hash = client.client_secret_hash.as_deref().unwrap_or("");
        if presented_hash != stored_hash {
            return Err(Error::engine("Invalid client secret"));
        }

        // Determine scopes.
        let allowed_scopes = parse_scope_string(client.scope.as_deref().unwrap_or("read"));
        let requested_scopes = match requested_scope {
            Some(s) if !s.is_empty() => parse_scope_string(s),
            _ => allowed_scopes.clone(),
        };
        let granted_scopes: Vec<&str> = requested_scopes
            .iter()
            .filter(|s| has_scope(&allowed_scopes, s.as_ref()))
            .map(|s| s.as_str())
            .collect();

        let ttl_override = client.token_ttl.filter(|&t| t > 0);

        let tokens = self
            .issue_oauth_tokens(client_id, &granted_scopes, false, ttl_override)
            .await?;
        Ok(tokens)
    }

    async fn verify_confidential_client_secret(
        &self,
        client_id: &str,
        presented_secret: &str,
    ) -> Result<OAuthClientInfo> {
        let client = self
            .get_client(client_id)
            .await?
            .ok_or_else(|| Error::engine("Invalid client"))?;

        if client.client_secret_hash.is_none() {
            return Err(Error::engine("Invalid client"));
        }

        let presented_hash = sha256_hex(presented_secret.as_bytes());
        let stored_hash = client.client_secret_hash.as_deref().unwrap_or("");
        if presented_hash != stored_hash {
            return Err(Error::engine("Invalid client"));
        }

        // Soft-delete probe.
        {
            let pool = self.pool()?;
            let row = sqlx::query(
                "SELECT 1 FROM oauth_clients WHERE client_id = $1 AND deleted_at IS NOT NULL",
            )
            .bind(client_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| Error::engine(format!("check revoked: {e}")))?;
            if row.is_some() {
                return Err(Error::engine("Client has been revoked"));
            }
        }

        Ok(client)
    }

    async fn exchange_authorization_code(
        &self,
        client_id: &str,
        authorization_code: &str,
        redirect_uri: Option<&str>,
    ) -> Result<ExchangeTokens> {
        let code_hash = sha256_hex(authorization_code.as_bytes());
        let now = unix_now_secs();
        let pool = self.pool()?;

        let row = if let Some(redirect) = redirect_uri {
            sqlx::query(
                "DELETE FROM oauth_codes \
                 WHERE code_hash = $1 AND client_id = $2 AND redirect_uri = $3 AND expires_at > $4 \
                 RETURNING client_id, scopes, resource",
            )
            .bind(&code_hash)
            .bind(client_id)
            .bind(redirect)
            .bind(now)
            .fetch_optional(pool)
            .await
        } else {
            sqlx::query(
                "DELETE FROM oauth_codes \
                 WHERE code_hash = $1 AND client_id = $2 AND expires_at > $3 \
                 RETURNING client_id, scopes, resource",
            )
            .bind(&code_hash)
            .bind(client_id)
            .bind(now)
            .fetch_optional(pool)
            .await
        }
        .map_err(|e| Error::engine(format!("exchange_authorization_code delete: {e}")))?;

        let row = row.ok_or_else(|| Error::engine("Authorization code not found or expired"))?;

        let scopes_json: String = row.try_get("scopes").unwrap_or_default();
        let scopes: Vec<String> = serde_json::from_str(&scopes_json).unwrap_or_default();
        let granted: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

        self.issue_oauth_tokens(client_id, &granted, true, None).await
    }

    async fn exchange_refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        requested_scopes: Option<&[String]>,
    ) -> Result<ExchangeTokens> {
        let token_hash = sha256_hex(refresh_token.as_bytes());
        let now = unix_now_secs();
        let pool = self.pool()?;

        let row = sqlx::query(
            "DELETE FROM oauth_tokens \
             WHERE token_hash = $1 AND token_type = 'refresh' AND client_id = $2 \
             RETURNING client_id, scopes, expires_at",
        )
        .bind(&token_hash)
        .bind(client_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| Error::engine(format!("exchange_refresh_token delete: {e}")))?;

        let row = row.ok_or_else(|| Error::engine("Refresh token not found"))?;

        let expires_at: i64 = row.try_get("expires_at").unwrap_or(0);
        if expires_at < now {
            return Err(Error::engine("Refresh token expired"));
        }

        let scopes_json: String = row.try_get("scopes").unwrap_or_default();
        let granted_scopes: Vec<String> = serde_json::from_str(&scopes_json).unwrap_or_default();

        let token_scopes: Vec<String> = match requested_scopes {
            Some(req) if !req.is_empty() => {
                for s in req {
                    if !has_scope(
                        &granted_scopes
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>(),
                        s.as_str(),
                    ) {
                        return Err(Error::engine(
                            "Requested scope exceeds refresh token grant",
                        ));
                    }
                }
                req.to_vec()
            }
            _ => granted_scopes.clone(),
        };

        let scope_refs: Vec<&str> = token_scopes.iter().map(|s| s.as_str()).collect();
        self.issue_oauth_tokens(client_id, &scope_refs, true, None).await
    }

    async fn sweep_expired_tokens(&self) -> Result<u64> {
        let pool = self.pool()?;
        let now = unix_now_secs();

        let tokens_deleted = sqlx::query("DELETE FROM oauth_tokens WHERE expires_at < $1")
            .bind(now)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("sweep_expired_tokens oauth_tokens: {e}")))?;

        let codes_deleted = sqlx::query("DELETE FROM oauth_codes WHERE expires_at < $1")
            .bind(now)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("sweep_expired_tokens oauth_codes: {e}")))?;

        Ok(tokens_deleted.rows_affected() + codes_deleted.rows_affected())
    }
}

// ── CalibrationQueries PostgresEngine implementation ──────────────────────

/// Postgres catalogue errors we treat as "schema not migrated yet" and degrade
/// to an empty/None result instead of failing the query — mirrors Libsql's
/// `no such table` / `no such column` handling. Postgres uses
/// `relation "x" does not exist` (42P01) and `column "x" does not exist`
/// (42703); both contain the substring `does not exist`.
fn pg_is_missing_schema(err: &sqlx::Error) -> bool {
    err.to_string().contains("does not exist")
}

#[async_trait]
impl CalibrationQueries for PostgresEngine {
    /// Aggregated scoring stats from resolved takes.
    ///
    /// Pulls the minimal scoped rows (`kind`/`weight`/`resolved_quality`) then
    /// delegates the canonical math to `aggregate_scorecard`, so InMemory,
    /// Libsql, and Postgres are bit-identical. Scoping mirrors canonical TS
    /// `getScorecard`: holder + optional slug-prefix domain via
    /// `EXISTS(pages.slug LIKE prefix%)` + `since_date` window + allow-list.
    async fn get_scorecard(&self, query: &ScorecardQuery<'_>) -> Result<TakesScorecard> {
        let pool = self.pool()?;

        // Build the scoped SELECT with positional $N placeholders. Every clause
        // is optional and appends its own placeholder so the bind order below
        // stays aligned (canonical `WHERE 1=1` + conditional clauses).
        let mut sql = String::from(
            "SELECT t.kind, t.weight, t.resolved_quality FROM takes t WHERE 1=1",
        );
        let mut n = 0;
        let has_holder = query.holder.is_some();
        if has_holder {
            n += 1;
            sql.push_str(&format!(" AND t.holder = ${n}"));
        }
        let domain_like = query.domain_prefix.map(|p| format!("{p}%"));
        if domain_like.is_some() {
            n += 1;
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM pages p WHERE p.id = t.page_id AND p.slug LIKE ${n})",
            ));
        }
        if query.since.is_some() {
            n += 1;
            sql.push_str(&format!(" AND t.since_date >= ${n}"));
        }
        if query.until.is_some() {
            n += 1;
            sql.push_str(&format!(" AND t.since_date <= ${n}"));
        }
        // Allow-list membership (`AND holder = ANY($list)`, D4 defense-in-depth).
        let has_allow_list = query.holders_allow_list.is_some();
        if has_allow_list {
            n += 1;
            sql.push_str(&format!(" AND t.holder = ANY(${n}::text[])"));
        }

        let mut q = sqlx::query_as::<_, (String, f64, Option<String>)>(&sql);
        if let Some(holder) = query.holder {
            q = q.bind(holder);
        }
        if let Some(ref like) = domain_like {
            q = q.bind(like);
        }
        if let Some(since) = query.since {
            q = q.bind(since);
        }
        if let Some(until) = query.until {
            q = q.bind(until);
        }
        if let Some(list) = query.holders_allow_list {
            q = q.bind(list.to_vec());
        }

        match q.fetch_all(pool).await {
            Err(e) if pg_is_missing_schema(&e) => Ok(aggregate_scorecard(std::iter::empty())),
            Err(e) => Err(Error::engine(format!("get_scorecard: {e}"))),
            Ok(rows) => {
                let scored = rows.into_iter().map(|(kind, weight, resolved_quality)| ScorecardRow {
                    kind,
                    weight,
                    resolved_quality,
                });
                Ok(aggregate_scorecard(scored))
            }
        }
    }

    /// Confidence-bucket accuracy curve (observed vs predicted per weight bucket).
    ///
    /// Pulls the scoped `(weight, resolved_quality)` rows (only
    /// `resolved_quality IN ('correct','incorrect')`), then delegates the
    /// canonical binning to `aggregate_calibration_curve`, so InMemory, Libsql,
    /// and Postgres are bit-identical. Scoping mirrors canonical TS
    /// `getCalibrationCurve`: optional holder + server-side allow-list.
    async fn get_calibration_curve(&self, query: &CalibrationCurveQuery<'_>) -> Result<Vec<CalibrationBucket>> {
        let pool = self.pool()?;

        let mut sql = String::from(
            "SELECT t.weight, t.resolved_quality FROM takes t WHERE 1=1",
        );
        let mut n = 0;
        if let Some(holder) = query.holder {
            n += 1;
            sql.push_str(&format!(" AND t.holder = ${n}"));
        }
        // Allow-list membership (`AND holder = ANY($list)`, D4 defense-in-depth).
        let has_allow_list = query.holders_allow_list.is_some();
        if has_allow_list {
            n += 1;
            sql.push_str(&format!(" AND t.holder = ANY(${n}::text[])"));
        }
        sql.push_str(" AND t.resolved_quality IN ('correct','incorrect')");

        let mut q = sqlx::query_as::<_, (f64, Option<String>)>(&sql);
        if let Some(holder) = query.holder {
            q = q.bind(holder);
        }
        if let Some(list) = query.holders_allow_list {
            q = q.bind(list.to_vec());
        }

        match q.fetch_all(pool).await {
            Err(e) if pg_is_missing_schema(&e) => Ok(Vec::new()),
            Err(e) => Err(Error::engine(format!("get_calibration_curve: {e}"))),
            Ok(rows) => {
                let scored = rows
                    .into_iter()
                    .map(|(weight, resolved_quality)| CalibrationRow { weight, resolved_quality });
                Ok(aggregate_calibration_curve(
                    scored,
                    query.bucket_size.unwrap_or(0.1),
                ))
            }
        }
    }

    /// Latest calibration profile for a holder.
    ///
    /// The `calibration_profiles` table does not exist in the Postgres schema
    /// yet, so this degrades to `None`.
    async fn get_latest_profile(&self, holder: &str, source_id: Option<&str>, source_ids: Option<&[String]>) -> Result<Option<CalibrationProfileRow>> {
        let pool = self.pool()?;
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, source_id, holder, wave_version, generated_at, published, \
                    total_resolved, brier, accuracy, partial_rate, grade_completion, \
                    domain_scorecards, pattern_statements, voice_gate_passed, \
                    voice_gate_attempts, active_bias_tags, model_id, cost_usd, \
                    judge_model_agreement \
             FROM calibration_profiles \
             WHERE holder = $1 "
        );

        let mut bind_count = 2;
        if let Some(s) = source_id {
            builder.push(" AND source_id = ");
            builder.push_bind(s);
        }
        if let Some(sids) = source_ids {
            if !sids.is_empty() {
                builder.push(" AND source_id IN (");
                let mut first = true;
                for sid in sids {
                    if !first {
                        builder.push(',');
                    }
                    first = false;
                    builder.push_bind(sid);
                }
                builder.push(')');
            }
        }

        builder.push(" ORDER BY generated_at DESC LIMIT 1");

        let result = builder.build_query_as::<CalibrationProfileRowDb>()
        .fetch_optional(pool)
        .await;

        match result {
            Err(e) if pg_is_missing_schema(&e) => Ok(None),
            Err(e) => Err(Error::engine(format!("get_latest_profile: {e}"))),
            Ok(None) => Ok(None),
            Ok(Some(row)) => Ok(Some(CalibrationProfileRow {
                id: row.id,
                source_id: row.source_id,
                holder: row.holder,
                wave_version: row.wave_version,
                generated_at: row.generated_at,
                published: row.published,
                total_resolved: row.total_resolved,
                brier: row.brier,
                accuracy: row.accuracy,
                partial_rate: row.partial_rate,
                grade_completion: row.grade_completion,
                domain_scorecards: row.domain_scorecards,
                pattern_statements: row.pattern_statements,
                voice_gate_passed: row.voice_gate_passed,
                voice_gate_attempts: row.voice_gate_attempts as i16,
                active_bias_tags: row.active_bias_tags,
                model_id: row.model_id,
                cost_usd: row.cost_usd,
                judge_model_agreement: row.judge_model_agreement,
            })),
        }
    }

    /// Pattern text + top-25 resolved takes for drill-down.
    ///
    /// Depends on `calibration_profiles` (absent in Postgres), so this degrades
    /// to `None`.
    async fn get_pattern_detail(
        &self,
        holder: &str,
        _pattern_index: usize,
    ) -> Result<Option<PatternDetail>> {
        let pool = self.pool()?;
        let result = sqlx::query_as::<_, (Option<serde_json::Value>,)>(
            "SELECT pattern_statements FROM calibration_profiles \
             WHERE holder = $1 ORDER BY generated_at DESC LIMIT 1",
        )
        .bind(holder)
        .fetch_optional(pool)
        .await;

        match result {
            Err(e) if pg_is_missing_schema(&e) => Ok(None),
            Err(e) => Err(Error::engine(format!("get_pattern_detail: {e}"))),
            // Table exists but no profile — treat as no detail available.
            Ok(_) => Ok(None),
        }
    }
}

// ── Internal: OAuth token issuance helper ─────────────────────────────────

impl PostgresEngine {
    async fn issue_oauth_tokens(
        &self,
        client_id: &str,
        scopes: &[&str],
        include_refresh: bool,
        ttl_override: Option<i64>,
    ) -> Result<ExchangeTokens> {
        let pool = self.pool()?;
        let access_token = format!("zbrain_at_{}", uuid::Uuid::new_v4().simple());
        let access_hash = sha256_hex(access_token.as_bytes());
        let now = unix_now_secs();
        let effective_ttl = ttl_override.unwrap_or(3600);
        let access_expiry = now + effective_ttl;
        let scopes_json = serde_json::to_string(&scopes)
            .map_err(|e| Error::engine(format!("serialize scopes: {e}")))?;
        let scope_string = scopes.join(" ");

        sqlx::query(
            "INSERT INTO oauth_tokens (token_hash, token_type, client_id, scopes, expires_at) \
             VALUES ($1, 'access', $2, $3, $4)",
        )
        .bind(&access_hash)
        .bind(client_id)
        .bind(&scopes_json)
        .bind(access_expiry)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("issue_oauth_tokens insert access: {e}")))?;

        let mut refresh_token: Option<String> = None;

        if include_refresh {
            let rt = format!("zbrain_rt_{}", uuid::Uuid::new_v4().simple());
            let rt_hash = sha256_hex(rt.as_bytes());
            let refresh_expiry = now + 30 * 24 * 3600;

            sqlx::query(
                "INSERT INTO oauth_tokens (token_hash, token_type, client_id, scopes, expires_at) \
                 VALUES ($1, 'refresh', $2, $3, $4)",
            )
            .bind(&rt_hash)
            .bind(client_id)
            .bind(&scopes_json)
            .bind(refresh_expiry)
            .execute(pool)
            .await
            .map_err(|e| Error::engine(format!("issue_oauth_tokens insert refresh: {e}")))?;

            refresh_token = Some(rt);
        }

        Ok(ExchangeTokens {
            access_token,
            token_type: "bearer".to_string(),
            expires_in: effective_ttl,
            scope: scope_string,
            refresh_token,
        })
    }

    // ─── Internal budget audit logging (1-3-2) ─────────────────────────────

    /// Write a row to `minion_budget_log`. This is a best-effort audit trail
    /// — failures are returned to the caller who may `tracing::warn!` them.
    /// `cents_delta` is positive for charges, negative for refunds.
    async fn log_budget_event(
        &self,
        job_id: i64,
        cents_delta: i64,
        reason: &str,
    ) -> Result<()> {
        let pool = self.pool()?;
        sqlx::query(
            "INSERT INTO minion_budget_log (job_id, cents_delta, reason) VALUES ($1, $2, $3)",
        )
        .bind(job_id)
        .bind(cents_delta)
        .bind(reason)
        .execute(pool)
        .await
        .map_err(|e| Error::engine(format!("log_budget_event: {e}")))?;
        Ok(())
    }

    async fn execute_raw(
        &self,
        sql: &str,
        params: &[&(dyn erased_serde::Serialize + Sync)],
    ) -> crate::Result<Vec<serde_json::Value>> {
        use sqlx::{Column, Row};

        // For each parameter, serialize it to JSON and then parse as sqlx::Value
        let pool = self.pool.get().expect("pool not initialized");
        let mut query = sqlx::query(sql);
        for p in params {
            let json = serde_json::to_value(p)
                .map_err(|e| crate::Error::engine(format!("serialize parameter: {e}")))?;
            query = match json {
                serde_json::Value::Null => query.bind(None as Option<&str>),
                serde_json::Value::Bool(b) => query.bind(b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        query.bind(i)
                    } else if let Some(f) = n.as_f64() {
                        query.bind(f)
                    } else {
                        query.bind(n.to_string())
                    }
                }
                serde_json::Value::String(s) => query.bind(s),
                // For complex types, just bind as JSONB
                _ => query.bind(serde_json::to_string(&json).unwrap()),
            };
        }

        let rows = query.fetch_all(pool)
            .await
            .map_err(|e| crate::Error::engine(format!("execute_raw query: {e}")))?;

        let mut result = Vec::new();
        for row in rows {
            let mut map = serde_json::Map::new();
            for col in row.columns() {
                let name = col.name();
                let json_val = pg_cell_to_json(&row, name);
                map.insert(name.to_string(), json_val);
            }
            result.push(serde_json::Value::Object(map));
        }

        Ok(result)
    }
}

// ── TokenQueries PostgresEngine implementation ────────────────────────────

#[async_trait]
impl TokenQueries for PostgresEngine {
    async fn verify_access_token(
        &self,
        token: &str,
    ) -> std::result::Result<AuthInfo, TokenError> {
        let token_hash = sha256_hex(token.as_bytes());

        let pool = self.pool().map_err(|e| TokenError::Storage(e.to_string()))?;

        let now_secs = unix_now_secs();

        let row = sqlx::query(
            "SELECT t.client_id, t.scopes, t.expires_at, \
                    c.client_name, c.source_id, t.resource, c.federated_read \
             FROM oauth_tokens t \
             LEFT JOIN oauth_clients c ON c.client_id = t.client_id \
             WHERE t.token_hash = $1 AND t.token_type = 'access'",
        )
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .map_err(|e| TokenError::Storage(e.to_string()))?;

        if let Some(row) = row {
            let expires_at: i64 = row.try_get("expires_at").unwrap_or(0);
            if expires_at == 0 || expires_at < now_secs {
                return Err(TokenError::Expired);
            }

            let scopes_raw: String = row.try_get("scopes").unwrap_or_default();
            let scopes: Vec<String> = serde_json::from_str(&scopes_raw).unwrap_or_default();

            let client_id: String = row.try_get("client_id").unwrap_or_default();
            let client_name: Option<String> = row.try_get("client_name").ok();
            let source_id: Option<String> = row.try_get("source_id").ok();
            let resource: Option<String> = row.try_get("resource").ok();
            let federated_read_raw: Option<String> = row.try_get("federated_read").ok();
            let allowed_sources: Option<Vec<String>> = federated_read_raw
                .and_then(|s| serde_json::from_str(&s).ok());

            return Ok(AuthInfo {
                token: token.to_string(),
                client_id,
                client_name,
                scopes,
                expires_at,
                source_id,
                resource,
                allowed_sources,
            });
        }

        // Fallback: legacy access_tokens table.
        let legacy = sqlx::query(
            "SELECT name FROM access_tokens \
             WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(&token_hash)
        .fetch_optional(pool)
        .await;

        match legacy {
            Ok(Some(row)) => {
                let name: String = row.try_get("name").unwrap_or_default();
                // Update last_used_at (best-effort).
                let _ = sqlx::query(
                    "UPDATE access_tokens SET last_used_at = NOW() WHERE token_hash = $1",
                )
                .bind(&token_hash)
                .execute(pool)
                .await;

                Ok(AuthInfo {
                    token: token.to_string(),
                    client_id: name.clone(),
                    client_name: Some(name),
                    scopes: vec!["read".into(), "write".into(), "admin".into()],
                    expires_at: now_secs + 365 * 24 * 3600,
                    source_id: Some("default".into()),
                    resource: None,
                    allowed_sources: Some(vec!["default".into()]),
                })
            }
            Ok(None) => Err(TokenError::Invalid),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") || msg.contains("does not exist") {
                    Err(TokenError::Invalid)
                } else {
                    Err(TokenError::Storage(msg))
                }
            }
        }
    }
}

/// UNION `code_edges_chunk` + `code_edges_symbol` on a symbol column.
/// Mirrors the libsql `code_edge_symbol_query` (Rust replacement of TS
/// `getCallersOf` / `getCalleesOf`). Free function (not a trait method) so it
/// can be shared by both `get_callers_of` / `get_callees_of`.
async fn code_edge_symbol_query_pg(
    engine: &PostgresEngine,
    symbol_col: &str,
    qualified_name: &str,
    opts: &crate::import::CodeGraphQueryOpts,
) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
    let pool = engine.pool()?;
    let limit = (opts.limit.unwrap_or(100) as i64).min(500);
    let has_source = !opts.all_sources && opts.source_id.is_some();
    let source_clause = if has_source { " AND source_id = $2" } else { "" };
    let limit_ph = if has_source { 3 } else { 2 };
    let sql = format!(
        "SELECT id, from_chunk_id, to_chunk_id, from_symbol_qualified, to_symbol_qualified, \
                edge_type, edge_metadata, source_id, true AS resolved \
           FROM code_edges_chunk WHERE {sym} = $1{sc} \
         UNION ALL \
         SELECT id, from_chunk_id, NULL AS to_chunk_id, from_symbol_qualified, to_symbol_qualified, \
                edge_type, edge_metadata, source_id, false AS resolved \
           FROM code_edges_symbol WHERE {sym} = $1{sc} \
         LIMIT ${lim}",
        sym = symbol_col,
        sc = source_clause,
        lim = limit_ph,
    );
    let mut q = sqlx::query(&sql).bind(qualified_name.to_string());
    if has_source {
        q = q.bind(opts.source_id.clone().unwrap());
    }
    q = q.bind(limit);
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| crate::Error::engine(format!("get_callers/callees ({symbol_col}) query failed: {e}")))?;
    rows.iter().map(code_edge_row_to_result_pg).collect()
}

/// Map a Postgres code-edge result row to the public `CodeEdgeResult` contract.
/// Column aliases must match the SELECTs in `code_edge_symbol_query_pg` /
/// `get_edges_by_chunk` (the `resolved`/`to_chunk_id` AS aliases come from SQL).
fn code_edge_row_to_result_pg(
    row: &sqlx::postgres::PgRow,
) -> crate::Result<crate::import::CodeEdgeResult> {
    // PG stores these as INT4 (from SERIAL / INTEGER); widen to i64 to match
    // the `CodeEdgeResult` contract and the libsql / InMemory backends.
    let id: i32 = row
        .try_get("id")
        .map_err(|e| crate::Error::engine(format!("code_edge decode id: {e}")))?;
    let from_chunk_id: i32 = row
        .try_get("from_chunk_id")
        .map_err(|e| crate::Error::engine(format!("code_edge decode from_chunk_id: {e}")))?;
    let to_chunk_id: Option<i32> = row
        .try_get("to_chunk_id")
        .map_err(|e| crate::Error::engine(format!("code_edge decode to_chunk_id: {e}")))?;
    let from_symbol_qualified: String = row
        .try_get("from_symbol_qualified")
        .map_err(|e| crate::Error::engine(format!("code_edge decode from_symbol_qualified: {e}")))?;
    let to_symbol_qualified: String = row
        .try_get("to_symbol_qualified")
        .map_err(|e| crate::Error::engine(format!("code_edge decode to_symbol_qualified: {e}")))?;
    let edge_type: String = row
        .try_get("edge_type")
        .map_err(|e| crate::Error::engine(format!("code_edge decode edge_type: {e}")))?;
    let edge_metadata: serde_json::Value = row
        .try_get("edge_metadata")
        .unwrap_or_else(|_| serde_json::json!({}));
    let source_id: Option<String> = row
        .try_get("source_id")
        .map_err(|e| crate::Error::engine(format!("code_edge decode source_id: {e}")))?;
    let resolved: bool = row
        .try_get("resolved")
        .map_err(|e| crate::Error::engine(format!("code_edge decode resolved: {e}")))?;
    Ok(crate::import::CodeEdgeResult {
        id: id as i64,
        from_chunk_id: from_chunk_id as i64,
        to_chunk_id: to_chunk_id.map(|v| v as i64),
        from_symbol_qualified,
        to_symbol_qualified,
        edge_type,
        edge_metadata,
        source_id,
        resolved,
    })
}

// ─── 1-6-7-10-3: code-graph symbol queries (Postgres) ──────────────────────

/// Definition-site symbol types (mirrors `CODE_DEF_TYPES` in libsql.rs and the
/// TS `DEF_TYPES`); interpolated into the `IN (...)` clause (trusted literal).
const CODE_DEF_TYPES_PG: &[&str] = &[
    "function", "class", "interface", "type", "enum", "struct", "trait", "module", "contract",
    "table", "view", "index", "procedure", "schema", "database", "trigger", "export statement",
];

/// Postgres mirror of `code_def_query` (libsql). `frontmatter->>'file'` extracts
/// the JSONB file key; `symbol_type IN (...)` restricts to real definitions.
async fn code_def_query_pg(
    pool: &sqlx::PgPool,
    symbol: &str,
    opts: &crate::import::CodeSymbolQueryOpts,
) -> Result<Vec<crate::import::CodeDefResult>> {
    let limit = (opts.limit.unwrap_or(20) as i64).min(500);
    let has_lang = opts.language.is_some();
    let lang_clause = if has_lang { " AND cc.language = $2" } else { "" };
    let limit_ph = if has_lang { 3 } else { 2 };
    let types_list = CODE_DEF_TYPES_PG
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT p.slug, p.frontmatter->>'file' AS file, cc.language, \
                cc.symbol_type, cc.start_line, cc.end_line, cc.chunk_text \
           FROM content_chunks cc \
           JOIN pages p ON p.id = cc.page_id \
          WHERE cc.symbol_name = $1 \
            {lang} \
            AND p.page_kind = 'code' \
            AND cc.symbol_type IN ({types}) \
          ORDER BY CASE cc.symbol_type \
                     WHEN 'function' THEN 1 WHEN 'class' THEN 2 WHEN 'interface' THEN 3 \
                     WHEN 'type' THEN 4 WHEN 'enum' THEN 5 WHEN 'struct' THEN 6 \
                     ELSE 7 END, \
                   p.slug, cc.start_line \
          LIMIT ${lim}",
        lang = lang_clause,
        types = types_list,
        lim = limit_ph,
    );
    let mut q = sqlx::query(&sql).bind(symbol.to_string());
    if has_lang {
        q = q.bind(opts.language.clone().unwrap());
    }
    q = q.bind(limit);
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| crate::Error::engine(format!("find_code_def (pg) query failed: {e}")))?;
    rows.iter().map(code_def_row_to_result_pg).collect()
}

/// Postgres mirror of `code_ref_query` (libsql). Native `ILIKE` for the
/// case-insensitive substring scan; every matching chunk is returned.
async fn code_ref_query_pg(
    pool: &sqlx::PgPool,
    symbol: &str,
    opts: &crate::import::CodeSymbolQueryOpts,
) -> Result<Vec<crate::import::CodeRefResult>> {
    let limit = (opts.limit.unwrap_or(50) as i64).min(500);
    let has_lang = opts.language.is_some();
    let lang_clause = if has_lang { " AND cc.language = $2" } else { "" };
    let limit_ph = if has_lang { 3 } else { 2 };
    let sql = format!(
        "SELECT p.slug, p.frontmatter->>'file' AS file, cc.language, \
                cc.symbol_name, cc.symbol_type, cc.start_line, cc.end_line, cc.chunk_text \
           FROM content_chunks cc \
           JOIN pages p ON p.id = cc.page_id \
          WHERE p.page_kind = 'code' \
            AND cc.chunk_text ILIKE $1 \
            {lang} \
          ORDER BY p.slug, cc.start_line \
          LIMIT ${lim}",
        lang = lang_clause,
        lim = limit_ph,
    );
    let mut q = sqlx::query(&sql).bind(format!("%{symbol}%"));
    if has_lang {
        q = q.bind(opts.language.clone().unwrap());
    }
    q = q.bind(limit);
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| crate::Error::engine(format!("find_code_refs (pg) query failed: {e}")))?;
    rows.iter().map(code_ref_row_to_result_pg).collect()
}

/// 1-6-7-10-4 符号消歧，Postgres 镜像（对齐 TS `disambiguateSymbol`）。
///
/// 阶段一（精确）：`symbol_name = $2 OR symbol_name_qualified = $2` 取
/// `DISTINCT symbol_name_qualified`（LIMIT 25）。阶段二（近似）：仅当无精确命中，
/// 按 `symbol_name_qualified ILIKE $2`（`$2 = '%bare%'`）取 `did_you_mean` 候选
/// （LIMIT 5）。两阶段均限定 `p.source_id = $1` 且 `symbol_name_qualified IS NOT NULL`。
async fn code_disambiguate_query_pg(
    pool: &sqlx::PgPool,
    bare: &str,
    source_id: &str,
) -> Result<crate::import::SymbolDisambiguation> {
    let exact_sql = "SELECT DISTINCT cc.symbol_name_qualified \
                       FROM content_chunks cc \
                       JOIN pages p ON p.id = cc.page_id \
                      WHERE p.source_id = $1 \
                        AND cc.symbol_name_qualified IS NOT NULL \
                        AND (cc.symbol_name = $2 OR cc.symbol_name_qualified = $2) \
                      LIMIT 25";
    let rows = sqlx::query(exact_sql)
        .bind(source_id.to_string())
        .bind(bare.to_string())
        .fetch_all(pool)
        .await
        .map_err(|e| crate::Error::engine(format!("disambiguate (pg exact) query failed: {e}")))?;
    let mut matches: Vec<String> = Vec::new();
    for row in rows.iter() {
        let q: Option<String> = row
            .try_get("symbol_name_qualified")
            .map_err(|e| crate::Error::engine(format!("disambiguate (pg exact) decode: {e}")))?;
        if let Some(q) = q {
            matches.push(q);
        }
    }
    if !matches.is_empty() {
        return Ok(crate::import::SymbolDisambiguation {
            matches,
            suggestions: Vec::new(),
        });
    }

    let like = format!("%{bare}%");
    let fuzzy_sql = "SELECT DISTINCT cc.symbol_name_qualified \
                       FROM content_chunks cc \
                       JOIN pages p ON p.id = cc.page_id \
                      WHERE p.source_id = $1 \
                        AND cc.symbol_name_qualified IS NOT NULL \
                        AND cc.symbol_name_qualified ILIKE $2 \
                      LIMIT 5";
    let rows = sqlx::query(fuzzy_sql)
        .bind(source_id.to_string())
        .bind(like)
        .fetch_all(pool)
        .await
        .map_err(|e| crate::Error::engine(format!("disambiguate (pg fuzzy) query failed: {e}")))?;
    let mut suggestions: Vec<String> = Vec::new();
    for row in rows.iter() {
        let q: Option<String> = row
            .try_get("symbol_name_qualified")
            .map_err(|e| crate::Error::engine(format!("disambiguate (pg fuzzy) decode: {e}")))?;
        if let Some(q) = q {
            suggestions.push(q);
        }
    }
    Ok(crate::import::SymbolDisambiguation {
        matches: Vec::new(),
        suggestions,
    })
}

/// 1-6-7-10-5 recursive walk for Postgres.
/// Mirrors the libsql implementation exactly, using existing code-edge query helper.
async fn code_recursive_walk_query_pg(
    engine: &PostgresEngine,
    pool: &sqlx::PgPool,
    symbol: &str,
    opts: &crate::import::RecursiveWalkOpts,
) -> Result<crate::import::RecursiveWalkResult> {
    use crate::import::{
        AmbiguousCandidate, DepthGroup, DidYouMeanCandidate, RecursiveWalkNode,
        RecursiveWalkResult, WalkFreshness, WalkTruncation,
    };

    const SUPPORTED_LANGS: &[&str] = &["typescript", "tsx", "javascript", "python"];

    let depth_cap = opts.depth_cap.unwrap_or(match opts.direction {
        crate::import::WalkDirection::Callers => 5,
        crate::import::WalkDirection::Callees => 8,
    });
    let max_nodes = opts.max_nodes.unwrap_or(200);
    let source_id = opts.source_id.as_str();
    let exact = opts.exact.unwrap_or(false);

    // Step 1: disambiguate starting symbol unless exact
    let qualified_start: String;
    if exact || symbol.contains("::") {
        qualified_start = symbol.to_string();
    } else {
        let disambig = code_disambiguate_query_pg(pool, symbol, source_id).await?;
        if disambig.matches.is_empty() {
            let dym = disambig
                .suggestions
                .into_iter()
                .map(|s| DidYouMeanCandidate {
                    symbol_qualified: s,
                    score: 0.5,
                })
                .collect();
            return Ok(RecursiveWalkResult::NotFound { did_you_mean: dym });
        }
        if disambig.matches.len() > 1 {
            let candidates = disambig
                .matches
                .into_iter()
                .map(|m| AmbiguousCandidate {
                    symbol_qualified: m,
                    lang: None,
                    file: None,
                    lines: None,
                })
                .collect();
            return Ok(RecursiveWalkResult::Ambiguous { candidates });
        }
        qualified_start = disambig.matches[0].clone();
    }

    // Step 2: language gate — get starting symbol's language
    let start_lang = 'find_lang: {
        let row = sqlx::query(
            r#"SELECT cc.language
               FROM content_chunks cc
               JOIN pages p ON p.id = cc.page_id
              WHERE p.source_id = $1
                AND cc.symbol_name_qualified = $2
              LIMIT 1"#,
        )
        .bind(source_id)
        .bind(&qualified_start)
        .fetch_optional(pool)
        .await
        .map_err(|e| crate::Error::engine(format!("recursive-walk language lookup failed: {e}")))?;
        match row {
            Some(r) => r.try_get::<Option<String>, _>("language").unwrap_or(None),
            None => None,
        }
    };

    if let Some(lang) = &start_lang {
        if !SUPPORTED_LANGS.contains(&lang.as_str()) {
            let supported: Vec<String> = SUPPORTED_LANGS.iter().map(|s| s.to_string()).collect();
            return Ok(RecursiveWalkResult::UnsupportedLanguage { supported });
        }
    }

    // Step 3: BFS walk
    let mut visited = std::collections::HashSet::<String>::new();
    let mut depth_groups: Vec<DepthGroup> = Vec::new();
    let mut cycles_detected = false;
    let mut truncation = WalkTruncation::None;
    let mut total_nodes = 0;
    let mut terminal_nodes = Vec::new();
    let freshness = WalkFreshness::Fresh;

    visited.insert(qualified_start.clone());
    let mut frontier = vec![qualified_start];

    for d in 1..=depth_cap {
        if truncation != WalkTruncation::None {
            break;
        }
        let mut next_frontier = Vec::new();
        let mut nodes_this_depth = Vec::new();

        'frontier_loop: for sym in frontier.iter() {
            // Get edges using existing code-edge query
            let (symbol_col, next_sym_extractor): (&str, Box<dyn Fn(&crate::import::CodeEdgeResult) -> Option<&String> + Send + Sync>) = match opts.direction {
                crate::import::WalkDirection::Callers => {
                    // callers of sym = edges where to_symbol_qualified = sym → next is from_symbol_qualified
                    ("to_symbol_qualified", Box::new(|e| Some(&e.from_symbol_qualified)))
                }
                crate::import::WalkDirection::Callees => {
                    // callees of sym = edges where from_symbol_qualified = sym → next is to_symbol_qualified
                    ("from_symbol_qualified", Box::new(|e| Some(&e.to_symbol_qualified)))
                }
            };
            let edges = code_edge_symbol_query_pg(
                engine,
                symbol_col,
                sym,
                &crate::import::CodeGraphQueryOpts {
                    source_id: Some(source_id.to_string()),
                    all_sources: false,
                    limit: Some(max_nodes - total_nodes),
                    ..Default::default()
                },
            )
            .await?;

            for e in edges {
                let next_sym = next_sym_extractor(&e);
                let Some(next_sym_str) = next_sym else {
                    continue;
                };
                if next_sym_str == sym {
                    continue; // self-loop skip
                }
                if visited.contains(next_sym_str) {
                    cycles_detected = true;
                    continue;
                }
                if total_nodes >= max_nodes {
                    truncation = match truncation {
                        WalkTruncation::None => WalkTruncation::MaxNodes,
                        WalkTruncation::DepthCap => WalkTruncation::Both,
                        _ => truncation,
                    };
                    break 'frontier_loop;
                }
                visited.insert(next_sym_str.clone());
                total_nodes += 1;

                let mut node = RecursiveWalkNode {
                    symbol: next_sym_str.clone(),
                    chunk_id: Some(e.from_chunk_id),
                    sink_kind: None,
                };

                // classify sink for callees direction when we have start language
                if matches!(opts.direction, crate::import::WalkDirection::Callees) && start_lang.is_some() {
                    if let Some(kind) = crate::code_intel::classify_sink(next_sym_str, start_lang.as_deref().unwrap_or("")) {
                        node.sink_kind = Some(kind.as_str().to_string());
                        terminal_nodes.push(crate::import::TerminalNode {
                            symbol: next_sym_str.clone(),
                            sink_kind: kind.as_str().to_string(),
                        });
                    }
                }

                nodes_this_depth.push(node);
                next_frontier.push(next_sym_str.clone());
            }
        }

        if !nodes_this_depth.is_empty() {
            let confidence = crate::engine::clamp_confidence(d);
            depth_groups.push(DepthGroup {
                depth: d,
                nodes: nodes_this_depth,
                confidence,
            });
        }
        if next_frontier.is_empty() {
            break;
        }
        if d == depth_cap && !next_frontier.is_empty() {
            truncation = match truncation {
                WalkTruncation::None => WalkTruncation::DepthCap,
                WalkTruncation::MaxNodes => WalkTruncation::Both,
                _ => truncation,
            };
        }
        frontier = next_frontier;
    }

    Ok(RecursiveWalkResult::Ok {
        depth_groups,
        cycles_detected,
        truncation,
        freshness,
        terminal_nodes: if terminal_nodes.is_empty() {
            None
        } else {
            Some(terminal_nodes)
        },
    })
}

fn code_def_row_to_result_pg(
    row: &sqlx::postgres::PgRow,
) -> crate::Result<crate::import::CodeDefResult> {
    let slug: String = row
        .try_get("slug")
        .map_err(|e| crate::Error::engine(format!("code_def (pg) decode slug: {e}")))?;
    let file: Option<String> = row
        .try_get("file")
        .map_err(|e| crate::Error::engine(format!("code_def (pg) decode file: {e}")))?;
    let language: Option<String> = row
        .try_get("language")
        .map_err(|e| crate::Error::engine(format!("code_def (pg) decode language: {e}")))?;
    let symbol_type: Option<String> = row
        .try_get("symbol_type")
        .map_err(|e| crate::Error::engine(format!("code_def (pg) decode symbol_type: {e}")))?;
    // PG stores start_line/end_line as INTEGER (INT4); widen to i64.
    let start_line: Option<i32> = row
        .try_get("start_line")
        .map_err(|e| crate::Error::engine(format!("code_def (pg) decode start_line: {e}")))?;
    let end_line: Option<i32> = row
        .try_get("end_line")
        .map_err(|e| crate::Error::engine(format!("code_def (pg) decode end_line: {e}")))?;
    let chunk_text: String = row
        .try_get("chunk_text")
        .map_err(|e| crate::Error::engine(format!("code_def (pg) decode chunk_text: {e}")))?;
    Ok(crate::import::CodeDefResult {
        slug,
        file,
        language,
        symbol_type,
        start_line: start_line.map(|v| v as i64),
        end_line: end_line.map(|v| v as i64),
        snippet: chunk_text.chars().take(500).collect(),
    })
}

fn code_ref_row_to_result_pg(
    row: &sqlx::postgres::PgRow,
) -> crate::Result<crate::import::CodeRefResult> {
    let slug: String = row
        .try_get("slug")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode slug: {e}")))?;
    let file: Option<String> = row
        .try_get("file")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode file: {e}")))?;
    let language: Option<String> = row
        .try_get("language")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode language: {e}")))?;
    let symbol_name: Option<String> = row
        .try_get("symbol_name")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode symbol_name: {e}")))?;
    let symbol_type: Option<String> = row
        .try_get("symbol_type")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode symbol_type: {e}")))?;
    let start_line: Option<i32> = row
        .try_get("start_line")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode start_line: {e}")))?;
    let end_line: Option<i32> = row
        .try_get("end_line")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode end_line: {e}")))?;
    let chunk_text: String = row
        .try_get("chunk_text")
        .map_err(|e| crate::Error::engine(format!("code_ref (pg) decode chunk_text: {e}")))?;
    Ok(crate::import::CodeRefResult {
        slug,
        file,
        language,
        symbol_name,
        symbol_type,
        start_line: start_line.map(|v| v as i64),
        end_line: end_line.map(|v| v as i64),
        snippet: chunk_text.chars().take(500).collect(),
    })
}

/// Convert a Postgres cell to a serde_json::Value based on its type.
fn pg_cell_to_json(row: &sqlx::postgres::PgRow, col_name: &str) -> serde_json::Value {
    // Try common types first, then fall back to deserializing via FromRow
    // This handles the most cases that come from calibration aggregations
    if let Ok(v) = row.try_get::<bool, _>(col_name) {
        return serde_json::Value::Bool(v);
    }
    if let Ok(v) = row.try_get::<i32, _>(col_name) {
        return serde_json::Value::Number(v.into());
    }
    if let Ok(v) = row.try_get::<i64, _>(col_name) {
        return serde_json::Value::Number(v.into());
    }
    if let Ok(v) = row.try_get::<f64, _>(col_name) {
        return serde_json::Number::from_f64(v)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
    }
    if let Ok(v) = row.try_get::<String, _>(col_name) {
        return serde_json::Value::String(v);
    }
    if let Ok(v) = row.try_get::<Option<bool>, _>(col_name) {
        return match v {
            Some(v) => serde_json::Value::Bool(v),
            None => serde_json::Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(col_name) {
        return match v {
            Some(v) => serde_json::Value::Number(v.into()),
            None => serde_json::Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(col_name) {
        return match v {
            Some(v) => serde_json::Value::Number(v.into()),
            None => serde_json::Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(col_name) {
        return match v {
            Some(v) => serde_json::Number::from_f64(v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<String>, _>(col_name) {
        return match v {
            Some(v) => serde_json::Value::String(v),
            None => serde_json::Value::Null,
        };
    }
    // Fallback: just return null for unknown types
    serde_json::Value::Null
}
