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
    page_sort_sql, BrainEngine, CreateSourceInput, EngineConfig, EngineKind, GetPageOpts, Page,
    PageFilters, PageInput, PageSort, ResolveSlugsOpts, SourceRow, UpdateSourceInput,
    is_valid_source_id,
};
use crate::oauth_queries::{
    ExchangeTokens, OAuthClientInfo, OAuthQueries, RegisterClientRequest,
    RegisterClientResponse, RevokeClientResponse, UpdateClientTtlResponse,
};
use crate::scope::{has_scope, parse_scope_string};
use crate::token_queries::{AuthInfo, TokenError, TokenQueries};

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
    LinkBatchInput, NewFact, PageKind, PageVersion, RawData, Take, TakeInput, UpsertFileResult,
    UpsertTakesResult,
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

    // --- Phase 7A: Takes ---

    async fn get_takes_for_page(&self, page_id: u64) -> Result<Vec<Take>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT id, page_id, row_num, claim, kind, holder, weight, \
                    since_date, until_date, source, superseded_by, active, \
                    resolved_at, resolved_quality, resolved_outcome, \
                    resolved_evidence, resolved_value, resolved_unit, \
                    resolved_by, created_at, updated_at \
             FROM takes WHERE page_id = $1 ORDER BY row_num ASC",
        )
        .bind(page_id as i64)
        .fetch_all(pool)
        .await
        .map_err(|e| Error::engine(format!("get_takes_for_page: {e}")))?;

        rows.into_iter()
            .map(|r| {
                Ok(Take {
                    id: r.try_get::<i64, _>("id").map(|v| v as u64)
                        .map_err(|e| Error::engine(format!("take id: {e}")))?,
                    page_id: r.try_get::<i64, _>("page_id").map(|v| v as u64)
                        .map_err(|e| Error::engine(format!("take page_id: {e}")))?,
                    row_num: r.try_get::<i32, _>("row_num")
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
                        let dt: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
                            r.try_get("created_at")
                                .map_err(|e| Error::engine(format!("take created_at: {e}")))?;
                        dt.to_rfc3339()
                    },
                    updated_at: {
                        let dt: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
                            r.try_get("updated_at")
                                .map_err(|e| Error::engine(format!("take updated_at: {e}")))?;
                        dt.to_rfc3339()
                    },
                })
            })
            .collect()
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
        let result = sqlx::query(
            "UPDATE takes SET \
                    resolved_at = $1, resolved_quality = $2, resolved_outcome = $3, \
                    resolved_evidence = $4, resolved_value = $5, resolved_unit = $6, \
                    resolved_by = $7, updated_at = $8 \
             WHERE page_id = $9 AND row_num = $10",
        )
        .bind(now)
        .bind(&resolution.quality)
        .bind(resolution.outcome)
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

        if result.rows_affected() == 0 {
            return Err(crate::error::StructuredError::new(
                "Not Found",
                "not_found",
                format!("no take found for page_id={page_id} row_num={row_num}"),
            ));
        }
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
                     context, valid_from, valid_until, source, source_session, confidence) \
                SELECT $1, $2, $3, $4, $5, $6, $7, $8::timestamptz, $9::timestamptz, $10, $11, $12 \
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

    // --- Background sweeps (1-1-2 C) ---
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
