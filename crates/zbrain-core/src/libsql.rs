//! Slice 5 — `LibsqlEngine` embedded `SQLite` backend.
//!
//! Mirrors `PostgresEngine` shape: lifecycle (connect / disconnect /
//! `init_schema`) + Page CRUD (`get` / `put` / `delete` / `list` / `resolve_slugs`)
//! against a local `SQLite` file via the `libsql` crate.
//!
//! Schema differs from PG only where the dialect forces it: `BIGSERIAL` →
//! `INTEGER PRIMARY KEY AUTOINCREMENT`, `TIMESTAMPTZ DEFAULT now()` →
//! `TEXT DEFAULT CURRENT_TIMESTAMP`, `$N` placeholders → `?N`. Migrations
//! are hand-rolled (no `sqlx::migrate`) because `libsql` lives outside sqlx —
//! we run `MIGRATION_SQL` once, guarded by `PRAGMA user_version`.
//!
//! Module name `libsql` shadows the extern crate `libsql` inside this
//! file; every reference to the crate uses the leading-`::libsql::…` form
//! to bypass the shadow.

use std::sync::{LazyLock, OnceLock};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::admin_queries::{
    AdminQueries, AgentClientSpend, AgentInfo, ApiKey, BrainStats, BudgetOwner, ErrorClusterCount,
    FullStats, HealthIndicators, JobTypeSummary, Paginated, QueueHealth, RequestLogEntry,
    RequestLogFilters, Stats, WatchSnapshot,
};
use crate::calibration_queries::{
    aggregate_calibration_curve, aggregate_scorecard, CalibrationBucket, CalibrationCurveQuery,
    CalibrationProfileRow, CalibrationQueries, CalibrationRow, CalibrationWaveQueries,
    PatternDetail, ScorecardQuery, ScorecardRow, TakeSummary, TakesScorecard, ThinkAbInsert,
    CalibrationProfileInsert,
};
use crate::oauth_queries::{
    OAuthQueries, RegisterClientRequest, RegisterClientResponse, RevokeClientResponse,
    UpdateClientTtlResponse,
};
use crate::engine::{
    fuse_and_boost, page_sort_sql, BrainEngine, CreateSourceInput, EngineConfig, EngineKind,
    GetPageOpts, Page, PageFilters, PageInput, PageSort, ResolveSlugsOpts, SearchOpts,
    SearchResult, SourceRow, UpdateSourceInput, is_valid_source_id,
};
use crate::error::{Error, Result};
use crate::migration::{Migration, MigrationRegistry};
use crate::time::current_utc_iso8601;
use crate::types::{
    CRMode, DuplicatePage, EffectiveDateSource, EntityCount, FactInsertStatus, FactKind,
    FactListOpts, FactRow, FactVisibility, FactsHealth, FileRow, FileSpec,
    FindDuplicatePageOpts, GraphPath, Link, LinkBatchInput, NewFact, OrphanPage, PageKind, PageRef,
    PageVersion, PurgeResult, RawData, RefreshPageBodyArgs, Take, TakeHit, TakeInput,
    TakesListOpts, SearchTakesOpts, UpsertFileResult, UpsertTakesResult, AdjacencyRow, Chunk,
    FileListRow, IngestLogEntry, IngestLogInput,
};

/// libsql-specific migration implementation. Wraps raw SQL from
/// migrations-sqlite/ files and implements the Migration trait.
#[derive(Debug, Clone)]
struct LibsqlMigration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

impl Migration for LibsqlMigration {
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

/// Embedded `SQLite` schema. Mirrors the PG migration semantics:
/// `sources` (with seeded 'default' row), `pages` with `UNIQUE (source_id,
/// slug)` and a `page_kind` CHECK, plus an index on `type`.
const MIGRATION_0001: &str = include_str!("../migrations-sqlite/0001_init.sql");

/// Slice 6a — fills in every `pages` column the 12 Page-CRUD methods need
/// (`frontmatter`, `emotional_weight`, `deleted_at`, …, `embedding`) plus the
/// generation-bump triggers. See file header in `0002_pages_full_columns.sql`.
const MIGRATION_0002: &str = include_str!("../migrations-sqlite/0002_pages_full_columns.sql");

/// Slice 6a S4 — adds `salience_score` and rebuilds `bump_page_generation_update`
/// to cover the full 10-column PG allow-list (adds `timeline` / `type` / `page_kind`).
/// See file header in `0003_salience_and_full_generation_trigger.sql`.
const MIGRATION_0003: &str =
    include_str!("../migrations-sqlite/0003_salience_and_full_generation_trigger.sql");

/// Slice 6a S6-T5c — adds `page_tags` table for tag filter in `list_pages`.
/// Composite primary key `(page_id, tag)` + `ON DELETE CASCADE` so hard
/// page deletes cleanly remove dangling tag rows. See file header in
/// `0004_page_tags.sql` for the TS reference and FK semantics.
const MIGRATION_0004: &str = include_str!("../migrations-sqlite/0004_page_tags.sql");

/// Slice 6c-takes-salience — adds minimal `takes` table (`id` / `page_id` /
/// `active` only). Required for the second term of the salience formula
/// `ln(1 + COUNT(DISTINCT t.id) WHERE t.active = 1)`. Full TS schema
/// (21 cols + vector + synthesis evidence) is deferred to a later
/// takes-CRUD slice. See file header in `0005_takes_min.sql` for the
/// subset rationale and FK semantics.
const MIGRATION_0005: &str = include_str!("../migrations-sqlite/0005_takes_min.sql");

/// Slice 6a-libsql find-orphan-pages — adds the minimal `links` table mirror
/// needed for inbound-link existence checks. Full link CRUD remains deferred.
const MIGRATION_0006: &str = include_str!("../migrations-sqlite/0006_links.sql");

/// File metadata rows for TS BrainEngine file-storage parity.
const MIGRATION_0007: &str = include_str!("../migrations-sqlite/0007_files.sql");
const MIGRATION_0008: &str =
    include_str!("../migrations-sqlite/0008_raw_data_and_page_versions.sql");
/// OAuth client, token, and authorization-code tables (PG→SQLite port).
const MIGRATION_0009: &str = include_str!("../migrations-sqlite/0009_oauth_tables.sql");
/// Expand `sources` table to full TS schema (PG→SQLite port).
const MIGRATION_0010: &str = include_str!("../migrations-sqlite/0010_sources_full_columns.sql");
/// Expand `takes` table to full TS schema (PG→SQLite port).
const MIGRATION_0012: &str = include_str!("../migrations-sqlite/0012_takes_full_columns.sql");
/// Create `facts` table with full 27-column TS schema (PG→SQLite port).
const MIGRATION_0013: &str = include_str!("../migrations-sqlite/0013_facts.sql");
/// Create `minion_jobs` table — SQLite port of the BullMQ-inspired job queue.
const MIGRATION_0014: &str = include_str!("../migrations-sqlite/0014_minion_jobs.sql");

/// Create `minion_inbox` table — SQLite port of the sidechannel inbox (1-1-3-1).
const MIGRATION_0015: &str = include_str!("../migrations-sqlite/0015_minion_inbox.sql");

/// Create `minion_attachments` table — SQLite port of per-job blob storage.
const MIGRATION_0016: &str = include_str!("../migrations-sqlite/0016_minion_attachments.sql");
const MIGRATION_0017: &str = include_str!("../migrations-sqlite/0017_minion_budget.sql");
const MIGRATION_0018: &str = include_str!("../migrations-sqlite/0018_rate_leases.sql");
/// 1-6-7-5: content chunks read side for the `get_chunks` op.
const MIGRATION_0019: &str = include_str!("../migrations-sqlite/0019_content_chunks.sql");
/// 1-6-7-5: ingest log for the `log_ingest` / `get_ingest_log` ops.
const MIGRATION_0020: &str = include_str!("../migrations-sqlite/0020_ingest_log.sql");
/// 1-6-7-10-1: code-graph edge storage (write side for code-intel ops).
const MIGRATION_0021: &str = include_str!("../migrations-sqlite/0021_code_edges.sql");
/// 1-6-7-11: search_by_image — image-search spend log table for daily budget tracking.
const MIGRATION_0022: &str = include_str!("../migrations-sqlite/0022_image_search_spend_log.sql");
const MIGRATION_0023: &str = include_str!("../migrations-sqlite/0023_calibration_tables.sql");

/// Legacy string array — REMOVED in favor of MigrationRegistry.
/// Use LIBQL_MIGRATIONS instead.
#[deprecated(note = "Use LIBQL_MIGRATIONS instead")]
const MIGRATIONS: &[&str] = &[
    MIGRATION_0001,
    MIGRATION_0002,
    MIGRATION_0003,
    MIGRATION_0004,
    MIGRATION_0005,
    MIGRATION_0006,
    MIGRATION_0007,
    MIGRATION_0008,
    MIGRATION_0009,
    MIGRATION_0010,
];

/// Legacy version constant — REMOVED in favor of MigrationRegistry.
/// Use LIBQL_MIGRATIONS.latest_version() instead.
#[deprecated(note = "Use LIBQL_MIGRATIONS.latest_version() instead")]
const SCHEMA_VERSION: i64 = 10;

/// Global migration registry for libsql backend. Built once at runtime first use.
/// All 8 existing migrations ported with zero SQL changes per 1-2-3-3 Q4 decision.
pub static LIBQL_MIGRATIONS: LazyLock<MigrationRegistry> = LazyLock::new(|| {
    let mut registry = MigrationRegistry::new();

    // Wrap each raw SQL file in LibsqlMigration. Version numbers start at 1,
    // matching the original TS-era numbering. Names are descriptive for
    // debugging and audit trail purposes only; not used for execution logic.
    registry.add(Box::new(LibsqlMigration {
        version: 1,
        name: "init",
        sql: MIGRATION_0001,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 2,
        name: "pages_full_columns",
        sql: MIGRATION_0002,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 3,
        name: "salience_and_full_generation_trigger",
        sql: MIGRATION_0003,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 4,
        name: "page_tags",
        sql: MIGRATION_0004,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 5,
        name: "takes_min",
        sql: MIGRATION_0005,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 6,
        name: "links",
        sql: MIGRATION_0006,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 7,
        name: "files",
        sql: MIGRATION_0007,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 8,
        name: "raw_data_and_page_versions",
        sql: MIGRATION_0008,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 9,
        name: "oauth_tables",
        sql: MIGRATION_0009,
    }));

    registry.add(Box::new(LibsqlMigration {
        version: 10,
        name: "sources_full_columns",
        sql: MIGRATION_0010,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 12,
        name: "takes_full_columns",
        sql: MIGRATION_0012,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 13,
        name: "facts",
        sql: MIGRATION_0013,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 14,
        name: "minion_jobs",
        sql: MIGRATION_0014,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 15,
        name: "minion_inbox",
        sql: MIGRATION_0015,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 16,
        name: "minion_attachments",
        sql: MIGRATION_0016,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 17,
        name: "minion_budget",
        sql: MIGRATION_0017,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 18,
        name: "rate_leases",
        sql: MIGRATION_0018,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 19,
        name: "content_chunks",
        sql: MIGRATION_0019,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 20,
        name: "ingest_log",
        sql: MIGRATION_0020,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 21,
        name: "code_edges",
        sql: MIGRATION_0021,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 22,
        name: "mcp_spend_log",
        sql: MIGRATION_0022,
    }));
    registry.add(Box::new(LibsqlMigration {
        version: 23,
        name: "calibration_tables",
        sql: MIGRATION_0023,
    }));

    registry
});

/// Bootstrap migration 0: creates `rust_schema_version` table for tracking
/// migration progress. Version 0 means no migrations have run yet.
/// Hard cutover from TS-era `PRAGMA user_version` per 1-2-3 Q4 decision.
const RUST_SCHEMA_VERSION_BOOTSTRAP: &str = r#"
CREATE TABLE IF NOT EXISTS rust_schema_version (
    version INTEGER PRIMARY KEY NOT NULL DEFAULT 0,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT OR IGNORE INTO rust_schema_version (version) VALUES (0);
"#;

/// Process-wide gate that serializes the *entire* `init_schema` flow
/// — including the very first `connect()` + `PRAGMA foreign_keys = ON`
/// on a freshly opened file — across **all** `LibsqlEngine` instances.
///
/// Why a process-wide lock and not a per-engine one: each test owns its
/// own `NamedTempFile` + `LibsqlEngine`, so per-engine state cannot
/// serialize anything across tests. Yet running 32 fresh engines in
/// parallel (see `libsql_init_schema_flake_reproduce.rs`) still trips
/// rare cold-start races inside the shared `libsql` / `SQLite` FFI
/// initialization paths — observed signature is `enable foreign_keys
/// failed: SQLite failure: bad parameter or other API misuse` emitted
/// from inside [`LibsqlEngine::conn`] before any migration write
/// executes. A single static `tokio::sync::Mutex` covering the whole
/// `init_schema` body makes that cold-start FFI sequence strictly
/// serial process-wide.
///
/// Why no fast-path / DCL: an earlier revision tried a lock-free
/// fast-path that read `PRAGMA user_version` outside the lock. That
/// shape forced the unguarded `self.conn().await?` to run concurrently
/// — which is exactly the FFI path that races. The fast-path was an
/// optimization that masked the real bug. We pay one extra
/// `PRAGMA user_version` round-trip per `init_schema` call, which only
/// matters once per engine / process anyway.
static SCHEMA_INIT_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Embedded `SQLite` engine. Use [`LibsqlEngine::new`] then [`connect`] before
/// any other method. Calling `connect` twice on the same instance is
/// rejected to keep ownership of the underlying `Database` handle clean.
/// TODO：单例中单线程，借助线程的消息循环序列化所有对数据库的读写操作，避免竞态问题。
pub struct LibsqlEngine {
    db: OnceLock<::libsql::Database>,
    db_path: OnceLock<String>,
}

impl LibsqlEngine {
    /// Construct a disconnected engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            db: OnceLock::new(),
            db_path: OnceLock::new(),
        }
    }

    fn database(&self) -> Result<&::libsql::Database> {
        self.db
            .get()
            .ok_or_else(|| Error::engine("LibsqlEngine is not connected"))
    }

    /// Open a fresh connection on the live database and enable foreign
    /// key enforcement on it.
    ///
    /// `libsql::Connection` is cheap to acquire (just an FFI handle bound
    /// to the open file), but `SQLite`'s `PRAGMA foreign_keys` is **per
    /// connection** — flipping it once on the `Database` does nothing for
    /// subsequent connections. So we re-issue the PRAGMA after every
    /// `connect()` call here. This guarantees `ON DELETE CASCADE` from
    /// `migration 0004` (`page_tags.page_id` → `pages.id`) actually fires.
    async fn conn(&self) -> Result<::libsql::Connection> {
        let conn = self
            .database()?
            .connect()
            .map_err(|e| Error::engine(format!("libsql connect failed: {e}")))?;
        conn.execute("PRAGMA foreign_keys = ON", ())
            .await
            .map_err(|e| Error::engine(format!("enable foreign_keys failed: {e}")))?;
        Ok(conn)
    }

    /// Read current migration version from a FRESH connection to the database
    /// file. This bypasses the cached `Database` handle and its connection pool
    /// to avoid libsql local-mode WAL visibility edge cases where new
    /// connections via `Database::connect()` may not see committed WAL entries
    /// from a previous `init_schema()` call on the same `Database`.
    async fn read_rust_schema_version_fresh(&self) -> Result<i64> {
        let path = self
            .db_path
            .get()
            .ok_or_else(|| Error::engine("LibsqlEngine is not connected"))?;
        let db = ::libsql::Builder::new_local(path)
            .build()
            .await
            .map_err(|e| Error::engine(format!("fresh version read open failed: {e}")))?;
        let conn = db
            .connect()
            .map_err(|e| Error::engine(format!("fresh version read connect failed: {e}")))?;
        Self::read_rust_schema_version_from_conn(&conn).await
    }

    /// Read current migration version from `rust_schema_version` table via
    /// a specific connection. Always returns 0 for fresh databases
    /// (guaranteed by bootstrap migration).
    async fn read_rust_schema_version_from_conn(conn: &::libsql::Connection) -> Result<i64> {
        let mut rows = conn
            .query("SELECT version FROM rust_schema_version LIMIT 1", ())
            .await
            .map_err(|e| Error::engine(format!("read rust_schema_version failed: {e}")))?;
        rows.next()
            .await
            .map_err(|e| Error::engine(format!("rust_schema_version row fetch failed: {e}")))?
            .ok_or_else(|| {
                Error::engine("rust_schema_version returned no row - run bootstrap first")
            })?
            .get(0)
            .map_err(|e| Error::engine(format!("rust_schema_version decode failed: {e}")))
    }

    /// Update `rust_schema_version` to the given version number after a
    /// successful migration run.
    async fn set_rust_schema_version(conn: &::libsql::Connection, ver: i64) -> Result<()> {
        conn.execute("UPDATE rust_schema_version SET version = ?", ::libsql::params![ver])
            .await
            .map_err(|e| Error::engine(format!("set rust_schema_version = {ver} failed: {e}")))?;
        Ok(())
    }
}

fn libsql_row_to_file(row: &::libsql::Row) -> Result<FileRow> {
    let id: i64 = row
        .get(0)
        .map_err(|e| Error::engine(format!("file row decode id: {e}")))?;
    let source_id: String = row
        .get(1)
        .map_err(|e| Error::engine(format!("file row decode source_id: {e}")))?;
    let page_slug: Option<String> = row
        .get(2)
        .map_err(|e| Error::engine(format!("file row decode page_slug: {e}")))?;
    let page_id_i64: Option<i64> = row
        .get(3)
        .map_err(|e| Error::engine(format!("file row decode page_id: {e}")))?;
    let filename: String = row
        .get(4)
        .map_err(|e| Error::engine(format!("file row decode filename: {e}")))?;
    let storage_path: String = row
        .get(5)
        .map_err(|e| Error::engine(format!("file row decode storage_path: {e}")))?;
    let mime_type: Option<String> = row
        .get(6)
        .map_err(|e| Error::engine(format!("file row decode mime_type: {e}")))?;
    let size_bytes: Option<i64> = row
        .get(7)
        .map_err(|e| Error::engine(format!("file row decode size_bytes: {e}")))?;
    let content_hash: String = row
        .get(8)
        .map_err(|e| Error::engine(format!("file row decode content_hash: {e}")))?;
    let metadata_text: String = row
        .get(9)
        .map_err(|e| Error::engine(format!("file row decode metadata: {e}")))?;
    let created_at: String = row
        .get(10)
        .map_err(|e| Error::engine(format!("file row decode created_at: {e}")))?;
    let metadata = serde_json::from_str(&metadata_text).unwrap_or_else(|_| json!({}));

    Ok(FileRow {
        id: id as u64,
        source_id,
        page_slug,
        page_id: page_id_i64.map(|value| value as u64),
        filename,
        storage_path,
        mime_type,
        size_bytes,
        content_hash,
        metadata,
        created_at,
    })
}

/// 1-6-7-5: decode a `content_chunks` row into the read-side [`Chunk`].
fn libsql_row_to_chunk(row: &::libsql::Row) -> Result<Chunk> {
    let page_id: i64 = row
        .get(0)
        .map_err(|e| Error::engine(format!("chunk row decode page_id: {e}")))?;
    let chunk_index: i64 = row
        .get(1)
        .map_err(|e| Error::engine(format!("chunk row decode chunk_index: {e}")))?;
    let chunk_text: String = row
        .get(2)
        .map_err(|e| Error::engine(format!("chunk row decode chunk_text: {e}")))?;
    let chunk_source: String = row
        .get(3)
        .map_err(|e| Error::engine(format!("chunk row decode chunk_source: {e}")))?;
    let model: Option<String> = row
        .get(4)
        .map_err(|e| Error::engine(format!("chunk row decode model: {e}")))?;
    let token_count: Option<i64> = row
        .get(5)
        .map_err(|e| Error::engine(format!("chunk row decode token_count: {e}")))?;
    let language: Option<String> = row
        .get(6)
        .map_err(|e| Error::engine(format!("chunk row decode language: {e}")))?;
    let symbol_name: Option<String> = row
        .get(7)
        .map_err(|e| Error::engine(format!("chunk row decode symbol_name: {e}")))?;
    let symbol_type: Option<String> = row
        .get(8)
        .map_err(|e| Error::engine(format!("chunk row decode symbol_type: {e}")))?;
    let start_line: Option<i64> = row
        .get(9)
        .map_err(|e| Error::engine(format!("chunk row decode start_line: {e}")))?;
    let end_line: Option<i64> = row
        .get(10)
        .map_err(|e| Error::engine(format!("chunk row decode end_line: {e}")))?;
    let parent_symbol_path: Option<String> = row
        .get(11)
        .map_err(|e| Error::engine(format!("chunk row decode parent_symbol_path: {e}")))?;
    let doc_comment: Option<String> = row
        .get(12)
        .map_err(|e| Error::engine(format!("chunk row decode doc_comment: {e}")))?;
    let symbol_name_qualified: Option<String> = row
        .get(13)
        .map_err(|e| Error::engine(format!("chunk row decode symbol_name_qualified: {e}")))?;
    let created_at: String = row
        .get(14)
        .map_err(|e| Error::engine(format!("chunk row decode created_at: {e}")))?;

    Ok(Chunk {
        page_id,
        chunk_index,
        chunk_text,
        chunk_source,
        model,
        token_count,
        language,
        symbol_name,
        symbol_type,
        start_line,
        end_line,
        parent_symbol_path,
        doc_comment,
        symbol_name_qualified,
        created_at,
    })
}

/// 1-6-7-5: decode an `ingest_log` row into the read-side [`IngestLogEntry`].
fn libsql_row_to_ingest_log(row: &::libsql::Row) -> Result<IngestLogEntry> {
    let id: i64 = row
        .get(0)
        .map_err(|e| Error::engine(format!("ingest row decode id: {e}")))?;
    let source_id: String = row
        .get(1)
        .map_err(|e| Error::engine(format!("ingest row decode source_id: {e}")))?;
    let source_type: String = row
        .get(2)
        .map_err(|e| Error::engine(format!("ingest row decode source_type: {e}")))?;
    let source_ref: String = row
        .get(3)
        .map_err(|e| Error::engine(format!("ingest row decode source_ref: {e}")))?;
    let pages_updated_text: String = row
        .get(4)
        .map_err(|e| Error::engine(format!("ingest row decode pages_updated: {e}")))?;
    let summary: String = row
        .get(5)
        .map_err(|e| Error::engine(format!("ingest row decode summary: {e}")))?;
    let created_at: String = row
        .get(6)
        .map_err(|e| Error::engine(format!("ingest row decode created_at: {e}")))?;
    let pages_updated: Vec<String> = serde_json::from_str(&pages_updated_text).unwrap_or_default();

    Ok(IngestLogEntry {
        id,
        source_id,
        source_type,
        source_ref,
        pages_updated,
        summary,
        created_at,
    })
}

impl Default for LibsqlEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LibsqlEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibsqlEngine")
            .field("connected", &self.db.get().is_some())
            .finish()
    }
}

/// Append a per-token takes-holder allow-list filter to a SQL query being
/// built with `::libsql::params_from_iter`. When `allow_list` is `None`, no
/// clause is added (trusted local caller sees all holders). When `Some(list)`,
/// an `AND holder IN (?, ?, ...)` clause is appended with one positional param
/// per holder (indices auto-align via `values.len()`). An empty list fails
/// closed — a restricted token with no permitted holders sees nothing.
///
/// Port of the Postgres `AND ($N::text[] IS NULL OR t.holder = ANY($N))`
/// clause; SQLite has no array `ANY`, so we expand to an `IN` list.
fn append_takes_holder_filter(
    sql: &mut String,
    values: &mut Vec<::libsql::Value>,
    allow_list: &Option<Vec<String>>,
) {
    match allow_list {
        None => {}
        Some(list) if list.is_empty() => {
            sql.push_str(" AND 0=1");
        }
        Some(list) => {
            let placeholders: Vec<String> = (0..list.len())
                .map(|i| format!("?{}", values.len() + i + 1))
                .collect();
            sql.push_str(&format!(" AND holder IN ({})", placeholders.join(", ")));
            for h in list {
                values.push(::libsql::Value::from(h.clone()));
            }
        }
    }
}

/// Map a `takes` SELECT row (21-column projection shared by
/// `get_takes_for_page` / `list_takes`) to a [`Take`].
fn take_from_row(row: &::libsql::Row) -> Result<Take> {
    Ok(Take {
        id: row.get::<i64>(0).map_err(|e| Error::engine(format!("take id: {e}")))? as u64,
        page_id: row.get::<i64>(1).map_err(|e| Error::engine(format!("take page_id: {e}")))? as u64,
        row_num: row.get::<i64>(2).map_err(|e| Error::engine(format!("take row_num: {e}")))? as i32,
        claim: row.get(3).unwrap_or_default(),
        kind: row.get(4).unwrap_or_default(),
        holder: row.get(5).unwrap_or_default(),
        weight: row.get::<f64>(6).unwrap_or(0.5),
        since_date: row.get(7).unwrap_or(None),
        until_date: row.get(8).unwrap_or(None),
        source: row.get(9).unwrap_or(None),
        superseded_by: row.get::<Option<i64>>(10).unwrap_or(None).map(|v| v as i32),
        active: row.get::<i64>(11).unwrap_or(1) != 0,
        resolved_at: row.get(12).unwrap_or(None),
        resolved_quality: row.get(13).unwrap_or(None),
        resolved_outcome: row.get::<Option<i64>>(14).unwrap_or(None).map(|v| v != 0),
        resolved_evidence: row.get(15).unwrap_or(None),
        resolved_value: row.get(16).unwrap_or(None),
        resolved_unit: row.get(17).unwrap_or(None),
        resolved_by: row.get(18).unwrap_or(None),
        created_at: row.get(19).unwrap_or_default(),
        updated_at: row.get(20).unwrap_or_default(),
    })
}

#[async_trait]
impl BrainEngine for LibsqlEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Libsql
    }

    async fn brain_identity(&self) -> crate::error::Result<crate::engine::BrainIdentity> {
        // Libsql exposes admin stats; populate the real page/chunk counts.
        let full = crate::admin_queries::AdminQueries::get_full_stats(self).await?;
        Ok(crate::engine::BrainIdentity {
            version: env!("CARGO_PKG_VERSION").to_string(),
            engine: crate::engine::engine_kind_str(self.kind()).to_string(),
            page_count: full.page_count,
            chunk_count: full.chunk_count,
            last_sync_iso: None,
        })
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    async fn connect(&self, config: &EngineConfig) -> Result<()> {
        let path = config
            .database_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::engine("LibsqlEngine requires EngineConfig.database_path"))?;

        let db = ::libsql::Builder::new_local(path)
            .build()
            .await
            .map_err(|e| Error::engine(format!("libsql open failed: {e}")))?;

        self.db_path
            .set(path.to_string())
            .map_err(|_| Error::engine("LibsqlEngine is already connected"))?;
        self.db
            .set(db)
            .map_err(|_| Error::engine("LibsqlEngine is already connected"))?;
        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        // libsql holds no pool — handles are released when the engine drops.
        // disconnect is a contract-honoring no-op so callers can treat all
        // engines uniformly.
        Ok(())
    }

    // ── Part12 1-1-2: extract-atoms discovery ────────────────────────────

    async fn discover_extractable_pages(
        &self,
        source_id: &str,
        affected_slugs: Option<&[String]>,
    ) -> crate::Result<Vec<crate::types::DiscoveredPage>> {
        let conn = self.conn().await?;
        let mut sql = String::from(
            "SELECT p.slug, p.compiled_truth, p.content_hash \
             FROM pages p \
             WHERE p.source_id = ?1 \
               AND p.type IN ('meeting','source','article','video','book','original') \
               AND p.deleted_at IS NULL \
               AND p.content_hash IS NOT NULL \
               AND COALESCE(p.frontmatter->>'imported_from', '') <> 'markdown-greenfield' \
               AND COALESCE(p.frontmatter->>'dream_generated', '') <> 'true' \
               AND length(COALESCE(p.compiled_truth, '')) >= ?2 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM pages atom \
                 WHERE atom.type = 'atom' \
                   AND atom.source_id = ?1 \
                   AND atom.frontmatter->>'source_hash' = substring(p.content_hash, 1, 16) \
                   AND atom.deleted_at IS NULL \
               )",
        );
        let mut params: Vec<::libsql::Value> =
            vec![::libsql::Value::Text(source_id.to_string()), ::libsql::Value::Integer(500)];
        if let Some(slugs) = affected_slugs {
            if !slugs.is_empty() {
                sql.push_str(" AND p.slug IN (");
                for (i, s) in slugs.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                    sql.push_str(&(params.len() + 1).to_string());
                    params.push(::libsql::Value::Text(s.clone()));
                }
                sql.push(')');
            }
        }
        sql.push_str(" ORDER BY p.updated_at DESC LIMIT ?");
        sql.push_str(&(params.len() + 1).to_string());
        params.push(::libsql::Value::Integer(50));
        let mut rows = conn
            .query(&sql, params)
            .await
            .map_err(|e| Error::engine(format!("discover_extractable_pages: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("discover_extractable_pages read: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("discover slug: {e}")))?;
            let content: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("discover content: {e}")))?;
            let content_hash: String = row
                .get(2)
                .map_err(|e| Error::engine(format!("discover hash: {e}")))?;
            out.push(crate::types::DiscoveredPage {
                slug,
                content,
                content_hash,
            });
        }
        Ok(out)
    }

    async fn atom_exists_for_hash(
        &self,
        source_id: &str,
        content_hash_16: &str,
    ) -> crate::Result<bool> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT 1 AS existing FROM pages \
                 WHERE type = 'atom' \
                   AND source_id = ?1 \
                   AND frontmatter->>'source_hash' = ?2 \
                   AND deleted_at IS NULL \
                 LIMIT 1",
                ::libsql::params![source_id, content_hash_16],
            )
            .await
            .map_err(|e| Error::engine(format!("atom_exists_for_hash: {e}")))?;
        Ok(rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("atom_exists read: {e}")))?
            .is_some())
    }

    async fn get_source_by_github_repo(
        &self,
        github_repo: &str,
    ) -> Result<Option<SourceRow>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, name, config FROM sources WHERE json_extract(config, '$.github_repo') = ?1 LIMIT 1",
                [github_repo],
            )
            .await
            .map_err(|e| Error::engine(format!("source lookup failed: {e}")))?;
        match rows.next().await
            .map_err(|e| Error::engine(format!("source row read failed: {e}")))?
        {
            Some(row) => {
                let config_str: String = row.get::<String>(2)
                    .map_err(|e| Error::engine(format!("config field read failed: {e}")))?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or_default();
                Ok(Some(SourceRow {
                    id: row.get::<String>(0)
                        .map_err(|e| Error::engine(format!("id field read failed: {e}")))?,
                    name: row.get::<String>(1)
                        .map_err(|e| Error::engine(format!("name field read failed: {e}")))?,
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
            None => Ok(None),
        }
    }

    async fn list_sources(&self, _include_archived: bool) -> Result<Vec<SourceRow>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query("SELECT id, name, config FROM sources", ())
            .await
            .map_err(|e| Error::engine(format!("list_sources failed: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("row read failed: {e}")))?
        {
            let config_str: String = row
                .get::<String>(2)
                .map_err(|e| Error::engine(format!("config field read failed: {e}")))?;
            let config: serde_json::Value =
                serde_json::from_str(&config_str).unwrap_or_default();
            out.push(SourceRow {
                id: row
                    .get::<String>(0)
                    .map_err(|e| Error::engine(format!("id field read failed: {e}")))?,
                name: row
                    .get::<String>(1)
                    .map_err(|e| Error::engine(format!("name field read failed: {e}")))?,
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
            });
        }
        Ok(out)
    }

    async fn get_source(&self, id: &str) -> Result<Option<SourceRow>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query("SELECT id, name, config FROM sources WHERE id = ?1", [id])
            .await
            .map_err(|e| Error::engine(format!("get_source failed: {e}")))?;
        match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("row read failed: {e}")))?
        {
            Some(row) => {
                let config_str: String = row
                    .get::<String>(2)
                    .map_err(|e| Error::engine(format!("config field read failed: {e}")))?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or_default();
                Ok(Some(SourceRow {
                    id: row
                        .get::<String>(0)
                        .map_err(|e| Error::engine(format!("id field read failed: {e}")))?,
                    name: row
                        .get::<String>(1)
                        .map_err(|e| Error::engine(format!("name field read failed: {e}")))?,
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
            None => Ok(None),
        }
    }

    async fn source_sync_stats(
        &self,
    ) -> Result<Vec<crate::sync_status::SourceSyncStat>> {
        let conn = self.conn().await?;

        // Source identity + sync metadata.
        let mut src_rows = conn
            .query(
                "SELECT id, name, local_path, last_commit, last_sync_at, config FROM sources",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("source_sync_stats sources failed: {e}")))?;
        let mut sources: Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            serde_json::Value,
        )> = Vec::new();
        while let Some(row) = src_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("row read failed: {e}")))?
        {
            let id: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("id read failed: {e}")))?;
            let name: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("name read failed: {e}")))?;
            let local_path: Option<String> =
                row.get::<Option<String>>(2).unwrap_or(None);
            let last_commit: Option<String> =
                row.get::<Option<String>>(3).unwrap_or(None);
            let last_sync_at: Option<String> =
                row.get::<Option<String>>(4).unwrap_or(None);
            let config_str: String = row
                .get(5)
                .map_err(|e| Error::engine(format!("config read failed: {e}")))?;
            let config: serde_json::Value =
                serde_json::from_str(&config_str).unwrap_or_default();
            sources.push((id, name, local_path, last_commit, last_sync_at, config));
        }

        // Per-source live page counts.
        let mut page_rows = conn
            .query(
                "SELECT source_id, COUNT(*) AS pages FROM pages WHERE deleted_at IS NULL GROUP BY source_id",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("source_sync_stats pages failed: {e}")))?;
        let mut page_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        while let Some(row) = page_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("row read failed: {e}")))?
        {
            let sid: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("source_id read failed: {e}")))?;
            let pages: i64 = row
                .get(1)
                .map_err(|e| Error::engine(format!("pages read failed: {e}")))?;
            page_counts.insert(sid, pages.max(0) as u64);
        }

        // Per-source chunk counts + unembedded (exclude deleted pages, mirroring
        // TS buildSyncStatusReport). `SUM(CASE WHEN ... IS NULL)` is portable
        // across libsql/SQLite and Postgres.
        let mut chunk_rows = conn
            .query(
                "SELECT c.source_id, COUNT(*) AS chunks_total, \
                 SUM(CASE WHEN c.embedding IS NULL THEN 1 ELSE 0 END) AS chunks_unembedded \
                 FROM content_chunks c JOIN pages p ON p.id = c.page_id \
                 WHERE p.deleted_at IS NULL GROUP BY c.source_id",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("source_sync_stats chunks failed: {e}")))?;
        let mut chunk_counts: std::collections::HashMap<String, (u64, u64)> =
            std::collections::HashMap::new();
        while let Some(row) = chunk_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("row read failed: {e}")))?
        {
            let sid: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("source_id read failed: {e}")))?;
            let total: i64 = row
                .get(1)
                .map_err(|e| Error::engine(format!("chunks_total read failed: {e}")))?;
            let unembedded: i64 = row
                .get(2)
                .map_err(|e| Error::engine(format!("chunks_unembedded read failed: {e}")))?;
            chunk_counts.insert(sid, (total.max(0) as u64, unembedded.max(0) as u64));
        }

        let mut out: Vec<crate::sync_status::SourceSyncStat> = Vec::new();
        for (id, name, local_path, last_commit, last_sync_at, config) in sources {
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
        let conn = self.conn().await?;
        let config = input.config.clone().unwrap_or_default();
        let config_str = serde_json::to_string(&config).unwrap_or_else(|_| "{}".into());
        conn.execute(
            "INSERT OR IGNORE INTO sources (id, name, config) VALUES (?1, ?2, ?3)",
            libsql::params![input.id.clone(), input.name.clone(), config_str],
        )
        .await
        .map_err(|e| Error::engine(format!("create_source failed: {e}")))?;
        // Read back to get created_at
        self.get_source(&input.id)
            .await?
            .ok_or_else(|| Error::engine(format!("source id '{}' already exists", input.id)))
    }

    async fn update_source(&self, id: &str, input: &UpdateSourceInput) -> Result<SourceRow> {
        let conn = self.conn().await?;
        let mut sets: Vec<String> = Vec::new();
        let mut params: Vec<libsql::Value> = Vec::new();
        if let Some(ref name) = input.name {
            sets.push("name = ?".into());
            params.push(name.as_str().into());
        }
        if let Some(ref config) = input.config {
            let s = serde_json::to_string(config).unwrap_or_else(|_| "{}".into());
            sets.push("config = ?".into());
            params.push(s.into());
        }
        if let Some(ref local_path) = input.local_path {
            sets.push("local_path = ?".into());
            params.push(local_path.as_str().into());
        }
        if let Some(ref last_commit) = input.last_commit {
            sets.push("last_commit = ?".into());
            params.push(last_commit.as_str().into());
        }
        if let Some(ref last_sync_at) = input.last_sync_at {
            sets.push("last_sync_at = ?".into());
            params.push(last_sync_at.as_str().into());
        }
        if let Some(ref chunker_version) = input.chunker_version {
            sets.push("chunker_version = ?".into());
            params.push(chunker_version.as_str().into());
        }
        if let Some(ref mode) = input.contextual_retrieval_mode {
            sets.push("contextual_retrieval_mode = ?".into());
            params.push(mode.as_str().into());
        }
        if let Some(trust) = input.trust_frontmatter_overrides {
            sets.push("trust_frontmatter_overrides = ?".into());
            params.push(libsql::Value::from(trust as i64));
        }
        if sets.is_empty() {
            return self
                .get_source(id)
                .await?
                .ok_or_else(|| Error::engine(format!("source '{}' not found", id)));
        }
        params.push(id.into());
        let sql = format!("UPDATE sources SET {} WHERE id = ?", sets.join(", "));
        conn.execute(&sql, params)
            .await
            .map_err(|e| Error::engine(format!("update_source failed: {e}")))?;
        self.get_source(id)
            .await?
            .ok_or_else(|| Error::engine(format!("source '{}' not found", id)))
    }

    async fn delete_source(&self, id: &str) -> Result<bool> {
        let conn = self.conn().await?;
        let rows_affected = conn
            .execute(
                "UPDATE sources SET archived = 1, archived_at = datetime('now'), \
                 archive_expires_at = datetime('now', '+72 hours') \
                 WHERE id = ?1 AND archived = 0",
                [id],
            )
            .await
            .map_err(|e| Error::engine(format!("delete_source failed: {e}")))?;
        Ok(rows_affected > 0)
    }

    async fn init_schema(&self) -> Result<()> {
        // Take the process-wide migration gate before touching the
        // connection at all. Empirically (see
        // `libsql_init_schema_flake_reproduce.rs`) the cold-start race
        // is **not** in the migration write step — it surfaces inside
        // libsql's first `connect()` + first `PRAGMA foreign_keys = ON`
        // on a freshly opened file ("bad parameter or other API misuse").
        // Guarding only the migration loop leaves that earlier FFI
        // sequence unprotected, so the lock must wrap the very first
        // `self.conn().await?` as well.
        let _guard = SCHEMA_INIT_LOCK.lock().await;

        // Step 1: Try reading the version table using a completely fresh
        // connection. If the table exists, we get the version directly.
        // If it doesn't exist (or we can't read it), we fall through to
        // bootstrap + migrate.
        //
        // Using a fresh Builder::new_local() connection avoids the libsql
        // local-mode WAL visibility problem where connections from a cached
        // Database::connect() may not see WAL entries committed by a
        // previous init_schema() call on the same Database instance.
        let current = self.read_rust_schema_version_fresh().await.map_or_else(
            |_| Err(()),
            Ok,
        );

        let latest = LIBQL_MIGRATIONS.latest_version();
        let current: i64 = match current {
            Ok(v) if v >= latest => return Ok(()),
            Ok(v) => v,
            Err(()) => 0, // fresh DB — no version table exists yet
        };

        // Step 2: Bootstrap the version tracking table and get a connection
        // for migration work.
        let conn = self.conn().await?;
        conn.execute_batch(RUST_SCHEMA_VERSION_BOOTSTRAP)
            .await
            .map_err(|e| Error::engine(format!("rust_schema_version bootstrap failed: {e}")))?;

        // Step 3: Apply all migrations in a single transaction (Q2 = C).
        // All-or-nothing atomicity: either all migrations apply successfully
        // or nothing changes. Version number is set once at the end.
        // Use conn.execute() (not execute_batch) for transaction control
        // to ensure BEGIN/COMMIT are issued on the same connection handle
        // that executes the migration DDL, avoiding libsql connection-pool
        // edge cases where execute_batch grabs a different handle.
        conn.execute("BEGIN TRANSACTION", ())
            .await
            .map_err(|e| Error::engine(format!("migration batch BEGIN failed: {e}")))?;

        let mut applied_any = false;
        for migration in LIBQL_MIGRATIONS.iter() {
            let ver = migration.version();
            if ver <= current {
                continue;
            }

            applied_any = true;
            match conn.execute_batch(migration.sql()).await {
                Ok(_) => {}
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(Error::engine(format!("migration {ver} failed: {e}")));
                }
            }
        }

        // Version number updated once at the end (single transaction mode)
        if applied_any {
            let latest = LIBQL_MIGRATIONS.latest_version();
            Self::set_rust_schema_version(&conn, latest).await?;
        }

        conn.execute("COMMIT", ())
            .await
            .map_err(|e| Error::engine(format!("migration batch COMMIT failed: {e}")))?;

        // Step 4: Run handler and verify hooks for all migrations that were just applied
        // Hooks run OUTSIDE the transaction (application-level logic may need to query DB)
        if applied_any {
            for migration in LIBQL_MIGRATIONS.iter() {
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

    // ── Page CRUD — slice 5 ───────────────────────────────────────────────
    // Same contract as PostgresEngine slice 4b. Differences live only in
    // dialect: `?N` placeholders, `INSERT … ON CONFLICT(source_id, slug)
    // DO UPDATE` (SQLite spelling), `LIMIT -1` as the unbounded sentinel.

    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> Result<Option<Page>> {
        // Slice 6a S6-T4: full 30-column projection backed by
        // `full_row_to_page`, with `deleted_at` filtering and optional
        // `source_id` scoping that mirror TS `getPage` semantics.
        //
        // Filters:
        // - `slug = ?1`
        // - `(?2 IS NULL OR source_id = ?2)` so `None` is unscoped
        // - `(?3 = 1 OR deleted_at IS NULL)` - default hides soft-deleted
        //
        // `include_deleted` is bound as an INTEGER (0/1) because libsql /
        // SQLite type affinity coerces TEXT booleans loosely; explicit i64
        // avoids any surprise.
        let conn = self.conn().await?;
        let include_deleted_flag: i64 = i64::from(opts.include_deleted);
        let source_id_param = opts.source_id.as_deref();
        let mut rows = conn
            .query(
                "SELECT id, slug, type, page_kind, title, compiled_truth, timeline, \
                        frontmatter, content_hash, emotional_weight, created_at, updated_at, \
                        deleted_at, last_retrieved_at, effective_date, effective_date_source, \
                        import_filename, salience_touched_at, salience_score, generation, \
                        embedding, chunker_version, source_path, source_id, source_kind, \
                        source_uri, ingested_via, ingested_at, contextual_retrieval_mode, \
                        corpus_generation \
                 FROM pages \
                 WHERE slug = ?1 \
                   AND (?2 IS NULL OR source_id = ?2) \
                   AND (?3 = 1 OR deleted_at IS NULL) \
                 ORDER BY source_id ASC \
                 LIMIT 1",
                ::libsql::params![slug, source_id_param, include_deleted_flag],
            )
            .await
            .map_err(|e| Error::engine(format!("get_page query failed: {e}")))?;

        match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_page row fetch failed: {e}")))?
        {
            Some(row) => Ok(Some(full_row_to_page(&row)?)),
            None => Ok(None),
        }
    }

    async fn put_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
        input: &PageInput,
    ) -> Result<Page> {
        let conn = self.conn().await?;

        // S6-T6 — 19-col INSERT mirroring TS pglite-engine.ts:866-887.
        // Column order (TS-locked): source_id, slug, type, page_kind, title,
        // compiled_truth, timeline, frontmatter, content_hash, updated_at (now()),
        // effective_date, effective_date_source, import_filename,
        // chunker_version (COALESCE), source_path, source_kind, source_uri,
        // ingested_via, ingested_at.
        //
        // ON CONFLICT path:
        //   * 8 cols unconditionally overwritten via `excluded.*`
        //     (type, page_kind, title, compiled_truth, timeline, frontmatter,
        //      content_hash, updated_at).
        //   * 9 cols COALESCE-preserved so null input keeps the prior value
        //     (effective_date, effective_date_source, import_filename,
        //      chunker_version, source_path, source_kind, source_uri,
        //      ingested_via, ingested_at).
        //
        // Server-stamp rule: `ingested_at = now()` ONLY when any of source_kind /
        // source_uri / ingested_via is non-null this call (provenance write-through).
        // The caller's `input.ingested_at` is intentionally ignored to match TS.
        //
        // Defaults / coercions:
        //   * source_id defaults to "default" when caller passes `None`
        //     (S6-T8 lifted the S6-T6 hardcoded literal).
        //   * page_kind defaults to "markdown" when input omits it.
        //   * timeline defaults to "" (NOT NULL column).
        //   * frontmatter defaults to "{}" (NOT NULL JSON column).
        //   * chunker_version uses SQL COALESCE(?14, 1) so null binds to 1.
        let source_id = source_id.unwrap_or("default");
        let page_kind_wire = encode_page_kind(input.page_kind.unwrap_or(PageKind::Markdown));
        let timeline = input.timeline.clone().unwrap_or_default();
        let frontmatter_json = match &input.frontmatter {
            Some(value) => serde_json::to_string(value)
                .map_err(|e| Error::engine(format!("put_page frontmatter encode: {e}")))?,
            None => "{}".to_string(),
        };
        let effective_date_source_wire = input
            .effective_date_source
            .map(encode_effective_date_source);
        let ingested_at = if input.source_kind.is_some()
            || input.source_uri.is_some()
            || input.ingested_via.is_some()
        {
            Some(current_utc_iso8601())
        } else {
            None
        };

        // G24: `embedding` (f32-LE BLOB) IS now written here as ?20, giving the
        // page-level vector path a write route. INSERT binds it directly; the
        // UPDATE branch COALESCE-preserves it so an upsert with embedding=None
        // keeps the previously stored vector (matches PageInput.embedding doc).
        let sql = "INSERT INTO pages (\
                source_id, slug, type, page_kind, title, compiled_truth, timeline, \
                frontmatter, content_hash, updated_at, effective_date, \
                effective_date_source, import_filename, chunker_version, \
                source_path, source_kind, source_uri, ingested_via, ingested_at, \
                embedding\
            ) VALUES (\
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                COALESCE(?14, 1), ?15, ?16, ?17, ?18, ?19, ?20\
            ) ON CONFLICT(source_id, slug) DO UPDATE SET \
                type = excluded.type, \
                page_kind = excluded.page_kind, \
                title = excluded.title, \
                compiled_truth = excluded.compiled_truth, \
                timeline = excluded.timeline, \
                frontmatter = excluded.frontmatter, \
                content_hash = excluded.content_hash, \
                updated_at = excluded.updated_at, \
                effective_date = COALESCE(excluded.effective_date, pages.effective_date), \
                effective_date_source = COALESCE(excluded.effective_date_source, pages.effective_date_source), \
                import_filename = COALESCE(excluded.import_filename, pages.import_filename), \
                chunker_version = COALESCE(excluded.chunker_version, pages.chunker_version), \
                source_path = COALESCE(excluded.source_path, pages.source_path), \
                source_kind = COALESCE(excluded.source_kind, pages.source_kind), \
                source_uri = COALESCE(excluded.source_uri, pages.source_uri), \
                ingested_via = COALESCE(excluded.ingested_via, pages.ingested_via), \
                ingested_at = COALESCE(excluded.ingested_at, pages.ingested_at), \
                embedding = COALESCE(excluded.embedding, pages.embedding) \
            RETURNING id, slug, type, page_kind, title, compiled_truth, timeline, \
                frontmatter, content_hash, emotional_weight, created_at, updated_at, \
                deleted_at, last_retrieved_at, effective_date, effective_date_source, \
                import_filename, salience_touched_at, salience_score, generation, \
                embedding, chunker_version, source_path, source_id, source_kind, \
                source_uri, ingested_via, ingested_at, contextual_retrieval_mode, \
                corpus_generation";

        let now = current_utc_iso8601();
        let mut rows = conn
            .query(
                sql,
                ::libsql::params![
                    source_id,                            // ?1  source_id
                    slug,                                 // ?2  slug
                    input.page_type.clone(),              // ?3  type
                    page_kind_wire,                       // ?4  page_kind
                    input.title.clone(),                  // ?5  title
                    input.compiled_truth.clone(),         // ?6  compiled_truth
                    timeline,                             // ?7  timeline
                    frontmatter_json,                     // ?8  frontmatter (JSON TEXT)
                    input.content_hash.clone(),           // ?9  content_hash
                    now,                                  // ?10 updated_at (server now)
                    input.effective_date.clone(),         // ?11 effective_date
                    effective_date_source_wire,           // ?12 effective_date_source
                    input.import_filename.clone(),        // ?13 import_filename
                    input.chunker_version.map(i64::from), // ?14 chunker_version (COALESCE 1)
                    input.source_path.clone(),            // ?15 source_path
                    input.source_kind.clone(),            // ?16 source_kind
                    input.source_uri.clone(),             // ?17 source_uri
                    input.ingested_via.clone(),           // ?18 ingested_via
                    ingested_at,                          // ?19 ingested_at (server-stamp)
                    input.embedding.clone(),              // ?20 embedding (f32-LE BLOB, G24)
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("put_page upsert failed: {e}")))?;

        let row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("put_page row fetch failed: {e}")))?
            .ok_or_else(|| Error::engine("put_page RETURNING produced no row"))?;
        full_row_to_page(&row)
    }

    async fn delete_page(&self, slug: &str, source_id: Option<&str>) -> Result<()> {
        let conn = self.conn().await?;
        let source_id = source_id.unwrap_or("default");
        // No-op on missing (`source_id`, `slug`) pair (matches PG + InMemory contracts).
        conn.execute(
            "DELETE FROM pages WHERE slug = ?1 AND source_id = ?2",
            ::libsql::params![slug, source_id],
        )
        .await
        .map_err(|e| Error::engine(format!("delete_page failed: {e}")))?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Dynamic SQL builder for 10 filters — extracting helpers is deferred to a later refactor slice.
    async fn list_pages(&self, filters: &PageFilters) -> Result<Vec<Page>> {
        let conn = self.conn().await?;

        // ── Build dynamic SQL ────────────────────────────────────────────
        // Full 30-column projection, same column order as `full_row_to_page`.
        // We alias `pages AS p` so the whitelisted ORDER BY fragments from
        // `page_sort_sql` (which use `p.` prefixes for future JOIN reuse)
        // bind cleanly.
        //
        // tag filter (S6-T5c): when `filters.tag.is_some()`, we INNER JOIN
        // `page_tags AS pt` and pin `pt.tag = ?` in WHERE.  Mirrors the TS
        // PGLite engine's `JOIN tags t ON t.page_id = p.id WHERE t.tag = $N`
        // shape.  The `(page_id, tag)` composite PK on `page_tags` guarantees
        // at most one matching row per page, so the JOIN cannot duplicate
        // results — semantically equivalent to EXISTS without the subquery.
        let mut sql = "SELECT p.id, p.slug, p.type, p.page_kind, p.title, p.compiled_truth, \
                       p.timeline, p.frontmatter, p.content_hash, p.emotional_weight, \
                       p.created_at, p.updated_at, p.deleted_at, p.last_retrieved_at, \
                       p.effective_date, p.effective_date_source, p.import_filename, \
                       p.salience_touched_at, p.salience_score, p.generation, p.embedding, \
                       p.chunker_version, p.source_path, p.source_id, p.source_kind, \
                       p.source_uri, p.ingested_via, p.ingested_at, \
                       p.contextual_retrieval_mode, p.corpus_generation \
                       FROM pages AS p"
            .to_owned();

        if filters.tag.is_some() {
            sql.push_str(" JOIN page_tags AS pt ON pt.page_id = p.id");
        }

        sql.push_str(" WHERE 1=1");

        let mut param_idx: u32 = 1;

        // Filter: page_type
        let page_type_param = if filters.page_type.is_some() {
            let frag = format!(" AND p.type = ?{param_idx}");
            param_idx += 1;
            Some(frag)
        } else {
            None
        };
        if let Some(ref frag) = page_type_param {
            sql.push_str(frag);
        }

        // Filter: slug_prefix (prefix match, no leading-wildcard fuzziness)
        let slug_prefix_param = if filters.slug_prefix.is_some() {
            let frag = format!(" AND p.slug LIKE ?{param_idx} || '%'");
            param_idx += 1;
            Some(frag)
        } else {
            None
        };
        if let Some(ref frag) = slug_prefix_param {
            sql.push_str(frag);
        }

        // Filter: source scope. TS precedence is:
        // non-empty sourceIds > sourceId > no source filter.
        let source_ids_filter = filters.source_ids.as_ref().filter(|ids| !ids.is_empty());
        let source_id_param = if source_ids_filter.is_none() && filters.source_id.is_some() {
            let frag = format!(" AND p.source_id = ?{param_idx}");
            param_idx += 1;
            Some(frag)
        } else {
            None
        };
        if let Some(ref frag) = source_id_param {
            sql.push_str(frag);
        }

        let source_id_in_param = if let Some(ids) = source_ids_filter {
            let mut placeholders: Vec<String> = Vec::with_capacity(ids.len());
            for _ in ids {
                placeholders.push(format!("?{param_idx}"));
                param_idx += 1;
            }
            Some(format!(" AND p.source_id IN ({})", placeholders.join(", ")))
        } else {
            None
        };
        if let Some(ref frag) = source_id_in_param {
            sql.push_str(frag);
        }

        // Filter: updated_after (strictly after — `>` not `>=`)
        let updated_after_param = if filters.updated_after.is_some() {
            let frag = format!(" AND p.updated_at > ?{param_idx}");
            param_idx += 1;
            Some(frag)
        } else {
            None
        };
        if let Some(ref frag) = updated_after_param {
            sql.push_str(frag);
        }

        // Filter: tag (S6-T5c, single-tag exact match via JOIN page_tags)
        // The JOIN was emitted above when `filters.tag.is_some()`; here we
        // add the bound WHERE predicate.  Composite PK (page_id, tag) ensures
        // no row duplication on join.
        let tag_param = if filters.tag.is_some() {
            let frag = format!(" AND pt.tag = ?{param_idx}");
            param_idx += 1;
            Some(frag)
        } else {
            None
        };
        if let Some(ref frag) = tag_param {
            sql.push_str(frag);
        }

        // Filter: include_deleted (default = false → exclude soft-deleted rows)
        if !filters.include_deleted {
            sql.push_str(" AND p.deleted_at IS NULL");
        }

        // ORDER BY — default UpdatedDesc when sort is None.
        //
        // Slice #110-g: append `p.slug ASC` as a deterministic tie-breaker
        // for non-slug sort modes. Without it, rows inserted at the same
        // millisecond (common in tests) produce a non-deterministic order
        // under SQLite, which flakes paginated `offset` assertions. `Slug`
        // already orders by slug so skip the duplicate.
        let sort_mode = filters.sort.unwrap_or_default();
        let sort_sql = page_sort_sql(sort_mode);
        sql.push_str(" ORDER BY ");
        sql.push_str(sort_sql);
        if sort_mode != PageSort::Slug {
            sql.push_str(", p.slug ASC");
        }

        // LIMIT — SQLite requires LIMIT before OFFSET.  When only OFFSET is
        // requested we still must emit a LIMIT clause; the SQLite convention
        // is `LIMIT -1` meaning "no upper bound".
        let limit_param = if filters.limit.is_some() {
            let frag = format!(" LIMIT ?{param_idx}");
            param_idx += 1;
            Some(frag)
        } else if filters.offset.is_some() {
            // Sentinel literal; not a bound parameter.
            Some(" LIMIT -1".to_owned())
        } else {
            None
        };
        if let Some(ref frag) = limit_param {
            sql.push_str(frag);
        }

        // OFFSET — last param slot; no further increment needed.
        let offset_param = if filters.offset.is_some() {
            let frag = format!(" OFFSET ?{param_idx}");
            Some(frag)
        } else {
            None
        };
        if let Some(ref frag) = offset_param {
            sql.push_str(frag);
        }

        // ── Bind parameters positionally ────────────────────────────────
        // libsql `::libsql::params!` macro requires concrete types; we build a Vec of
        // `Value` for positional binding.
        //
        // ORDER CONTRACT: the push order below MUST match the `param_idx`
        // bumps above — page_type → slug_prefix → source scope (sourceIds
        // non-empty wins, else sourceId) → updated_after → tag → limit →
        // offset. Reordering either side without the other will silently misbind.
        let mut param_vals: Vec<::libsql::Value> = Vec::new();

        if let Some(ref pt) = filters.page_type {
            param_vals.push(::libsql::Value::from(pt.clone()));
        }
        if let Some(ref prefix) = filters.slug_prefix {
            param_vals.push(::libsql::Value::from(prefix.clone()));
        }
        if let Some(ids) = filters.source_ids.as_ref().filter(|ids| !ids.is_empty()) {
            for id in ids {
                param_vals.push(::libsql::Value::from(id.clone()));
            }
        } else if let Some(ref sid) = filters.source_id {
            param_vals.push(::libsql::Value::from(sid.clone()));
        }
        if let Some(ref ts) = filters.updated_after {
            param_vals.push(::libsql::Value::from(ts.clone()));
        }
        if let Some(ref tag) = filters.tag {
            param_vals.push(::libsql::Value::from(tag.clone()));
        }
        if let Some(n) = filters.limit {
            let limit_i64 = i64::try_from(n).unwrap_or(i64::MAX);
            param_vals.push(::libsql::Value::from(limit_i64));
        }
        if let Some(n) = filters.offset {
            let offset_i64 = i64::try_from(n).unwrap_or(i64::MAX);
            param_vals.push(::libsql::Value::from(offset_i64));
        }

        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(param_vals))
            .await
            .map_err(|e| Error::engine(format!("list_pages query failed: {e}")))?;

        let mut out = Vec::new();
        loop {
            let next = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("list_pages row fetch failed: {e}")))?;
            match next {
                Some(row) => out.push(full_row_to_page(&row)?),
                None => break,
            }
        }
        Ok(out)
    }

    async fn list_stale_pages(&self) -> Result<Vec<Page>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
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
                ::libsql::params![],
            )
            .await
            .map_err(|e| Error::engine(format!("list_stale_pages query failed: {e}")))?;

        let mut out = Vec::new();
        loop {
            let next = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("list_stale_pages row fetch failed: {e}")))?;
            match next {
                Some(row) => out.push(full_row_to_page(&row)?),
                None => break,
            }
        }
        Ok(out)
    }

    async fn put_page_embedding(
        &self,
        slug: &str,
        source_id: &str,
        embedding: Vec<u8>,
    ) -> Result<()> {
        let conn = self.conn().await?;
        conn.execute(
            "UPDATE pages SET embedding = ? WHERE slug = ? AND source_id = ? AND deleted_at IS NULL",
            ::libsql::params![embedding, slug, source_id],
        )
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
        let conn = self.conn().await?;
        conn.execute(
            "UPDATE pages SET timeline = ? WHERE slug = ? AND source_id = ? AND deleted_at IS NULL",
            ::libsql::params![timeline, slug, source_id],
        )
        .await
        .map_err(|e| Error::engine(format!("set_page_timeline failed: {e}")))?;
        Ok(())
    }

    /// Real hybrid search over the libsql-backed store.
    ///
    /// Overrides the `BrainEngine::search_pages` trait default (which returns
    /// an empty Vec) so the production CLI `query` path — which constructs a
    /// `LibsqlEngine` — actually returns results instead of silently nothing.
    ///
    /// Backend-specific half only: materialize the live (non-deleted),
    /// optionally source-scoped candidate pages with the full 30-column
    /// projection (so `full_row_to_page` recovers `embedding` for the vector
    /// path), then hand the owned `Vec<Page>` to the shared, backend-agnostic
    /// `fuse_and_boost` core (lexical + vector RRF fusion + snippet +
    /// salience/recency boost). InMemory and libsql therefore share one scoring
    /// truth instead of two drifting copies.
    ///
    /// No candidate pre-filtering by keyword happens in SQL: like InMemory, the
    /// lexical match is a case-insensitive substring scan done in `fuse_and_boost`
    /// (over title / compiled_truth / frontmatter with per-field weights), which
    /// SQL `LIKE` cannot reproduce faithfully (weighting, frontmatter JSON, the
    /// vector path). Pulling the live candidate set into memory and fusing there
    /// keeps parity exact. A future FTS5 / sqlite-vec push-down is possible but
    /// out of scope (registered in docs/plans/KNOWN-GAPS.md).
    async fn search_pages(&self, opts: &SearchOpts) -> Result<Vec<SearchResult>> {
        let conn = self.conn().await?;

        // Candidate retrieval: full 30-column projection (same column order as
        // `full_row_to_page`), live rows only, optionally source-scoped.
        // `(?1 IS NULL OR source_id = ?1)` leaves `None` unscoped, mirroring
        // get_page's source filter.
        let source_id_param = opts.source_id.as_deref();
        let mut rows = conn
            .query(
                "SELECT id, slug, type, page_kind, title, compiled_truth, timeline, \
                        frontmatter, content_hash, emotional_weight, created_at, updated_at, \
                        deleted_at, last_retrieved_at, effective_date, effective_date_source, \
                        import_filename, salience_touched_at, salience_score, generation, \
                        embedding, chunker_version, source_path, source_id, source_kind, \
                        source_uri, ingested_via, ingested_at, contextual_retrieval_mode, \
                        corpus_generation \
                 FROM pages \
                 WHERE deleted_at IS NULL \
                   AND (?1 IS NULL OR source_id = ?1)",
                ::libsql::params![source_id_param],
            )
            .await
            .map_err(|e| Error::engine(format!("search_pages candidate query failed: {e}")))?;

        let mut candidates: Vec<Page> = Vec::new();
        loop {
            let next = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("search_pages row fetch failed: {e}")))?;
            match next {
                Some(row) => candidates.push(full_row_to_page(&row)?),
                None => break,
            }
        }

        fuse_and_boost(self, &candidates, opts).await
    }

    async fn search_pages_by_embedding(
        &self,
        query_embedding: &[f32],
        limit: usize,
        source_id: Option<&str>,
    ) -> Result<Vec<Page>> {
        use crate::engine::{cosine_similarity, decode_embedding_le};

        let conn = self.conn().await?;
        let source_id_param = source_id;

        // Fetch all live pages with non-null embeddings, optionally source-scoped.
        let mut rows = conn
            .query(
                "SELECT id, slug, type, page_kind, title, compiled_truth, timeline, \
                        frontmatter, content_hash, emotional_weight, created_at, updated_at, \
                        deleted_at, last_retrieved_at, effective_date, effective_date_source, \
                        import_filename, salience_touched_at, salience_score, generation, \
                        embedding, chunker_version, source_path, source_id, source_kind, \
                        source_uri, ingested_via, ingested_at, contextual_retrieval_mode, \
                        corpus_generation \
                 FROM pages \
                 WHERE embedding IS NOT NULL \
                   AND deleted_at IS NULL \
                   AND (?1 IS NULL OR source_id = ?1)",
                ::libsql::params![source_id_param],
            )
            .await
            .map_err(|e| Error::engine(format!("search_pages_by_embedding query failed: {e}")))?;

        // Decode embeddings and compute cosine similarity.
        let mut scored: Vec<(Page, f64)> = Vec::new();
        loop {
            let next = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("search_pages_by_embedding row fetch failed: {e}")))?;
            match next {
                Some(row) => {
                    let page = full_row_to_page(&row)?;
                    let score = page
                        .embedding
                        .as_deref()
                        .and_then(decode_embedding_le)
                        .map(|emb| cosine_similarity(query_embedding, &emb))
                        .unwrap_or(0.0);
                    scored.push((page, score));
                }
                None => break,
            }
        }

        // Sort by similarity score descending.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Truncate to top-N.
        scored.truncate(limit.min(scored.len()));

        Ok(scored.into_iter().map(|(p, _)| p).collect())
    }

    async fn image_search_daily_spend_cents(&self, client_id: &str) -> Result<i64> {
        let conn = self.conn().await?;
        let today_start = format!("{}T00:00:00Z", &crate::time::current_utc_iso8601()[..10]);
        let mut rows = conn
            .query(
                "SELECT COALESCE(SUM(amount_cents), 0) AS total FROM image_search_spend_log \
                 WHERE client_id = ?1 AND created_at >= ?2",
                ::libsql::params![client_id, today_start],
            )
            .await
            .map_err(|e| Error::engine(format!("image_search_daily_spend_cents query failed: {e}")))?;
        match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("image_search_daily_spend_cents row failed: {e}")))?
        {
            Some(row) => {
                let total: i64 = row
                    .get(0)
                    .map_err(|e| Error::engine(format!("image_search_daily_spend_cents decode failed: {e}")))?;
                Ok(total)
            }
            None => Ok(0),
        }
    }

    async fn record_image_search_spend(
        &self,
        client_id: &str,
        amount_cents: i64,
        provider: &str,
        model: &str,
    ) -> Result<()> {
        let conn = self.conn().await?;
        conn.execute(
            "INSERT INTO image_search_spend_log (client_id, amount_cents, provider, model, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ::libsql::params![
                client_id,
                amount_cents,
                provider,
                model,
                crate::time::current_utc_iso8601()
            ],
        )
        .await
        .map_err(|e| Error::engine(format!("record_image_search_spend insert failed: {e}")))?;
        Ok(())
    }

    async fn resolve_slugs(&self, partial: &str, opts: &ResolveSlugsOpts) -> Result<Vec<String>> {
        let conn = self.conn().await?;
        let (source_clause, mut params) =
            match opts.source_ids.as_ref().filter(|ids| !ids.is_empty()) {
                Some(source_ids) => {
                    let placeholders = (0..source_ids.len())
                        .map(|idx| format!("?{}", idx + 2))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mut params = vec![partial.to_string()];
                    params.extend(source_ids.iter().cloned());
                    (format!(" AND source_id IN ({placeholders})"), params)
                }
                None => match opts.source_id.as_ref() {
                    Some(source_id) => (
                        " AND source_id = ?2".to_string(),
                        vec![partial.to_string(), source_id.clone()],
                    ),
                    None => (String::new(), vec![partial.to_string()]),
                },
            };

        let exact_sql = format!(
            "SELECT slug FROM pages WHERE slug = ?1 AND deleted_at IS NULL{source_clause} ORDER BY slug ASC"
        );
        let mut exact_rows = conn
            .query(&exact_sql, ::libsql::params_from_iter(params.clone()))
            .await
            .map_err(|e| Error::engine(format!("resolve_slugs exact query failed: {e}")))?;
        let exact = collect_slug_rows(&mut exact_rows).await?;
        if !exact.is_empty() {
            return Ok(exact);
        }

        let fuzzy_param_idx = params.len() + 1;
        let fuzzy_sql = format!(
            "SELECT slug FROM pages WHERE deleted_at IS NULL AND slug LIKE ?{fuzzy_param_idx}{source_clause} ORDER BY slug ASC LIMIT 5"
        );
        params.push(format!("%{partial}%"));
        let mut fuzzy_rows = conn
            .query(&fuzzy_sql, ::libsql::params_from_iter(params))
            .await
            .map_err(|e| Error::engine(format!("resolve_slugs fuzzy query failed: {e}")))?;
        collect_slug_rows(&mut fuzzy_rows).await
    }

    async fn upsert_file(&self, spec: &FileSpec) -> Result<UpsertFileResult> {
        let conn = self.conn().await?;
        let source_id = spec.source_id.as_deref().unwrap_or("default");
        let metadata = spec.metadata.clone().unwrap_or_else(|| json!({}));
        let metadata_text = serde_json::to_string(&metadata)
            .map_err(|e| Error::engine(format!("serialize file metadata failed: {e}")))?;
        let page_id = spec.page_id.map(|id| id as i64);

        let existed = conn
            .query(
                "SELECT id FROM files WHERE storage_path = ?1",
                ::libsql::params![spec.storage_path.clone()],
            )
            .await
            .map_err(|e| Error::engine(format!("upsert_file existence query failed: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("upsert_file existence fetch failed: {e}")))?
            .is_some();

        let mut rows = conn
            .query(
                "INSERT INTO files (source_id, page_slug, page_id, filename, storage_path, mime_type, size_bytes, content_hash, metadata) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(storage_path) DO UPDATE SET \
                   source_id = excluded.source_id, \
                   page_slug = excluded.page_slug, \
                   page_id = excluded.page_id, \
                   filename = excluded.filename, \
                   mime_type = excluded.mime_type, \
                   size_bytes = excluded.size_bytes, \
                   content_hash = excluded.content_hash, \
                   metadata = excluded.metadata \
                 RETURNING id",
                ::libsql::params![
                    source_id,
                    spec.page_slug.as_deref(),
                    page_id,
                    spec.filename.clone(),
                    spec.storage_path.clone(),
                    spec.mime_type.as_deref(),
                    spec.size_bytes,
                    spec.content_hash.clone(),
                    metadata_text,
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("upsert_file failed: {e}")))?;
        let row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("upsert_file returning fetch failed: {e}")))?
            .ok_or_else(|| Error::engine("upsert_file returned no row"))?;
        let id: i64 = row
            .get(0)
            .map_err(|e| Error::engine(format!("upsert_file decode id failed: {e}")))?;

        Ok(UpsertFileResult {
            id: id as u64,
            created: !existed,
        })
    }

    async fn get_file(&self, source_id: &str, storage_path: &str) -> Result<Option<FileRow>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, source_id, page_slug, page_id, filename, storage_path, mime_type, size_bytes, content_hash, metadata, created_at \
                 FROM files \
                 WHERE source_id = ?1 AND storage_path = ?2 \
                 LIMIT 1",
                ::libsql::params![source_id, storage_path],
            )
            .await
            .map_err(|e| Error::engine(format!("get_file query failed: {e}")))?;
        rows.next()
            .await
            .map_err(|e| Error::engine(format!("get_file fetch failed: {e}")))?
            .map(|row| libsql_row_to_file(&row))
            .transpose()
    }

    async fn list_files_for_page(&self, page_id: u64) -> Result<Vec<FileRow>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, source_id, page_slug, page_id, filename, storage_path, mime_type, size_bytes, content_hash, metadata, created_at \
                 FROM files \
                 WHERE page_id = ?1 \
                 ORDER BY id ASC",
                ::libsql::params![page_id as i64],
            )
            .await
            .map_err(|e| Error::engine(format!("list_files_for_page query failed: {e}")))?;
        let mut files = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_files_for_page fetch failed: {e}")))?
        {
            files.push(libsql_row_to_file(&row)?);
        }
        Ok(files)
    }

    // ── 1-6-7-5: file listing + ingestion + chunks ──────────────────────

    async fn list_files(&self, slug: Option<&str>) -> Result<Vec<FileListRow>> {
        const FILE_LIST_LIMIT: i64 = 100;
        let conn = self.conn().await?;
        let mut sql = String::from(
            "SELECT id, page_slug, filename, storage_path, mime_type, size_bytes, content_hash, created_at \
             FROM files",
        );
        if slug.is_some() {
            sql.push_str(" WHERE page_slug = ?1");
        }
        sql.push_str(" ORDER BY page_slug, filename LIMIT ?2");
        // SQLite ignores the unreferenced ?1 binding when slug is None.
        let slug_val = slug.unwrap_or("");
        let params = ::libsql::params![slug_val, FILE_LIST_LIMIT];
        let mut rows = conn
            .query(&sql, params)
            .await
            .map_err(|e| Error::engine(format!("list_files query failed: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_files fetch failed: {e}")))?
        {
            let id: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("file row decode id: {e}")))?;
            let page_slug: Option<String> = row
                .get(1)
                .map_err(|e| Error::engine(format!("file row decode page_slug: {e}")))?;
            let filename: String = row
                .get(2)
                .map_err(|e| Error::engine(format!("file row decode filename: {e}")))?;
            let storage_path: String = row
                .get(3)
                .map_err(|e| Error::engine(format!("file row decode storage_path: {e}")))?;
            let mime_type: Option<String> = row
                .get(4)
                .map_err(|e| Error::engine(format!("file row decode mime_type: {e}")))?;
            let size_bytes: Option<i64> = row
                .get(5)
                .map_err(|e| Error::engine(format!("file row decode size_bytes: {e}")))?;
            let content_hash: String = row
                .get(6)
                .map_err(|e| Error::engine(format!("file row decode content_hash: {e}")))?;
            let created_at: String = row
                .get(7)
                .map_err(|e| Error::engine(format!("file row decode created_at: {e}")))?;
            out.push(FileListRow {
                id,
                page_slug,
                filename,
                storage_path,
                mime_type,
                size_bytes,
                content_hash,
                created_at,
            });
        }
        Ok(out)
    }

    async fn get_chunks(&self, slug: &str, source_id: &str) -> Result<Vec<Chunk>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT cc.page_id, cc.chunk_index, cc.chunk_text, cc.chunk_source, cc.model, \
                        cc.token_count, cc.language, cc.symbol_name, cc.symbol_type, cc.start_line, \
                        cc.end_line, cc.parent_symbol_path, cc.doc_comment, cc.symbol_name_qualified, cc.created_at \
                 FROM content_chunks cc \
                 JOIN pages p ON p.id = cc.page_id \
                 WHERE p.slug = ?1 AND p.source_id = ?2 \
                 ORDER BY cc.chunk_index ASC",
                ::libsql::params![slug, source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_chunks query failed: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_chunks fetch failed: {e}")))?
        {
            out.push(libsql_row_to_chunk(&row)?);
        }
        Ok(out)
    }

    async fn log_ingest(&self, input: &IngestLogInput) -> Result<()> {
        let conn = self.conn().await?;
        let pages_updated_json = serde_json::to_string(&input.pages_updated)
            .map_err(|e| Error::engine(format!("log_ingest serialize pages_updated: {e}")))?;
        conn.execute(
            "INSERT INTO ingest_log (source_id, source_type, source_ref, pages_updated, summary) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ::libsql::params![
                input.source_id.clone(),
                input.source_type.clone(),
                input.source_ref.clone(),
                pages_updated_json,
                input.summary.clone(),
            ],
        )
        .await
        .map_err(|e| Error::engine(format!("log_ingest insert failed: {e}")))?;
        Ok(())
    }

    async fn get_ingest_log(&self, limit: u32) -> Result<Vec<IngestLogEntry>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, source_id, source_type, source_ref, pages_updated, summary, created_at \
                 FROM ingest_log ORDER BY created_at DESC LIMIT ?1",
                ::libsql::params![limit as i64],
            )
            .await
            .map_err(|e| Error::engine(format!("get_ingest_log query failed: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_ingest_log fetch failed: {e}")))?
        {
            out.push(libsql_row_to_ingest_log(&row)?);
        }
        Ok(out)
    }

    async fn add_code_edges(
        &self,
        edges: &[crate::import::CodeEdgeInput],
    ) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let conn = self.conn().await?;
        for e in edges {
            // Mirror TS: edge_metadata defaults to {} when absent/null.
            let meta = if e.edge_metadata.is_null() {
                "{}".to_string()
            } else {
                serde_json::to_string(&e.edge_metadata)
                    .unwrap_or_else(|_| "{}".to_string())
            };
            match e.to_chunk_id {
                Some(to_chunk_id) => {
                    conn.execute(
                        "INSERT OR IGNORE INTO code_edges_chunk \
                         (from_chunk_id, to_chunk_id, from_symbol_qualified, to_symbol_qualified, edge_type, edge_metadata, source_id) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        ::libsql::params![
                            e.from_chunk_id,
                            to_chunk_id,
                            e.from_symbol_qualified.clone(),
                            e.to_symbol_qualified.clone(),
                            e.edge_type.clone(),
                            meta,
                            e.source_id.clone(),
                        ],
                    )
                    .await
                    .map_err(|err| Error::engine(format!("add_code_edges (chunk) insert failed: {err}")))?;
                }
                None => {
                    conn.execute(
                        "INSERT OR IGNORE INTO code_edges_symbol \
                         (from_chunk_id, from_symbol_qualified, to_symbol_qualified, edge_type, edge_metadata, source_id) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        ::libsql::params![
                            e.from_chunk_id,
                            e.from_symbol_qualified.clone(),
                            e.to_symbol_qualified.clone(),
                            e.edge_type.clone(),
                            meta,
                            e.source_id.clone(),
                        ],
                    )
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
        let conn = self.conn().await?;
        // code_edges_chunk matches either endpoint; code_edges_symbol is from-only
        // (no to_chunk_id to match against), mirroring TS deleteCodeEdgesForChunks.
        for &cid in chunk_ids {
            conn.execute(
                "DELETE FROM code_edges_chunk WHERE from_chunk_id = ?1 OR to_chunk_id = ?1",
                ::libsql::params![cid],
            )
            .await
            .map_err(|err| Error::engine(format!("delete_code_edges_for_chunks (chunk) failed: {err}")))?;
            conn.execute(
                "DELETE FROM code_edges_symbol WHERE from_chunk_id = ?1",
                ::libsql::params![cid],
            )
            .await
            .map_err(|err| Error::engine(format!("delete_code_edges_for_chunks (symbol) failed: {err}")))?;
        }
        Ok(())
    }

    // ── 1-6-7-10-2: code-graph query methods (Libsql) ────────────────────

    async fn get_callers_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> Result<Vec<crate::import::CodeEdgeResult>> {
        code_edge_symbol_query(self, "to_symbol_qualified", qualified_name, opts).await
    }

    async fn get_callees_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> Result<Vec<crate::import::CodeEdgeResult>> {
        code_edge_symbol_query(self, "from_symbol_qualified", qualified_name, opts).await
    }

    async fn get_edges_by_chunk(
        &self,
        chunk_id: i64,
        opts: &crate::import::CodeEdgeByChunkOpts,
    ) -> Result<Vec<crate::import::CodeEdgeResult>> {
        let conn = self.conn().await?;
        let limit = (opts.limit.unwrap_or(50) as i64).min(200);

        let mut params: Vec<::libsql::Value> = Vec::new();
        params.push(::libsql::Value::from(chunk_id));

        let chunk_filter = match opts.direction {
            crate::import::CodeEdgeDirection::In => " WHERE to_chunk_id = ?1",
            crate::import::CodeEdgeDirection::Out => " WHERE from_chunk_id = ?1",
            // Parenthesize so an optional edge_type filter applies to BOTH
            // endpoints (intent), not just the second via OR/AND precedence.
            crate::import::CodeEdgeDirection::Both => " WHERE (from_chunk_id = ?1 OR to_chunk_id = ?1)",
        };
        let mut sql = format!(
            "SELECT id, from_chunk_id, to_chunk_id, from_symbol_qualified, to_symbol_qualified, \
                    edge_type, edge_metadata, source_id, 1 AS resolved \
               FROM code_edges_chunk{chunk_filter}",
        );

        // Unresolved rows carry only `from_chunk_id`, so they contribute only
        // for 'out' / 'both' directions.
        let sym_filter = match opts.direction {
            crate::import::CodeEdgeDirection::In => None,
            crate::import::CodeEdgeDirection::Out | crate::import::CodeEdgeDirection::Both => {
                Some(" WHERE from_chunk_id = ?1")
            }
        };
        if let Some(sf) = sym_filter {
            sql.push_str(&format!(
                " UNION ALL SELECT id, from_chunk_id, NULL AS to_chunk_id, from_symbol_qualified, \
                        to_symbol_qualified, edge_type, edge_metadata, source_id, 0 AS resolved \
                   FROM code_edges_symbol{sf}",
            ));
        }

        if let Some(et) = &opts.edge_type {
            sql.push_str(" AND edge_type = ?2");
            if sym_filter.is_some() {
                sql.push_str(" AND edge_type = ?2");
            }
            params.push(::libsql::Value::from(et.clone()));
        }

        let limit_ph = params.len() + 1;
        sql.push_str(&format!(" LIMIT ?{limit_ph}"));
        params.push(::libsql::Value::from(limit));

        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(params))
            .await
            .map_err(|e| Error::engine(format!("get_edges_by_chunk query failed: {e}")))?;

        let mut out = Vec::new();
        loop {
            let next = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("get_edges_by_chunk row fetch failed: {e}")))?;
            match next {
                Some(row) => out.push(code_edge_row_to_result(&row)?),
                None => break,
            }
        }
        Ok(out)
    }

    async fn find_code_def(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> Result<Vec<crate::import::CodeDefResult>> {
        let conn = self.conn().await?;
        code_def_query(&conn, symbol, opts).await
    }

    async fn find_code_refs(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> Result<Vec<crate::import::CodeRefResult>> {
        let conn = self.conn().await?;
        code_ref_query(&conn, symbol, opts).await
    }

    async fn disambiguate_symbol(
        &self,
        bare: &str,
        source_id: &str,
    ) -> Result<crate::import::SymbolDisambiguation> {
        let conn = self.conn().await?;
        code_disambiguate_query(&conn, bare, source_id).await
    }

    async fn recursive_walk(
        &self,
        symbol: &str,
        opts: &crate::import::RecursiveWalkOpts,
    ) -> Result<crate::import::RecursiveWalkResult> {
        let conn = self.conn().await?;
        code_recursive_walk_query(self, &conn, symbol, opts).await
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

    async fn get_calibration_curve(
        &self,
        query: &crate::calibration_queries::CalibrationCurveQuery<'_>,
    ) -> Result<Vec<crate::calibration_queries::CalibrationBucket>> {
        crate::calibration_queries::CalibrationQueries::get_calibration_curve(self, query).await
    }

    async fn insert_calibration_profile(
        &self,
        row: &crate::calibration_queries::CalibrationProfileInsert<'_>,
    ) -> Result<i64> {
        crate::calibration_queries::CalibrationQueries::insert_calibration_profile(self, row).await
    }

    // ── undo-wave reversal bridge (1-3-3-2) ──

    async fn revert_wave_resolutions(
        &self,
        wave_version: &str,
        resolved_by: &str,
        dry_run: bool,
    ) -> Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::revert_wave_resolutions(self, wave_version, resolved_by, dry_run).await
    }

    async fn unapply_wave_grade_cache(&self, wave_version: &str, dry_run: bool) -> Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::unapply_wave_grade_cache(self, wave_version, dry_run).await
    }

    async fn delete_calibration_profiles_for_wave(
        &self,
        wave_version: &str,
        dry_run: bool,
    ) -> Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::delete_calibration_profiles_for_wave(self, wave_version, dry_run).await
    }

    async fn purge_nudge_log_for_wave(&self, wave_version: &str, dry_run: bool) -> Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::purge_nudge_log_for_wave(self, wave_version, dry_run).await
    }

    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> Result<Option<DuplicatePage>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, slug \
                 FROM pages \
                 WHERE source_id = ?1 \
                   AND deleted_at IS NULL \
                   AND (content_hash = ?2 OR (?3 IS NOT NULL AND json_extract(frontmatter, '$.id') = ?3)) \
                 ORDER BY id ASC \
                 LIMIT 1",
                ::libsql::params![source_id, opts.content_hash.clone(), opts.frontmatter_id.clone()],
            )
            .await
            .map_err(|e| Error::engine(format!("find_duplicate_page query failed: {e}")))?;

        match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_duplicate_page row fetch failed: {e}")))?
        {
            Some(row) => {
                let id: i64 = row
                    .get(0)
                    .map_err(|e| Error::engine(format!("find_duplicate_page decode id: {e}")))?;
                let slug: String = row
                    .get(1)
                    .map_err(|e| Error::engine(format!("find_duplicate_page decode slug: {e}")))?;
                Ok(Some(DuplicatePage {
                    slug,
                    id: u64::try_from(id).map_err(|e| {
                        Error::engine(format!("find_duplicate_page decode id range: {e}"))
                    })?,
                }))
            }
            None => Ok(None),
        }
    }

    async fn soft_delete_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "UPDATE pages \
                 SET deleted_at = CURRENT_TIMESTAMP \
                 WHERE slug = ?1 \
                   AND deleted_at IS NULL \
                   AND (?2 IS NULL OR source_id = ?2) \
                 RETURNING slug",
                ::libsql::params![slug, source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("soft_delete_page update failed: {e}")))?;

        match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("soft_delete_page row fetch failed: {e}")))?
        {
            Some(row) => {
                let slug: String = row
                    .get(0)
                    .map_err(|e| Error::engine(format!("soft_delete_page decode failed: {e}")))?;
                Ok(Some(slug))
            }
            None => Ok(None),
        }
    }

    async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> Result<bool> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "UPDATE pages \
                 SET deleted_at = NULL \
                 WHERE slug = ?1 \
                   AND deleted_at IS NOT NULL \
                   AND (?2 IS NULL OR source_id = ?2) \
                 RETURNING slug",
                ::libsql::params![slug, source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("restore_page update failed: {e}")))?;

        Ok(rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("restore_page row fetch failed: {e}")))?
            .is_some())
    }

    async fn purge_deleted_pages(&self, older_than_hours: u32) -> Result<PurgeResult> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "DELETE FROM pages \
                 WHERE deleted_at IS NOT NULL \
                   AND deleted_at < strftime('%Y-%m-%d %H:%M:%f', 'now', '-' || ?1 || ' hours') \
                 RETURNING slug",
                ::libsql::params![older_than_hours.to_string()],
            )
            .await
            .map_err(|e| Error::engine(format!("purge_deleted_pages delete failed: {e}")))?;

        let mut slugs = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("purge_deleted_pages row fetch failed: {e}")))?
        {
            slugs.push(
                row.get(0).map_err(|e| {
                    Error::engine(format!("purge_deleted_pages decode failed: {e}"))
                })?,
            );
        }

        let count = slugs.len() as u64;
        Ok(PurgeResult { slugs, count })
    }

    async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> Result<()> {
        // TS semantic: `opts?.sourceId ?? 'default'`. Mirror exactly.
        let sid = source_id.unwrap_or("default");
        let conn = self.conn().await?;

        // Single-statement insert: only proceeds when a *live* page row exists
        // (slug, source_id, deleted_at IS NULL). The INSERT…SELECT pattern
        // returns 0 affected rows for both "page missing" and "(page_id, tag)
        // duplicate", so we disambiguate with a follow-up EXISTS probe.
        //
        // Stronger-than-TS clause: `AND deleted_at IS NULL`. TS allows tagging
        // soft-deleted pages (likely an oversight — those rows are hidden from
        // every read path and will eventually be purged). Rust deliberately
        // surfaces this as PageNotFound. See commit message / S6-T7 plan node 5.
        let affected = conn
            .execute(
                "INSERT INTO page_tags (page_id, tag) \
                 SELECT id, ?2 FROM pages \
                 WHERE slug = ?1 AND source_id = ?3 AND deleted_at IS NULL \
                 ON CONFLICT (page_id, tag) DO NOTHING",
                ::libsql::params![slug, tag, sid],
            )
            .await
            .map_err(|e| Error::engine(format!("add_tag insert failed: {e}")))?;

        if affected > 0 {
            return Ok(());
        }

        // Zero rows affected → either the page is missing/soft-deleted, OR
        // the (page_id, tag) pair already existed (ON CONFLICT DO NOTHING).
        // Probe live page existence to decide between PageNotFound and
        // idempotent success.
        let mut rows = conn
            .query(
                "SELECT 1 FROM pages \
                 WHERE slug = ?1 AND source_id = ?2 AND deleted_at IS NULL \
                 LIMIT 1",
                ::libsql::params![slug, sid],
            )
            .await
            .map_err(|e| Error::engine(format!("add_tag existence probe failed: {e}")))?;

        let page_exists = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("add_tag probe row fetch failed: {e}")))?
            .is_some();

        if page_exists {
            // Page is live; affected==0 must be the DO NOTHING branch ⇒ tag
            // already attached. Idempotent success, exactly like TS where the
            // unique constraint silently absorbs the dup.
            Ok(())
        } else {
            Err(Error::page_not_found(slug, source_id))
        }
    }

    async fn remove_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> Result<()> {
        // TS `removeTag` uses a sub-select; when the page is missing the
        // sub-select returns NULL and the DELETE matches zero rows — silent
        // no-op. Rust preserves that asymmetry vs addTag.
        let sid = source_id.unwrap_or("default");
        let conn = self.conn().await?;
        conn.execute(
            "DELETE FROM page_tags \
             WHERE tag = ?2 \
               AND page_id = ( \
                   SELECT id FROM pages \
                   WHERE slug = ?1 AND source_id = ?3 AND deleted_at IS NULL \
               )",
            ::libsql::params![slug, tag, sid],
        )
        .await
        .map_err(|e| Error::engine(format!("remove_tag delete failed: {e}")))?;
        Ok(())
    }

    async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> Result<Vec<String>> {
        // TS `getTags` returns [] for missing pages (sub-select → NULL →
        // page_id IS NULL never matches). Same shape in Rust.
        let sid = source_id.unwrap_or("default");
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT tag FROM page_tags \
                 WHERE page_id = ( \
                     SELECT id FROM pages \
                     WHERE slug = ?1 AND source_id = ?2 AND deleted_at IS NULL \
                 ) \
                 ORDER BY tag",
                ::libsql::params![slug, sid],
            )
            .await
            .map_err(|e| Error::engine(format!("get_tags query failed: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_tags row fetch failed: {e}")))?
        {
            let tag: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_tags decode failed: {e}")))?;
            out.push(tag);
        }
        Ok(out)
    }
    async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> Result<()> {
        let conn = self.conn().await?;
        let timeline = args.timeline.to_string();

        conn.execute(
            "UPDATE pages \
             SET compiled_truth = ?1, \
                 timeline = ?2, \
                 content_hash = ?3, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE source_id = ?4 \
               AND slug = ?5 \
               AND deleted_at IS NULL",
            ::libsql::params![
                args.compiled_truth.clone(),
                timeline,
                args.content_hash.clone(),
                args.source_id.clone(),
                args.slug.clone(),
            ],
        )
        .await
        .map_err(|e| Error::engine(format!("refresh_page_body failed: {e}")))?;

        Ok(())
    }

    async fn update_page_contextual_retrieval_state(
        &self,
        slug: &str,
        source_id: &str,
        mode: &str,
        corpus_generation: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn().await?;

        conn.execute(
            "UPDATE pages \
             SET contextual_retrieval_mode = ?1, \
                 corpus_generation = ?2, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE source_id = ?3 \
               AND slug = ?4 \
               AND deleted_at IS NULL",
            ::libsql::params![mode, corpus_generation, source_id, slug],
        )
        .await
        .map_err(|e| {
            Error::engine(format!(
                "update_page_contextual_retrieval_state failed: {e}"
            ))
        })?;

        Ok(())
    }

    async fn put_raw_data(
        &self,
        slug: &str,
        source: &str,
        data: &Value,
        source_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn().await?;
        let source_id = source_id.unwrap_or("default");
        let data_text = serde_json::to_string(data)
            .map_err(|e| Error::engine(format!("put_raw_data encode: {e}")))?;

        // Resolve page_id for the (slug, source_id) pair, matching TS putRawData.
        let mut rows = conn
            .query(
                "SELECT id FROM pages WHERE slug = ?1 AND source_id = ?2 AND deleted_at IS NULL",
                ::libsql::params![slug.to_string(), source_id.to_string()],
            )
            .await
            .map_err(|e| Error::engine(format!("put_raw_data page lookup failed: {e}")))?;
        let page_row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("put_raw_data page fetch failed: {e}")))?;
        let Some(page_row) = page_row else {
            return Err(Error::page_not_found(slug, Some(source_id)));
        };
        let page_id: i64 = page_row
            .get(0)
            .map_err(|e| Error::engine(format!("put_raw_data decode page_id: {e}")))?;

        conn.execute(
            "INSERT INTO raw_data (page_id, source, data) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(page_id, source) DO UPDATE SET \
               data = excluded.data, \
               fetched_at = CURRENT_TIMESTAMP",
            ::libsql::params![page_id, source.to_string(), data_text],
        )
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
        let conn = self.conn().await?;
        let source_id = source_id.unwrap_or("default");

        // Resolve page_id first.
        let mut rows = conn
            .query(
                "SELECT id FROM pages WHERE slug = ?1 AND source_id = ?2",
                ::libsql::params![slug.to_string(), source_id.to_string()],
            )
            .await
            .map_err(|e| Error::engine(format!("get_raw_data page lookup failed: {e}")))?;
        let page_row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_raw_data page fetch failed: {e}")))?;
        let Some(page_row) = page_row else {
            return Ok(vec![]); // missing page → empty
        };
        let page_id: i64 = page_row
            .get(0)
            .map_err(|e| Error::engine(format!("get_raw_data decode page_id: {e}")))?;

        let sql = if source.is_some() {
            "SELECT source, data, fetched_at FROM raw_data WHERE page_id = ?1 AND source = ?2"
        } else {
            "SELECT source, data, fetched_at FROM raw_data WHERE page_id = ?1"
        };
        let mut params_vec: Vec<::libsql::Value> = vec![page_id.into()];
        if let Some(s) = source {
            params_vec.push(s.to_string().into());
        }
        let mut rows = conn
            .query(sql, ::libsql::params_from_iter(params_vec))
            .await
            .map_err(|e| Error::engine(format!("get_raw_data query failed: {e}")))?;

        let mut results = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_raw_data fetch failed: {e}")))?
        {
            let src: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_raw_data decode source: {e}")))?;
            let data_text: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("get_raw_data decode data: {e}")))?;
            let fetched_at: String = row
                .get(2)
                .map_err(|e| Error::engine(format!("get_raw_data decode fetched_at: {e}")))?;
            let data: Value = serde_json::from_str(&data_text)
                .map_err(|e| Error::engine(format!("get_raw_data parse data: {e}")))?;
            results.push(RawData {
                source: src,
                data,
                fetched_at,
            });
        }
        Ok(results)
    }

    async fn create_version(&self, slug: &str, source_id: Option<&str>) -> Result<PageVersion> {
        let conn = self.conn().await?;
        let source_id = source_id.unwrap_or("default");

        // Snapshot current page state.
        let mut rows = conn
            .query(
                "SELECT id, compiled_truth, frontmatter \
                 FROM pages \
                 WHERE slug = ?1 AND source_id = ?2 AND deleted_at IS NULL",
                ::libsql::params![slug.to_string(), source_id.to_string()],
            )
            .await
            .map_err(|e| Error::engine(format!("create_version page lookup failed: {e}")))?;
        let page_row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("create_version page fetch failed: {e}")))?
            .ok_or_else(|| Error::page_not_found(slug, Some(source_id)))?;
        let page_id: i64 = page_row
            .get(0)
            .map_err(|e| Error::engine(format!("create_version decode page_id: {e}")))?;
        let compiled_truth: String = page_row
            .get(1)
            .map_err(|e| Error::engine(format!("create_version decode compiled_truth: {e}")))?;
        let frontmatter_text: String = page_row
            .get(2)
            .map_err(|e| Error::engine(format!("create_version decode frontmatter: {e}")))?;

        let mut rows = conn
            .query(
                "INSERT INTO page_versions (page_id, compiled_truth, frontmatter) \
                 VALUES (?1, ?2, ?3) RETURNING id, page_id, compiled_truth, frontmatter, snapshot_at",
                ::libsql::params![page_id, compiled_truth, frontmatter_text],
            )
            .await
            .map_err(|e| Error::engine(format!("create_version insert failed: {e}")))?;
        let row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("create_version return fetch failed: {e}")))?
            .ok_or_else(|| Error::engine("create_version RETURNING produced no row"))?;
        let id: i64 = row
            .get(0)
            .map_err(|e| Error::engine(format!("create_version decode id: {e}")))?;
        let returned_page_id: i64 = row
            .get(1)
            .map_err(|e| Error::engine(format!("create_version decode page_id: {e}")))?;
        let returned_truth: String = row
            .get(2)
            .map_err(|e| Error::engine(format!("create_version decode truth: {e}")))?;
        let returned_fm_text: String = row
            .get(3)
            .map_err(|e| Error::engine(format!("create_version decode frontmatter: {e}")))?;
        let snapshot_at: String = row
            .get(4)
            .map_err(|e| Error::engine(format!("create_version decode snapshot_at: {e}")))?;
        let frontmatter: Value = serde_json::from_str(&returned_fm_text)
            .map_err(|e| Error::engine(format!("create_version parse frontmatter: {e}")))?;

        Ok(PageVersion {
            id: id as u64,
            page_id: returned_page_id as u64,
            compiled_truth: returned_truth,
            frontmatter,
            snapshot_at,
        })
    }

    async fn get_versions(&self, slug: &str, source_id: Option<&str>) -> Result<Vec<PageVersion>> {
        let conn = self.conn().await?;
        let source_id = source_id.unwrap_or("default");

        // Resolve page_id.
        let mut rows = conn
            .query(
                "SELECT id FROM pages WHERE slug = ?1 AND source_id = ?2",
                ::libsql::params![slug.to_string(), source_id.to_string()],
            )
            .await
            .map_err(|e| Error::engine(format!("get_versions page lookup failed: {e}")))?;
        let page_row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_versions page fetch failed: {e}")))?;
        let Some(page_row) = page_row else {
            return Ok(vec![]);
        };
        let page_id: i64 = page_row
            .get(0)
            .map_err(|e| Error::engine(format!("get_versions decode page_id: {e}")))?;

        let mut rows = conn
            .query(
                "SELECT id, page_id, compiled_truth, frontmatter, snapshot_at \
                 FROM page_versions \
                 WHERE page_id = ?1 \
                 ORDER BY snapshot_at DESC, id DESC",
                ::libsql::params![page_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_versions query failed: {e}")))?;

        let mut results = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_versions fetch failed: {e}")))?
        {
            let id: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_versions decode id: {e}")))?;
            let v_page_id: i64 = row
                .get(1)
                .map_err(|e| Error::engine(format!("get_versions decode page_id: {e}")))?;
            let compiled_truth: String = row
                .get(2)
                .map_err(|e| Error::engine(format!("get_versions decode truth: {e}")))?;
            let fm_text: String = row
                .get(3)
                .map_err(|e| Error::engine(format!("get_versions decode frontmatter: {e}")))?;
            let snapshot_at: String = row
                .get(4)
                .map_err(|e| Error::engine(format!("get_versions decode snapshot_at: {e}")))?;
            let frontmatter: Value = serde_json::from_str(&fm_text)
                .map_err(|e| Error::engine(format!("get_versions parse frontmatter: {e}")))?;
            results.push(PageVersion {
                id: id as u64,
                page_id: v_page_id as u64,
                compiled_truth,
                frontmatter,
                snapshot_at,
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
        let conn = self.conn().await?;
        let source_id = source_id.unwrap_or("default");

        // Read the version snapshot.
        let mut rows = conn
            .query(
                "SELECT compiled_truth, frontmatter FROM page_versions WHERE id = ?1",
                ::libsql::params![version_id as i64],
            )
            .await
            .map_err(|e| Error::engine(format!("revert_to_version lookup failed: {e}")))?;
        let ver_row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("revert_to_version fetch failed: {e}")))?;
        let Some(ver_row) = ver_row else {
            return Err(Error::engine(format!("version {version_id} not found")));
        };
        let compiled_truth: String = ver_row
            .get(0)
            .map_err(|e| Error::engine(format!("revert_to_version decode truth: {e}")))?;
        let frontmatter_text: String = ver_row
            .get(1)
            .map_err(|e| Error::engine(format!("revert_to_version decode frontmatter: {e}")))?;

        // Apply to the live page. generation trigger handles cache invalidation.
        let affected = conn
            .execute(
                "UPDATE pages SET compiled_truth = ?1, frontmatter = ?2, updated_at = ?3 \
                 WHERE slug = ?4 AND source_id = ?5 AND deleted_at IS NULL",
                ::libsql::params![
                    compiled_truth,
                    frontmatter_text,
                    current_utc_iso8601(),
                    slug.to_string(),
                    source_id.to_string(),
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("revert_to_version update failed: {e}")))?;

        if affected == 0 {
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
        let conn = self.conn().await?;
        let source_id = source_id.unwrap_or("default");

        // Conflict check: new_slug must not already exist in this source.
        let mut rows = conn
            .query(
                "SELECT 1 FROM pages WHERE slug = ?1 AND source_id = ?2",
                ::libsql::params![new_slug.to_string(), source_id.to_string()],
            )
            .await
            .map_err(|e| Error::engine(format!("update_slug conflict check failed: {e}")))?;
        if rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("update_slug conflict fetch failed: {e}")))?
            .is_some()
        {
            return Err(Error::engine(format!(
                "slug '{new_slug}' already exists in source '{source_id}'"
            )));
        }

        let affected = conn
            .execute(
                "UPDATE pages SET slug = ?1, updated_at = ?2 \
                 WHERE slug = ?3 AND source_id = ?4 AND deleted_at IS NULL",
                ::libsql::params![
                    new_slug.to_string(),
                    current_utc_iso8601(),
                    old_slug.to_string(),
                    source_id.to_string(),
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("update_slug failed: {e}")))?;

        if affected == 0 {
            return Err(Error::page_not_found(old_slug, Some(source_id)));
        }
        Ok(())
    }

    /// Explicit no-op — libsql page rows use integer `page_id` foreign keys
    /// so there are no embedded slug strings to rewrite. Returns `Ok(())`.
    async fn rewrite_links(&self, _old_slug: &str, _new_slug: &str) -> Result<()> {
        Ok(())
    }

    async fn find_orphan_pages(&self) -> Result<Vec<OrphanPage>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT p.slug, COALESCE(p.title, p.slug) AS title, \
                        json_extract(p.frontmatter, '$.domain') AS domain \
                 FROM pages p \
                 WHERE p.deleted_at IS NULL \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM links l \
                     JOIN pages src ON src.id = l.from_page_id \
                     WHERE l.to_page_id = p.id AND src.deleted_at IS NULL \
                   ) \
                 ORDER BY p.slug",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("find_orphan_pages query failed: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_orphan_pages row fetch failed: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("find_orphan_pages decode slug: {e}")))?;
            let title: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("find_orphan_pages decode title: {e}")))?;
            let domain: Option<String> = row
                .get(2)
                .map_err(|e| Error::engine(format!("find_orphan_pages decode domain: {e}")))?;
            out.push(OrphanPage {
                slug,
                title,
                domain,
            });
        }

        Ok(out)
    }

    /// `find_anomalies` — Libsql (SQLite dialect) rewrite of the TS Postgres
    /// `findAnomalies` SQL. Key dialect differences:
    /// * `generate_series` → recursive `date()` CTE for zero-filled days.
    /// * `date_trunc('day', …)` → `substr(updated_at, 1, 10)` (RFC3339 `Z` form).
    /// * `array_agg(DISTINCT slug)` → `json_group_array(DISTINCT slug)`.
    ///
    /// `?1`/`?2` are `YYYY-MM-DD` date-only bounds for the recursive CTE;
    /// `?3`/`?4` are full RFC3339 `…Z` bounds for the `updated_at` range filter
    /// (matching the stored format, so lexicographic compare is chronological).
    ///
    /// NOTE: SQLite caps recursive CTE depth (~1000); this matches the default
    /// `lookback_days=30` comfortably but a pathological `--lookback-days`
    /// beyond the limit would error — same spirit as the PG `generate_series`.
    async fn find_anomalies(
        &self,
        opts: crate::anomaly::AnomaliesOpts,
    ) -> crate::Result<Vec<crate::anomaly::AnomalyResult>> {
        use crate::anomaly::{
            compute_anomalies_from_buckets, resolve_anomaly_windows, CohortDayRow,
            CohortKind, CohortTodayRow,
        };

        let (baseline_from, baseline_to, today_from, today_to, _window_days, sigma, limit) =
            resolve_anomaly_windows(&opts)?;
        let baseline_start_day = _window_days.first().map(String::as_str).unwrap_or("");
        let since_day = baseline_to.get(..10).unwrap_or("");

        let conn = self.conn().await?;

        // ---- tag baseline (densified) ----
        let tag_baseline_sql = "
            WITH RECURSIVE days(d) AS (
                SELECT date(?1)
                UNION ALL
                SELECT date(d, '+1 day')
                FROM days
                WHERE d < date(?2)
            ),
            cohort_keys AS (
                SELECT DISTINCT t.tag AS cohort_value
                FROM page_tags t
                JOIN pages p ON p.id = t.page_id
                WHERE p.updated_at >= ?3 AND p.updated_at < ?4 AND p.deleted_at IS NULL
            ),
            touched AS (
                SELECT t.tag AS cohort_value,
                       substr(p.updated_at, 1, 10) AS day,
                       COUNT(DISTINCT p.id) AS cnt
                FROM page_tags t
                JOIN pages p ON p.id = t.page_id
                WHERE p.updated_at >= ?3 AND p.updated_at < ?4 AND p.deleted_at IS NULL
                GROUP BY 1, 2
            )
            SELECT cd.cohort_value AS cohort_value, d.d AS day, COALESCE(t.cnt, 0) AS count
            FROM cohort_keys cd
            CROSS JOIN days d
            LEFT JOIN touched t ON t.cohort_value = cd.cohort_value AND t.day = d.d";
        let mut tag_baseline_rows = conn
            .query(
                tag_baseline_sql,
                ::libsql::params![
                    baseline_start_day,
                    since_day,
                    baseline_from.clone(),
                    baseline_to.clone()
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("find_anomalies tag baseline query failed: {e}")))?;
        let mut baseline: Vec<CohortDayRow> = Vec::new();
        while let Some(row) = tag_baseline_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_anomalies tag baseline fetch failed: {e}")))?
        {
            let cohort_value: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("decode tag baseline cohort_value: {e}")))?;
            let day: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("decode tag baseline day: {e}")))?;
            let count: i64 = row
                .get(2)
                .map_err(|e| Error::engine(format!("decode tag baseline count: {e}")))?;
            baseline.push(CohortDayRow {
                cohort_kind: CohortKind::Tag,
                cohort_value,
                day,
                count,
            });
        }

        // ---- type baseline (densified) ----
        let type_baseline_sql = "
            WITH RECURSIVE days(d) AS (
                SELECT date(?1)
                UNION ALL
                SELECT date(d, '+1 day')
                FROM days
                WHERE d < date(?2)
            ),
            cohort_keys AS (
                SELECT DISTINCT p.type AS cohort_value
                FROM pages p
                WHERE p.updated_at >= ?3 AND p.updated_at < ?4 AND p.deleted_at IS NULL
            ),
            touched AS (
                SELECT p.type AS cohort_value,
                       substr(p.updated_at, 1, 10) AS day,
                       COUNT(DISTINCT p.id) AS cnt
                FROM pages p
                WHERE p.updated_at >= ?3 AND p.updated_at < ?4 AND p.deleted_at IS NULL
                GROUP BY 1, 2
            )
            SELECT cd.cohort_value AS cohort_value, d.d AS day, COALESCE(t.cnt, 0) AS count
            FROM cohort_keys cd
            CROSS JOIN days d
            LEFT JOIN touched t ON t.cohort_value = cd.cohort_value AND t.day = d.d";
        let mut type_baseline_rows = conn
            .query(
                type_baseline_sql,
                ::libsql::params![
                    baseline_start_day,
                    since_day,
                    baseline_from.clone(),
                    baseline_to.clone()
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("find_anomalies type baseline query failed: {e}")))?;
        while let Some(row) = type_baseline_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_anomalies type baseline fetch failed: {e}")))?
        {
            let cohort_value: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("decode type baseline cohort_value: {e}")))?;
            let day: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("decode type baseline day: {e}")))?;
            let count: i64 = row
                .get(2)
                .map_err(|e| Error::engine(format!("decode type baseline count: {e}")))?;
            baseline.push(CohortDayRow {
                cohort_kind: CohortKind::Type,
                cohort_value,
                day,
                count,
            });
        }

        // ---- today (tag + type) ----
        let parse_slugs = |raw: Option<String>| -> Vec<String> {
            match raw {
                Some(ref js) => serde_json::from_str::<Vec<String>>(js).unwrap_or_default(),
                None => Vec::new(),
            }
        };

        let mut today: Vec<CohortTodayRow> = Vec::new();

        let tag_today_sql = "
            SELECT t.tag AS cohort_value,
                   COUNT(DISTINCT p.id) AS count,
                   json_group_array(DISTINCT p.slug) AS slugs
            FROM page_tags t
            JOIN pages p ON p.id = t.page_id
            WHERE p.updated_at >= ?1 AND p.updated_at < ?2 AND p.deleted_at IS NULL
            GROUP BY 1";
        let mut tag_today_rows = conn
            .query(
                tag_today_sql,
                ::libsql::params![today_from.clone(), today_to.clone()],
            )
            .await
            .map_err(|e| Error::engine(format!("find_anomalies tag today query failed: {e}")))?;
        while let Some(row) = tag_today_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_anomalies tag today fetch failed: {e}")))?
        {
            let cohort_value: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("decode tag today cohort_value: {e}")))?;
            let count: i64 = row
                .get(1)
                .map_err(|e| Error::engine(format!("decode tag today count: {e}")))?;
            let slugs: Option<String> = row
                .get(2)
                .map_err(|e| Error::engine(format!("decode tag today slugs: {e}")))?;
            today.push(CohortTodayRow {
                cohort_kind: CohortKind::Tag,
                cohort_value,
                count,
                page_slugs: parse_slugs(slugs),
            });
        }

        let type_today_sql = "
            SELECT p.type AS cohort_value,
                   COUNT(DISTINCT p.id) AS count,
                   json_group_array(DISTINCT p.slug) AS slugs
            FROM pages p
            WHERE p.updated_at >= ?1 AND p.updated_at < ?2 AND p.deleted_at IS NULL
            GROUP BY 1";
        let mut type_today_rows = conn
            .query(
                type_today_sql,
                ::libsql::params![today_from, today_to],
            )
            .await
            .map_err(|e| Error::engine(format!("find_anomalies type today query failed: {e}")))?;
        while let Some(row) = type_today_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_anomalies type today fetch failed: {e}")))?
        {
            let cohort_value: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("decode type today cohort_value: {e}")))?;
            let count: i64 = row
                .get(1)
                .map_err(|e| Error::engine(format!("decode type today count: {e}")))?;
            let slugs: Option<String> = row
                .get(2)
                .map_err(|e| Error::engine(format!("decode type today slugs: {e}")))?;
            today.push(CohortTodayRow {
                cohort_kind: CohortKind::Type,
                cohort_value,
                count,
                page_slugs: parse_slugs(slugs),
            });
        }

        Ok(compute_anomalies_from_buckets(&baseline, &today, sigma, limit))
    }

    async fn get_all_slugs(
        &self,
        source_id: Option<&str>,
    ) -> Result<std::collections::HashSet<String>> {
        // §11.1 R1 TS parity: does NOT filter `deleted_at`.
        let conn = self.conn().await?;
        let mut rows = match source_id {
            Some(sid) => conn
                .query(
                    "SELECT slug FROM pages WHERE source_id = ?1",
                    ::libsql::params![sid],
                )
                .await
                .map_err(|e| Error::engine(format!("get_all_slugs query failed: {e}")))?,
            None => conn
                .query("SELECT slug FROM pages", ())
                .await
                .map_err(|e| Error::engine(format!("get_all_slugs query failed: {e}")))?,
        };

        let mut out = std::collections::HashSet::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_all_slugs row fetch failed: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_all_slugs decode failed: {e}")))?;
            out.insert(slug);
        }
        Ok(out)
    }

    async fn list_all_page_refs(&self) -> Result<Vec<PageRef>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT slug, source_id FROM pages \
                 WHERE deleted_at IS NULL \
                 ORDER BY source_id ASC, slug ASC",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("list_all_page_refs query failed: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_all_page_refs row fetch failed: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("list_all_page_refs decode slug: {e}")))?;
            let source_id: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("list_all_page_refs decode source_id: {e}")))?;
            out.push(PageRef { slug, source_id });
        }
        Ok(out)
    }

    async fn get_page_timestamps(
        &self,
        slugs: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        let mut out = std::collections::HashMap::new();
        if slugs.is_empty() {
            return Ok(out);
        }
        let conn = self.conn().await?;
        let placeholders = (1..=slugs.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT slug, COALESCE(updated_at, created_at) AS ts \
             FROM pages \
             WHERE slug IN ({placeholders})"
        );
        let params: Vec<::libsql::Value> = slugs
            .iter()
            .map(|s| ::libsql::Value::from(s.clone()))
            .collect();
        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(params))
            .await
            .map_err(|e| Error::engine(format!("get_page_timestamps query failed: {e}")))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_page_timestamps row fetch failed: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_page_timestamps decode slug: {e}")))?;
            let ts: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("get_page_timestamps decode ts: {e}")))?;
            out.insert(slug, ts);
        }
        Ok(out)
    }

    async fn get_effective_dates(
        &self,
        refs: &[PageRef],
    ) -> Result<std::collections::HashMap<String, String>> {
        let mut out = std::collections::HashMap::new();
        if refs.is_empty() {
            return Ok(out);
        }
        let conn = self.conn().await?;
        let mut pairs = Vec::with_capacity(refs.len());
        let mut params: Vec<::libsql::Value> = Vec::with_capacity(refs.len() * 2);
        for (i, r) in refs.iter().enumerate() {
            let p1 = i * 2 + 1;
            let p2 = i * 2 + 2;
            pairs.push(format!("(?{p1}, ?{p2})"));
            params.push(::libsql::Value::from(r.slug.clone()));
            params.push(::libsql::Value::from(r.source_id.clone()));
        }
        let sql = format!(
            "SELECT slug, source_id, COALESCE(effective_date, updated_at, created_at) AS ts \
             FROM pages \
             WHERE (slug, source_id) IN ({}) AND deleted_at IS NULL",
            pairs.join(", ")
        );
        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(params))
            .await
            .map_err(|e| Error::engine(format!("get_effective_dates query failed: {e}")))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_effective_dates row fetch failed: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_effective_dates decode slug: {e}")))?;
            let source_id: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("get_effective_dates decode source_id: {e}")))?;
            let ts: String = row
                .get(2)
                .map_err(|e| Error::engine(format!("get_effective_dates decode ts: {e}")))?;
            out.insert(format!("{source_id}::{slug}"), ts);
        }
        Ok(out)
    }

    async fn get_salience_scores(
        &self,
        refs: &[PageRef],
    ) -> Result<std::collections::HashMap<String, f64>> {
        // Slice 6c-takes-salience: full TS formula
        //   score = COALESCE(emotional_weight, 0) * 5
        //         + ln(1 + COUNT(DISTINCT t.id) WHERE t.active = 1)
        // mirroring src/core/pglite-engine.ts §2596-2617.
        //
        // libsql 0.9 ships **without** SQLite's math-functions extension and
        // the async `Connection` API does not expose `create_scalar_function`,
        // so we cannot call `ln()` inside SQL. We split the calculation:
        //   1. SQL returns `weighted_emotion` (= COALESCE(ew,0) * 5) and
        //      `active_take_count` (= COUNT(DISTINCT t.id WHERE t.active = 1)).
        //   2. Rust performs `score = weighted_emotion + (1.0 + count).ln()`.
        // This keeps the JOIN/GROUP BY shape close to the PG implementation
        // (which uses `ln()` natively) while staying portable on libsql.
        //
        // Other SQLite-specific notes:
        // - `t.active = 1` keeps PG/libsql parity: SQLite stores BOOLEAN as
        //   INTEGER 0/1 and our 0005 migration declares the column with
        //   default 1. `= TRUE` would also work (SQLite parses TRUE as 1).
        // - `LEFT JOIN` ensures pages without any active takes still appear
        //   in the result set with COUNT = 0 → ln(1+0) = 0.
        // - We keep the manual `IN ((?,?), …)` expansion so the query stays
        //   close to the 6a shape and survives the IN-list helper refactor
        //   slated for slice 7. `GROUP BY p.id, p.slug, p.source_id,
        //   p.emotional_weight` is required because SQLite (unlike PG with
        //   functional-dependency tracking) needs every non-aggregated
        //   projected column listed.
        let mut out = std::collections::HashMap::new();
        if refs.is_empty() {
            return Ok(out);
        }
        let conn = self.conn().await?;
        let mut pairs = Vec::with_capacity(refs.len());
        let mut params: Vec<::libsql::Value> = Vec::with_capacity(refs.len() * 2);
        for (i, r) in refs.iter().enumerate() {
            let p1 = i * 2 + 1;
            let p2 = i * 2 + 2;
            pairs.push(format!("(?{p1}, ?{p2})"));
            params.push(::libsql::Value::from(r.slug.clone()));
            params.push(::libsql::Value::from(r.source_id.clone()));
        }
        let sql = format!(
            "SELECT p.slug, p.source_id, \
                    COALESCE(p.emotional_weight, 0.0) * 5.0 AS weighted_emotion, \
                    COUNT(DISTINCT t.id) AS active_take_count \
             FROM pages p \
             LEFT JOIN takes t ON t.page_id = p.id AND t.active = 1 \
             WHERE (p.slug, p.source_id) IN ({}) AND p.deleted_at IS NULL \
             GROUP BY p.id, p.slug, p.source_id, p.emotional_weight",
            pairs.join(", ")
        );
        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(params))
            .await
            .map_err(|e| Error::engine(format!("get_salience_scores query failed: {e}")))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_salience_scores row fetch failed: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_salience_scores decode slug: {e}")))?;
            let source_id: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("get_salience_scores decode source_id: {e}")))?;
            let weighted_emotion: f64 = row.get(2).map_err(|e| {
                Error::engine(format!("get_salience_scores decode weighted_emotion: {e}"))
            })?;
            let active_take_count: i64 = row.get(3).map_err(|e| {
                Error::engine(format!("get_salience_scores decode active_take_count: {e}"))
            })?;
            // ln(1 + N) computed in Rust because libsql lacks `ln()`.
            // `as f64` cast is safe: take counts in practice fit well within f64's 52-bit mantissa.
            #[allow(clippy::cast_precision_loss)]
            let score = weighted_emotion + (1.0 + active_take_count as f64).ln();
            out.insert(format!("{source_id}::{slug}"), score);
        }
        Ok(out)
    }

    async fn touch_salience(&self, slug: &str, source_id: &str) -> Result<bool> {
        let conn = self.conn().await?;
        let sql = "UPDATE pages SET salience_touched_at = datetime('now') \
                   WHERE slug = ?1 AND source_id = ?2 AND deleted_at IS NULL";
        let rows_affected = conn
            .execute(sql, ::libsql::params![slug, source_id])
            .await
            .map_err(|e| Error::engine(format!("touch_salience execute failed: {e}")))?;
        Ok(rows_affected > 0)
    }

    async fn get_recent_salience(
        &self,
        days: u32,
        limit: u32,
        slug_prefix: Option<&str>,
    ) -> Result<Vec<crate::types::SalienceResult>> {
        let now = chrono::Utc::now();
        let boundary = now - chrono::Duration::days(days as i64);
        let boundary_str = boundary.to_rfc3339();
        let limit = limit.min(100);

        let conn = self.conn().await?;
        let mut params: Vec<::libsql::Value> = Vec::new();
        params.push(::libsql::Value::from(boundary_str.clone()));

        let mut prefix_condition = String::new();
        if let Some(pfx) = slug_prefix {
            let escaped = pfx.replace('%', "\\%").replace('_', "\\_") + "%";
            params.push(::libsql::Value::from(escaped));
            prefix_condition = format!(
                "AND p.slug LIKE ?{} ESCAPE '\\'",
                params.len()
            );
        }

        params.push(::libsql::Value::from(limit as i64));

        let sql = format!(
            "SELECT p.slug, p.source_id, p.title, p.type, p.updated_at, \
                    COALESCE(p.emotional_weight, 0.0) AS emotional_weight, \
                    COUNT(DISTINCT t.id) AS take_count, \
                    COALESCE(AVG(t.weight), 0.0) AS take_avg_weight \
             FROM pages p \
             LEFT JOIN takes t ON t.page_id = p.id AND t.active = 1 \
             WHERE p.deleted_at IS NULL \
               AND CASE WHEN p.salience_touched_at > p.updated_at \
                        THEN p.salience_touched_at \
                        ELSE p.updated_at END >= ?1 \
               {prefix_condition} \
             GROUP BY p.id, p.slug, p.source_id, p.title, p.type, p.updated_at, p.emotional_weight \
             ORDER BY p.updated_at DESC \
             LIMIT ?{limit_idx}",
            prefix_condition = prefix_condition,
            limit_idx = params.len(),
        );

        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(params))
            .await
            .map_err(|e| Error::engine(format!("get_recent_salience query failed: {e}")))?;

        let mut raw: Vec<(String, String, String, String, String, f64, u32, f64)> = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_recent_salience row fetch failed: {e}")))?
        {
            let slug: String = row.get(0).map_err(|e| Error::engine(format!("decode slug: {e}")))?;
            let source_id: String = row.get(1).map_err(|e| Error::engine(format!("decode source_id: {e}")))?;
            let title: String = row.get(2).map_err(|e| Error::engine(format!("decode title: {e}")))?;
            let page_type: String = row.get(3).map_err(|e| Error::engine(format!("decode type: {e}")))?;
            let updated_at: String = row.get(4).map_err(|e| Error::engine(format!("decode updated_at: {e}")))?;
            let emotional_weight: f64 = row.get(5).map_err(|e| Error::engine(format!("decode ew: {e}")))?;
            let take_count_i64: i64 = row.get(6).map_err(|e| Error::engine(format!("decode take_count: {e}")))?;
            #[allow(clippy::cast_sign_loss)]
            let take_count = take_count_i64 as u32;
            let take_avg_weight: f64 = row.get(7).map_err(|e| Error::engine(format!("decode take_avg: {e}")))?;
            raw.push((slug, source_id, title, page_type, updated_at, emotional_weight, take_count, take_avg_weight));
        }

        // Compute score in Rust (libsql lacks ln() SQL function).
        let mut results: Vec<crate::types::SalienceResult> = raw
            .into_iter()
            .map(|(slug, source_id, title, pt, updated_at, ew, take_count, take_avg_weight)| {
                let days_old = chrono::DateTime::parse_from_rfc3339(&updated_at)
                    .ok()
                    .map(|dt| {
                        let dur = now.signed_duration_since(dt.with_timezone(&chrono::Utc));
                        dur.num_milliseconds() as f64 / (86400.0 * 1000.0)
                    })
                    .unwrap_or(0.0);
                let recency_decay = 1.0 / (1.0 + days_old.max(0.0));
                #[allow(clippy::cast_precision_loss)]
                let score = ew * 5.0 + (1.0 + take_count as f64).ln() + recency_decay;

                crate::types::SalienceResult {
                    slug,
                    source_id,
                    title,
                    page_type: pt,
                    updated_at,
                    emotional_weight: ew,
                    take_count,
                    take_avg_weight,
                    score,
                }
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit as usize);
        Ok(results)
    }

    // --- Phase 7A: Takes ---

    async fn get_takes_for_page(
        &self,
        page_id: u64,
        takes_holders_allow_list: Option<Vec<String>>,
    ) -> Result<Vec<Take>> {
        let mut sql = String::from(
            "SELECT id, page_id, row_num, claim, kind, holder, weight, \
                    since_date, until_date, source, superseded_by, active, \
                    resolved_at, resolved_quality, resolved_outcome, \
                    resolved_evidence, resolved_value, resolved_unit, \
                    resolved_by, created_at, updated_at \
             FROM takes WHERE page_id = ?1",
        );
        let mut values: Vec<::libsql::Value> = vec![::libsql::Value::from(page_id as i64)];
        append_takes_holder_filter(&mut sql, &mut values, &takes_holders_allow_list);
        sql.push_str(" ORDER BY row_num ASC");
        let conn = self.conn().await?;
        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(values))
            .await
            .map_err(|e| Error::engine(format!("get_takes_for_page query: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_takes_for_page row: {e}")))?
        {
            out.push(Take {
                id: row.get::<i64>(0).map_err(|e| Error::engine(format!("take id: {e}")))? as u64,
                page_id: row.get::<i64>(1).map_err(|e| Error::engine(format!("take page_id: {e}")))? as u64,
                row_num: row.get::<i64>(2).map_err(|e| Error::engine(format!("take row_num: {e}")))? as i32,
                claim: row.get(3).unwrap_or_default(),
                kind: row.get(4).unwrap_or_default(),
                holder: row.get(5).unwrap_or_default(),
                weight: row.get::<f64>(6).unwrap_or(0.5),
                since_date: row.get(7).unwrap_or(None),
                until_date: row.get(8).unwrap_or(None),
                source: row.get(9).unwrap_or(None),
                superseded_by: row.get::<Option<i64>>(10).unwrap_or(None).map(|v| v as i32),
                active: row.get::<i64>(11).unwrap_or(1) != 0,
                resolved_at: row.get(12).unwrap_or(None),
                resolved_quality: row.get(13).unwrap_or(None),
                resolved_outcome: row.get::<Option<i64>>(14).unwrap_or(None).map(|v| v != 0),
                resolved_evidence: row.get(15).unwrap_or(None),
                resolved_value: row.get(16).unwrap_or(None),
                resolved_unit: row.get(17).unwrap_or(None),
                resolved_by: row.get(18).unwrap_or(None),
                created_at: row.get(19).unwrap_or_default(),
                updated_at: row.get(20).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    async fn list_takes(&self, opts: &TakesListOpts) -> Result<Vec<Take>> {
        let mut sql = String::from(
            "SELECT id, page_id, row_num, claim, kind, holder, weight, \
                    since_date, until_date, source, superseded_by, active, \
                    resolved_at, resolved_quality, resolved_outcome, \
                    resolved_evidence, resolved_value, resolved_unit, \
                    resolved_by, created_at, updated_at \
             FROM takes WHERE 1=1",
        );
        let mut values: Vec<::libsql::Value> = Vec::new();
        if let Some(pid) = opts.page_id {
            sql.push_str(&format!(" AND page_id = ?{}", values.len() + 1));
            values.push(::libsql::Value::from(pid as i64));
        }
        if let Some(h) = &opts.holder {
            sql.push_str(&format!(" AND holder = ?{}", values.len() + 1));
            values.push(::libsql::Value::from(h.clone()));
        }
        if let Some(k) = &opts.kind {
            sql.push_str(&format!(" AND kind = ?{}", values.len() + 1));
            values.push(::libsql::Value::from(k.clone()));
        }
        if let Some(a) = opts.active {
            sql.push_str(&format!(" AND active = ?{}", values.len() + 1));
            values.push(::libsql::Value::from(a as i64));
        }
        if let Some(r) = opts.resolved {
            if r {
                sql.push_str(" AND resolved_at IS NOT NULL");
            } else {
                sql.push_str(" AND resolved_at IS NULL");
            }
        }
        append_takes_holder_filter(&mut sql, &mut values, &opts.takes_holders_allow_list);
        sql.push_str(" ORDER BY weight DESC");
        let limit = opts.limit.unwrap_or(100) as i64;
        let offset = opts.offset.unwrap_or(0) as i64;
        sql.push_str(&format!(" LIMIT ?{} OFFSET ?{}", values.len() + 1, values.len() + 2));
        values.push(::libsql::Value::from(limit));
        values.push(::libsql::Value::from(offset));
        let conn = self.conn().await?;
        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(values))
            .await
            .map_err(|e| Error::engine(format!("list_takes query: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_takes row: {e}")))?
        {
            out.push(take_from_row(&row)?);
        }
        Ok(out)
    }

    async fn search_takes(&self, query: &str, opts: &SearchTakesOpts) -> Result<Vec<TakeHit>> {
        let mut sql = String::from(
            "SELECT t.id, t.page_id, p.slug, t.row_num, t.claim, t.kind, t.holder, t.weight \
             FROM takes t JOIN pages p ON p.id = t.page_id \
             WHERE t.active AND LOWER(t.claim) LIKE '%' || LOWER(?1) || '%'",
        );
        let mut values: Vec<::libsql::Value> = vec![::libsql::Value::from(query.to_string())];
        append_takes_holder_filter(&mut sql, &mut values, &opts.takes_holders_allow_list);
        sql.push_str(" ORDER BY t.weight DESC");
        let limit = opts.limit.unwrap_or(30) as i64;
        sql.push_str(&format!(" LIMIT ?{}", values.len() + 1));
        values.push(::libsql::Value::from(limit));
        let conn = self.conn().await?;
        let mut rows = conn
            .query(&sql, ::libsql::params_from_iter(values))
            .await
            .map_err(|e| Error::engine(format!("search_takes query: {e}")))?;
        let q = query.to_lowercase();
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("search_takes row: {e}")))?
        {
            let claim: String = row.get(4).unwrap_or_default();
            let weight: f64 = row.get(7).unwrap_or(0.5);
            let score = if q.is_empty() {
                0.0
            } else {
                claim.to_lowercase().matches(&q).count() as f64 * (1.0 + weight)
            };
            out.push(TakeHit {
                take_id: row.get::<i64>(0).map_err(|e| Error::engine(format!("take id: {e}")))? as u64,
                page_id: row.get::<i64>(1).map_err(|e| Error::engine(format!("take page_id: {e}")))? as u64,
                page_slug: row.get(2).unwrap_or_default(),
                row_num: row.get::<i64>(3).map_err(|e| Error::engine(format!("take row_num: {e}")))? as i32,
                claim,
                kind: row.get(5).unwrap_or_default(),
                holder: row.get(6).unwrap_or_default(),
                weight,
                score,
            });
        }
        Ok(out)
    }

    async fn add_takes_batch(
        &self,
        page_id: u64,
        takes: &[TakeInput],
    ) -> Result<UpsertTakesResult> {
        if takes.is_empty() {
            return Ok(UpsertTakesResult { upserted: 0, weight_clamped: 0 });
        }
        let conn = self.conn().await?;
        let now = current_utc_iso8601();
        let mut upserted = 0usize;
        let mut weight_clamped = 0usize;

        for input in takes {
            let weight = input.weight.clamp(0.0, 1.0);
            if (weight - input.weight).abs() > f64::EPSILON {
                weight_clamped += 1;
            }
            conn.execute(
                "INSERT INTO takes (page_id, row_num, claim, kind, holder, weight, \
                        since_date, until_date, source, superseded_by, active, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                ::libsql::params![
                    page_id as i64,
                    input.row_num.unwrap_or(0) as i64,
                    input.claim.clone(),
                    input.kind.clone(),
                    input.holder.clone(),
                    weight,
                    input.since_date.clone(),
                    input.until_date.clone(),
                    input.source.clone(),
                    input.superseded_by.map(|v| v as i64),
                    input.active.unwrap_or(true) as i64,
                    now.clone(),
                ],
            )
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
        let conn = self.conn().await?;
        let now = current_utc_iso8601();
        // Existence check first, matching canonical TS ordering: TAKE_ROW_NOT_FOUND
        // is thrown before deriveResolutionTuple validates the resolution.
        let mut rows = conn
            .query(
                "SELECT 1 FROM takes WHERE page_id = ?1 AND row_num = ?2",
                ::libsql::params![page_id as i64, row_num as i64],
            )
            .await
            .map_err(|e| Error::engine(format!("resolve_take existence check: {e}")))?;
        if rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("resolve_take existence check: {e}")))?
            .is_none()
        {
            return Err(crate::error::StructuredError::new(
                "Not Found",
                "not_found",
                format!("no take found for page_id={page_id} row_num={row_num}"),
            ));
        }
        // Canonical (resolved_quality, resolved_outcome) derivation — parity
        // with TS `deriveResolutionTuple`; errors on invalid/contradictory input.
        let (resolved_quality, resolved_outcome) = resolution.derive_quality_outcome()?;
        conn
            .execute(
                "UPDATE takes SET \
                        resolved_at = ?1, resolved_quality = ?2, resolved_outcome = ?3, \
                        resolved_evidence = ?4, resolved_value = ?5, resolved_unit = ?6, \
                        resolved_by = ?7, updated_at = ?8 \
                 WHERE page_id = ?9 AND row_num = ?10",
                ::libsql::params![
                    now.clone(),
                    resolved_quality.clone(),
                    resolved_outcome.map(|b| b as i64),
                    resolution.evidence.clone(),
                    resolution.value,
                    resolution.unit.clone(),
                    resolution.by.clone(),
                    now,
                    page_id as i64,
                    row_num as i64,
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("resolve_take: {e}")))?;
        Ok(())
    }

    // ── Links (Phase 7B) ──────────────────────────────────────────────────

    async fn add_links_batch(&self, links: &[LinkBatchInput]) -> Result<usize> {
        if links.is_empty() {
            return Ok(0);
        }
        let conn = self.conn().await?;
        let mut inserted = 0usize;

        for input in links {
            let link_type = input.link_type.as_deref().unwrap_or("");
            let context = input.context.as_deref().unwrap_or("");
            let link_source = input.link_source.as_deref().unwrap_or("markdown");
            let from_source_id = input.from_source_id.as_deref().unwrap_or("default");
            let to_source_id = input.to_source_id.as_deref().unwrap_or("default");
            let origin_source_id = input.origin_source_id.as_deref().unwrap_or("default");

            let affected = conn
                .execute(
                    "INSERT OR IGNORE INTO links \
                            (from_page_id, to_page_id, link_type, context, link_source, \
                             origin_page_id, origin_field) \
                     SELECT f.id, t.id, ?3, ?4, ?5, o.id, ?6 \
                     FROM pages f \
                     JOIN pages t ON t.slug = ?2 AND t.source_id = ?8 \
                     LEFT JOIN pages o ON o.slug = ?7 AND o.source_id = ?9 \
                     WHERE f.slug = ?1 AND f.source_id = ?10",
                    ::libsql::params![
                        input.from_slug.as_str(),
                        input.to_slug.as_str(),
                        link_type,
                        context,
                        link_source,
                        input.origin_field.as_deref(),
                        input.origin_slug.as_deref(),
                        to_source_id,
                        origin_source_id,
                        from_source_id,
                    ],
                )
                .await
                .map_err(|e| Error::engine(format!("add_links_batch INSERT: {e}")))?;
            inserted += affected as usize;
        }
        Ok(inserted)
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
        let conn = self.conn().await?;
        let from_sid = from_source_id.unwrap_or("default");
        let to_sid = to_source_id.unwrap_or("default");

        // Build DELETE with dynamic WHERE clauses.
        // SQLite doesn't support DELETE ... JOIN, so we use a subquery.
        let mut sql = String::from(
            "DELETE FROM links WHERE rowid IN (\
                    SELECT l.rowid FROM links l \
                    JOIN pages f ON f.id = l.from_page_id \
                    JOIN pages t ON t.id = l.to_page_id \
                    WHERE f.slug = ?1 AND f.source_id = ?3 \
                      AND t.slug = ?2 AND t.source_id = ?4",
        );
        let mut param_idx = 5usize;

        if link_type.is_some() {
            sql.push_str(&format!(" AND l.link_type = ?{param_idx}"));
            param_idx += 1;
        }
        if link_source.is_some() {
            sql.push_str(&format!(" AND l.link_source = ?{param_idx}"));
        }
        sql.push(')');

        let mut params: Vec<::libsql::Value> = vec![
            ::libsql::Value::from(from),
            ::libsql::Value::from(to),
            ::libsql::Value::from(from_sid),
            ::libsql::Value::from(to_sid),
        ];
        if let Some(lt) = link_type {
            params.push(::libsql::Value::from(lt));
        }
        if let Some(ls) = link_source {
            params.push(::libsql::Value::from(ls));
        }

        conn.execute(&sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("remove_link DELETE: {e}")))?;

        Ok(())
    }

    async fn get_links(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> Result<Vec<Link>> {
        let conn = self.conn().await?;
        let sid = source_id.unwrap_or("default");

        let mut rows = conn
            .query(
                "SELECT f.slug, t.slug, l.link_type, l.context, l.link_source, \
                        o.slug, l.origin_field \
                 FROM links l \
                 JOIN pages f ON f.id = l.from_page_id \
                 JOIN pages t ON t.id = l.to_page_id \
                 LEFT JOIN pages o ON o.id = l.origin_page_id \
                 WHERE f.slug = ?1 AND f.source_id = ?2 \
                   AND f.deleted_at IS NULL AND t.deleted_at IS NULL \
                 ORDER BY l.link_type, t.slug",
                ::libsql::params![slug, sid],
            )
            .await
            .map_err(|e| Error::engine(format!("get_links query: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await
            .map_err(|e| Error::engine(format!("get_links row: {e}")))?
        {
            out.push(Link {
                from_slug: row.get(0).map_err(|e| Error::engine(format!("get_links decode from_slug: {e}")))?,
                to_slug: row.get(1).map_err(|e| Error::engine(format!("get_links decode to_slug: {e}")))?,
                link_type: row.get(2).map_err(|e| Error::engine(format!("get_links decode link_type: {e}")))?,
                context: row.get(3).map_err(|e| Error::engine(format!("get_links decode context: {e}")))?,
                link_source: row.get(4).map_err(|e| Error::engine(format!("get_links decode link_source: {e}")))?,
                origin_slug: row.get(5).map_err(|e| Error::engine(format!("get_links decode origin_slug: {e}")))?,
                origin_field: row.get(6).map_err(|e| Error::engine(format!("get_links decode origin_field: {e}")))?,
            });
        }
        Ok(out)
    }

    async fn get_backlinks(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> Result<Vec<Link>> {
        let conn = self.conn().await?;
        let sid = source_id.unwrap_or("default");

        let mut rows = conn
            .query(
                "SELECT f.slug, t.slug, l.link_type, l.context, l.link_source, \
                        o.slug, l.origin_field \
                 FROM links l \
                 JOIN pages f ON f.id = l.from_page_id \
                 JOIN pages t ON t.id = l.to_page_id \
                 LEFT JOIN pages o ON o.id = l.origin_page_id \
                 WHERE t.slug = ?1 AND t.source_id = ?2 \
                   AND f.deleted_at IS NULL AND t.deleted_at IS NULL \
                 ORDER BY l.link_type, f.slug",
                ::libsql::params![slug, sid],
            )
            .await
            .map_err(|e| Error::engine(format!("get_backlinks query: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await
            .map_err(|e| Error::engine(format!("get_backlinks row: {e}")))?
        {
            out.push(Link {
                from_slug: row.get(0).map_err(|e| Error::engine(format!("get_backlinks decode from_slug: {e}")))?,
                to_slug: row.get(1).map_err(|e| Error::engine(format!("get_backlinks decode to_slug: {e}")))?,
                link_type: row.get(2).map_err(|e| Error::engine(format!("get_backlinks decode link_type: {e}")))?,
                context: row.get(3).map_err(|e| Error::engine(format!("get_backlinks decode context: {e}")))?,
                link_source: row.get(4).map_err(|e| Error::engine(format!("get_backlinks decode link_source: {e}")))?,
                origin_slug: row.get(5).map_err(|e| Error::engine(format!("get_backlinks decode origin_slug: {e}")))?,
                origin_field: row.get(6).map_err(|e| Error::engine(format!("get_backlinks decode origin_field: {e}")))?,
            });
        }
        Ok(out)
    }

    async fn get_backlink_counts(
        &self,
        slugs: &[String],
    ) -> Result<std::collections::HashMap<String, u64>> {
        if slugs.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let conn = self.conn().await?;

        // Build IN clause placeholders
        let placeholders: Vec<String> = (0..slugs.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT t.slug, COUNT(*) \
             FROM links l \
             JOIN pages t ON t.id = l.to_page_id \
             JOIN pages f ON f.id = l.from_page_id \
             WHERE t.slug IN ({}) \
               AND f.deleted_at IS NULL AND t.deleted_at IS NULL \
             GROUP BY t.slug",
            placeholders.join(",")
        );

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(
                slugs.iter().map(|s| ::libsql::Value::from(s.as_str())).collect()
            ))
            .await
            .map_err(|e| Error::engine(format!("get_backlink_counts query: {e}")))?;

        let mut counts: std::collections::HashMap<String, u64> =
            slugs.iter().map(|s| (s.clone(), 0u64)).collect();

        while let Some(row) = rows.next().await
            .map_err(|e| Error::engine(format!("get_backlink_counts row: {e}")))?
        {
            let slug: String = row.get(0)
                .map_err(|e| Error::engine(format!("get_backlink_counts decode slug: {e}")))?;
            let count: i64 = row.get(1)
                .map_err(|e| Error::engine(format!("get_backlink_counts decode count: {e}")))?;
            counts.insert(slug, count as u64);
        }
        Ok(counts)
    }

    async fn get_adjacency_boosts(
        &self,
        page_ids: &[u64],
    ) -> crate::Result<std::collections::HashMap<u64, AdjacencyRow>> {
        if page_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let conn = self.conn().await?;

        let value_placeholders: Vec<String> = (0..page_ids.len()).map(|i| format!("(?{})", i + 1)).collect();
        let sql = format!(
            "WITH input_ids(id) AS (VALUES {}) \
             ,targets AS ( \
               SELECT id, COALESCE(source_id, 'default') AS source_id \
               FROM pages \
               WHERE id IN (SELECT id FROM input_ids) \
                 AND deleted_at IS NULL \
             ) \
             SELECT \
               l.to_page_id, \
               COUNT(DISTINCT l.from_page_id) AS hits, \
               COUNT(DISTINCT \
                 CASE WHEN COALESCE(p.source_id, 'default') <> t.source_id \
                      THEN COALESCE(p.source_id, 'default') END \
               ) AS cross_source_hits \
             FROM links l \
             JOIN pages p ON p.id = l.from_page_id AND p.deleted_at IS NULL \
             JOIN targets t ON t.id = l.to_page_id \
             WHERE l.from_page_id IN (SELECT id FROM input_ids) \
               AND l.to_page_id IN (SELECT id FROM input_ids) \
             GROUP BY l.to_page_id \
             HAVING COUNT(DISTINCT l.from_page_id) >= 1",
            value_placeholders.join(",")
        );

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(
                page_ids.iter().map(|&id| ::libsql::Value::from(id as i64)).collect()
            ))
            .await
            .map_err(|e| Error::engine(format!("get_adjacency_boosts query: {e}")))?;

        let mut result: std::collections::HashMap<u64, AdjacencyRow> = std::collections::HashMap::new();

        while let Some(row) = rows.next().await
            .map_err(|e| Error::engine(format!("get_adjacency_boosts row: {e}")))?
        {
            let to_page_id: i64 = row.get(0)
                .map_err(|e| Error::engine(format!("get_adjacency_boosts decode to_page_id: {e}")))?;
            let hits: i64 = row.get(1)
                .map_err(|e| Error::engine(format!("get_adjacency_boosts decode hits: {e}")))?;
            let cross_source_hits: i64 = row.get(2)
                .map_err(|e| Error::engine(format!("get_adjacency_boosts decode cross_source_hits: {e}")))?;

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

    // ─── Facts (Phase 7B) ──────────────────────────────────────────────────

    async fn insert_fact(
        &self,
        source_id: &str,
        entity_slug: &str,
        input: &NewFact,
    ) -> Result<FactInsertStatus> {
        let now = current_utc_iso8601();
        let conn = self.conn().await?;
        let tx = conn
            .transaction()
            .await
            .map_err(|e| Error::engine(format!("insert_fact begin tx: {e}")))?;

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
        let notability = input
            .notability
            .as_deref()
            .unwrap_or("medium");

        // Step 1: Check for exact duplicate
        let mut dup_rows = tx
            .query(
                "SELECT id FROM facts \
                 WHERE source_id = ?1 AND entity_slug = ?2 AND fact = ?3 \
                   AND kind = ?4 AND expired_at IS NULL AND superseded_by IS NULL \
                 LIMIT 1",
                ::libsql::params![source_id, entity_slug, input.fact.as_str(), kind.as_str()],
            )
            .await
            .map_err(|e| Error::engine(format!("insert_fact dup check: {e}")))?;
        if (dup_rows.next().await)
            .map_err(|e| Error::engine(format!("insert_fact dup row: {e}")))?
            .is_some()
        {
            return Ok(FactInsertStatus::Duplicate);
        }

        // Step 2: Find supersede target (only if confidence > 0.9)
        let supersede_threshold = 0.9;
        let supersede_target_id: Option<i64> =
            if input.confidence.unwrap_or(1.0) > supersede_threshold {
                let mut rows = tx
                    .query(
                        "SELECT id FROM facts \
                         WHERE source_id = ?1 AND entity_slug = ?2 AND kind = ?3 \
                           AND expired_at IS NULL AND superseded_by IS NULL \
                         LIMIT 1",
                        ::libsql::params![source_id, entity_slug, kind.as_str()],
                    )
                    .await
                    .map_err(|e| Error::engine(format!("insert_fact supersede check: {e}")))?;
                if let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| Error::engine(format!("insert_fact supersede row: {e}")))?
                {
                    Some(
                        row.get::<i64>(0)
                            .map_err(|e| Error::engine(format!("insert_fact supersede id: {e}")))?,
                    )
                } else {
                    None
                }
            } else {
                None
            };

        let valid_from = input
            .valid_from
            .clone()
            .unwrap_or_else(|| now.clone());

        // Step 3: INSERT new fact
        tx.execute(
            "INSERT INTO facts \
                  (source_id, entity_slug, fact, kind, visibility, notability, \
                   context, valid_from, valid_until, source, source_session, \
                   confidence, claim_metric, claim_value, claim_unit, claim_period, event_type, \
                   row_num, source_markdown_slug) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            ::libsql::params![
                source_id,
                entity_slug,
                input.fact.as_str(),
                kind.as_str(),
                visibility.as_str(),
                notability,
                input.context.as_deref().unwrap_or(""),
                valid_from.as_str(),
                input.valid_until.as_deref().unwrap_or(""),
                input.source.as_str(),
                input.source_session.as_deref().unwrap_or(""),
                input.confidence.unwrap_or(1.0),
                input.claim_metric.clone(),
                input.claim_value,
                input.claim_unit.clone(),
                input.claim_period.clone(),
                input.event_type.clone(),
                input.row_num,
                input.source_markdown_slug.clone(),
            ],
        )
        .await
        .map_err(|e| Error::engine(format!("insert_fact INSERT: {e}")))?;

        // Get new row ID
        let mut id_rows = tx
            .query("SELECT last_insert_rowid()", ())
            .await
            .map_err(|e| Error::engine(format!("insert_fact last_rowid: {e}")))?;
        let new_id: i64 = if let Some(row) = id_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("insert_fact last_rowid row: {e}")))?
        {
            row.get::<i64>(0)
                .map_err(|e| Error::engine(format!("insert_fact last_rowid val: {e}")))?
        } else {
            0
        };

        // Step 4: UPDATE supersede target
        if let Some(old_id) = supersede_target_id {
            tx.execute(
                "UPDATE facts SET superseded_by = ?1 WHERE id = ?2",
                ::libsql::params![new_id, old_id],
            )
            .await
            .map_err(|e| Error::engine(format!("insert_fact supersede UPDATE: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| Error::engine(format!("insert_fact commit: {e}")))?;

        if supersede_target_id.is_some() {
            Ok(FactInsertStatus::Superseded)
        } else {
            Ok(FactInsertStatus::Inserted)
        }
    }

    async fn delete_facts_for_page(&self, slug: &str, source_id: &str) -> Result<i64> {
        self.delete_facts_for_page_impl(slug, source_id).await
    }

    async fn count_legacy_fact_rows(&self) -> Result<i64> {
        self.count_legacy_fact_rows_impl().await
    }

    async fn list_facts_by_entity(
        &self,
        source_id: &str,
        entity_slug: &str,
        opts: &FactListOpts,
    ) -> Result<Vec<FactRow>> {
        let conn = self.conn().await?;

        let mut sql = String::from(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from, valid_until, expired_at, \
                    superseded_by, consolidated_at, consolidated_into, source, \
                    source_session, confidence, created_at, \
                    row_num, source_markdown_slug \
             FROM facts \
             WHERE source_id = ? AND entity_slug = ?",
        );

        if opts.active_only.unwrap_or(false) {
            sql.push_str(" AND expired_at IS NULL AND superseded_by IS NULL");
        }
        if let Some(ref kinds) = opts.kinds {
            if !kinds.is_empty() {
                sql.push_str(" AND kind IN (");
                for (i, _) in kinds.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                }
                sql.push(')');
            }
        }
        if let Some(ref vs) = opts.visibility {
            if !vs.is_empty() {
                sql.push_str(" AND visibility IN (");
                for (i, _) in vs.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                }
                sql.push(')');
            }
        }

        sql.push_str(" ORDER BY created_at DESC");

        if let Some(_limit) = opts.limit {
            sql.push_str(" LIMIT ?");
        }
        if let Some(_offset) = opts.offset {
            sql.push_str(" OFFSET ?");
        }

        let mut params: Vec<::libsql::Value> = vec![
            ::libsql::Value::from(source_id),
            ::libsql::Value::from(entity_slug),
        ];
        if let Some(ref kinds) = opts.kinds {
            for k in kinds {
                params.push(::libsql::Value::from(k.to_string()));
            }
        }
        if let Some(ref vs) = opts.visibility {
            for v in vs {
                params.push(::libsql::Value::from(v.to_string()));
            }
        }
        if let Some(ref limit) = opts.limit {
            params.push(::libsql::Value::from(*limit));
        }
        if let Some(ref offset) = opts.offset {
            params.push(::libsql::Value::from(*offset));
        }

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("list_facts_by_entity: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_facts_by_entity row: {e}")))?
        {
            out.push(row_to_fact(&row)?);
        }
        Ok(out)
    }

    async fn find_trajectory(
        &self,
        opts: &crate::types::TrajectoryOpts,
    ) -> Result<Vec<crate::types::TrajectoryPoint>> {
        let conn = self.conn().await?;

        let mut sql = String::from(
            "SELECT id, valid_from, claim_metric, claim_value, claim_unit, claim_period, \
                    event_type, fact, source_session, source_markdown_slug, embedding \
             FROM facts \
             WHERE ",
        );

        let mut params: Vec<::libsql::Value> = Vec::new();
        match &opts.source_ids {
            Some(ids) if !ids.is_empty() => {
                sql.push_str("source_id IN (");
                for (i, id) in ids.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                    params.push(::libsql::Value::from(id.clone()));
                }
                sql.push_str(")");
            }
            _ => {
                let sid = opts
                    .source_id
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                sql.push_str("source_id = ?");
                params.push(::libsql::Value::from(sid));
            }
        }

        sql.push_str(" AND entity_slug = ? AND expired_at IS NULL");
        params.push(::libsql::Value::from(opts.entity_slug.clone()));

        if opts.remote {
            sql.push_str(" AND visibility = 'world'");
        }
        if let Some(ref metric) = opts.metric {
            sql.push_str(" AND claim_metric = ?");
            params.push(::libsql::Value::from(metric.clone()));
        }
        match opts.kind {
            crate::types::TrajectoryKind::Metric => sql.push_str(" AND claim_metric IS NOT NULL"),
            crate::types::TrajectoryKind::Event => sql.push_str(" AND event_type IS NOT NULL"),
            crate::types::TrajectoryKind::All => {}
        }
        if let Some(ref since) = opts.since {
            sql.push_str(" AND valid_from >= ?");
            params.push(::libsql::Value::from(since.clone()));
        }
        if let Some(ref until) = opts.until {
            sql.push_str(" AND valid_from <= ?");
            params.push(::libsql::Value::from(until.clone()));
        }

        sql.push_str(" ORDER BY valid_from ASC, id ASC");

        let limit = (opts.limit.unwrap_or(100) as i64).clamp(1, 500);
        sql.push_str(" LIMIT ?");
        params.push(::libsql::Value::from(limit));

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("find_trajectory: {e}")))?;

        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_trajectory row: {e}")))?
        {
            let valid_from: Option<String> = row.get(1).ok().flatten();
            let embedding =
                crate::trajectory_stats::parse_embedding_text(row.get::<Option<String>>(10).ok().flatten());
            out.push(crate::types::TrajectoryPoint {
                fact_id: row.get::<i64>(0).map_err(|e| Error::engine(format!("ft id: {e}")))?,
                valid_from: valid_from.map(|s| crate::trajectory_stats::iso_date_prefix(&s)),
                metric: row.get(2).ok().flatten(),
                value: row.get(3).ok().flatten(),
                unit: row.get(4).ok().flatten(),
                period: row.get(5).ok().flatten(),
                event_type: row.get(6).ok().flatten(),
                text: row.get::<String>(7).unwrap_or_default(),
                source_session: row.get(8).ok().flatten(),
                source_markdown_slug: row.get(9).ok().flatten(),
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
        let conn = self.conn().await?;
        let mut sql = String::from(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from, valid_until, expired_at, \
                    superseded_by, consolidated_at, consolidated_into, source, \
                    source_session, confidence, created_at \
             FROM facts \
             WHERE source_id = ? AND created_at >= ?",
        );
        let mut params: Vec<::libsql::Value> =
            vec![::libsql::Value::from(source_id), ::libsql::Value::from(since_iso)];
        Self::append_fact_list_filters(&mut sql, &mut params, opts);

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("list_facts_since: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_facts_since row: {e}")))?
        {
            out.push(row_to_fact(&row)?);
        }
        Ok(out)
    }

    async fn list_facts_by_session(
        &self,
        source_id: &str,
        session_id: &str,
        opts: &FactListOpts,
    ) -> Result<Vec<FactRow>> {
        let conn = self.conn().await?;
        let mut sql = String::from(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from, valid_until, expired_at, \
                    superseded_by, consolidated_at, consolidated_into, source, \
                    source_session, confidence, created_at \
             FROM facts \
             WHERE source_id = ? AND source_session = ?",
        );
        let mut params: Vec<::libsql::Value> =
            vec![::libsql::Value::from(source_id), ::libsql::Value::from(session_id)];
        Self::append_fact_list_filters(&mut sql, &mut params, opts);

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("list_facts_by_session: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_facts_by_session row: {e}")))?
        {
            out.push(row_to_fact(&row)?);
        }
        Ok(out)
    }

    async fn list_supersessions(
        &self,
        source_id: &str,
        opts: &crate::types::SupersessionOpts,
    ) -> Result<Vec<FactRow>> {
        let conn = self.conn().await?;
        let mut sql = String::from(
            "SELECT id, source_id, entity_slug, fact, kind, visibility, \
                    notability, context, valid_from, valid_until, expired_at, \
                    superseded_by, consolidated_at, consolidated_into, source, \
                    source_session, confidence, created_at \
             FROM facts \
             WHERE source_id = ? AND expired_at IS NOT NULL AND superseded_by IS NOT NULL",
        );
        let mut params: Vec<::libsql::Value> = vec![::libsql::Value::from(source_id)];
        if let Some(ref since) = opts.since {
            sql.push_str(" AND expired_at >= ?");
            params.push(::libsql::Value::from(since.clone()));
        }
        sql.push_str(" ORDER BY expired_at DESC, id DESC");
        if let Some(ref limit) = opts.limit {
            sql.push_str(" LIMIT ?");
            params.push(::libsql::Value::from(*limit));
        }

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("list_supersessions: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_supersessions row: {e}")))?
        {
            out.push(row_to_fact(&row)?);
        }
        Ok(out)
    }

    async fn count_unconsolidated_facts(&self, source_id: &str) -> Result<i64> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM facts \
                 WHERE source_id = ?1 AND consolidated_at IS NULL AND expired_at IS NULL",
                ::libsql::params![source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("count_unconsolidated_facts: {e}")))?;
        let row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("count_unconsolidated_facts row: {e}")))?
            .ok_or_else(|| Error::engine("count_unconsolidated_facts: no row"))?;
        let count: i64 = row
            .get(0)
            .map_err(|e| Error::engine(format!("count_unconsolidated_facts get: {e}")))?;
        Ok(count)
    }

    async fn get_facts_health(&self, source_id: &str) -> Result<FactsHealth> {
        let conn = self.conn().await?;
        let now = current_utc_iso8601();
        let today_prefix = &now[..10];

        let total_active: i64 = conn
            .query(
                "SELECT COUNT(*) FROM facts \
                 WHERE source_id = ?1 AND expired_at IS NULL AND superseded_by IS NULL",
                ::libsql::params![source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_facts_health active: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_facts_health active row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);

        let total_today: i64 = conn
            .query(
                "SELECT COUNT(*) FROM facts \
                 WHERE source_id = ?1 AND created_at >= ?2",
                ::libsql::params![source_id, today_prefix],
            )
            .await
            .map_err(|e| Error::engine(format!("get_facts_health today: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_facts_health today row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);

        let total_week: i64 = conn
            .query(
                "SELECT COUNT(*) FROM facts \
                 WHERE source_id = ?1 AND datetime(created_at) >= datetime('now', '-7 days')",
                ::libsql::params![source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_facts_health week: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_facts_health week row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);

        let total_expired: i64 = conn
            .query(
                "SELECT COUNT(*) FROM facts \
                 WHERE source_id = ?1 AND expired_at IS NOT NULL",
                ::libsql::params![source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_facts_health expired: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_facts_health expired row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);

        let total_consolidated: i64 = conn
            .query(
                "SELECT COUNT(*) FROM facts \
                 WHERE source_id = ?1 AND consolidated_at IS NOT NULL",
                ::libsql::params![source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_facts_health consolidated: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_facts_health consolidated row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);

        // Top entities by fact count
        let mut top_rows = conn
            .query(
                "SELECT entity_slug, COUNT(*) AS cnt \
                 FROM facts \
                 WHERE source_id = ?1 AND entity_slug IS NOT NULL \
                 GROUP BY entity_slug \
                 ORDER BY cnt DESC \
                 LIMIT 10",
                ::libsql::params![source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_facts_health top: {e}")))?;

        let mut top_entities = Vec::new();
        while let Some(row) = top_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_facts_health top row: {e}")))?
        {
            let slug: String = row
                .get(0)
                .map_err(|e| Error::engine(format!("get_facts_health top slug: {e}")))?;
            let count: i64 = row
                .get(1)
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
        let conn = self.conn().await?;
        let now = current_utc_iso8601();

        let affected = conn
            .execute(
                "UPDATE facts SET expired_at = ?1 \
                 WHERE id = ?2 AND source_id = ?3 AND expired_at IS NULL",
                ::libsql::params![now.as_str(), fact_id, source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("expire_fact: {e}")))?;

        Ok(affected > 0)
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

        let conn = self.conn().await?;
        let max_depth = depth.unwrap_or(1);
        let dir = direction.unwrap_or("out");

        // ── Fetch all non-deleted pages → build id→slug map + find start ──
        let mut rows = conn
            .query(
                "SELECT id, slug, source_id FROM pages WHERE deleted_at IS NULL",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("traverse_paths pages query: {e}")))?;

        let mut id_to_slug: HashMap<u64, String> = HashMap::new();
        let mut start_page_id: Option<u64> = None;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("traverse_paths pages row: {e}")))?
        {
            let id: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("traverse_paths decode id: {e}")))?;
            let slug_str: String = row
                .get(1)
                .map_err(|e| Error::engine(format!("traverse_paths decode slug: {e}")))?;
            let source_id_str: String = row
                .get(2)
                .map_err(|e| Error::engine(format!("traverse_paths decode source_id: {e}")))?;
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
        let mut link_rows = conn
            .query(
                "SELECT from_page_id, to_page_id, link_type, context FROM links",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("traverse_paths links query: {e}")))?;

        // (from_id, to_id, link_type, context)
        let mut edges: Vec<(u64, u64, String, String)> = Vec::new();
        while let Some(row) = link_rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("traverse_paths links row: {e}")))?
        {
            let from_id: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("traverse_paths decode from_page_id: {e}")))?;
            let to_id: i64 = row
                .get(1)
                .map_err(|e| Error::engine(format!("traverse_paths decode to_page_id: {e}")))?;
            let lt: String = row
                .get(2)
                .map_err(|e| Error::engine(format!("traverse_paths decode link_type: {e}")))?;
            let ctx: String = row
                .get(3)
                .map_err(|e| Error::engine(format!("traverse_paths decode context: {e}")))?;
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

    // ─── Minion job queue (Phase 9, slice 1-1-1 A+B) ─────────────────────────
    //
    // SQLite port of the queue. Scheduling columns (lock_until/delay_until/
    // timeout_at) are INTEGER epoch-ms; all `now + N ms` arithmetic happens in
    // Rust (SQLite has no interval type). Record columns are TEXT RFC-3339,
    // written explicitly with `current_utc_iso8601()` so the format matches the
    // postgres backend rather than SQLite's CURRENT_TIMESTAMP. `claim_job` uses
    // BEGIN IMMEDIATE to serialize on the write lock (single-writer analogue of
    // PG's FOR UPDATE SKIP LOCKED).

    async fn enqueue_job(
        &self,
        input: &crate::minions::types::MinionJobInput,
    ) -> Result<crate::minions::types::MinionJob> {
        use crate::minions::types::MinionJobStatus;

        let conn = self.conn().await?;

        // Idempotency fast path: a matching non-null key returns the existing
        // row (the unique partial index guarantees at most one).
        if let Some(ref key) = input.idempotency_key {
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT {MINION_JOB_COLUMNS} FROM minion_jobs WHERE idempotency_key = ?1"
                    ),
                    ::libsql::params![key.as_str()],
                )
                .await
                .map_err(|e| Error::engine(format!("enqueue_job idempotency SELECT: {e}")))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("enqueue_job idempotency row: {e}")))?
            {
                return libsql_row_to_job(&row);
            }
        }

        let now_iso = current_utc_iso8601();
        let now_ms = crate::time::now_epoch_ms();
        let (status, delay_until) = match input.delay {
            Some(d) if d > 0 => (MinionJobStatus::Delayed, Some(now_ms + d)),
            _ => (MinionJobStatus::Waiting, None),
        };
        let max_stalled = input.max_stalled.map_or(5, |v| v.clamp(1, 100));
        let backoff_type = input
            .backoff_type
            .unwrap_or(crate::minions::types::BackoffType::Exponential);
        let on_child_fail = input
            .on_child_fail
            .unwrap_or(crate::minions::types::ChildFailPolicy::FailParent);
        let data_json = input
            .data
            .clone()
            .unwrap_or_else(|| serde_json::json!({}))
            .to_string();

        // D-layer (1-1-3-1): spawning under a parent must validate depth +
        // max_children and flip the parent to waiting-children atomically with
        // the child insert. Wrap in BEGIN IMMEDIATE (SQLite single-writer
        // analogue of the PG `FOR UPDATE` on the parent row). Non-child inserts
        // keep the simple non-transactional path.
        const MAX_SPAWN_DEPTH: i64 = 5;
        let insert_sql = "INSERT INTO minion_jobs \
                (name, queue, status, priority, data, max_attempts, backoff_type, \
                 backoff_delay, backoff_jitter, max_stalled, delay_until, parent_job_id, \
                 depth, on_child_fail, max_children, timeout_ms, remove_on_complete, \
                 remove_on_fail, idempotency_key, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                     ?16, ?17, ?18, ?19, ?20, ?21)";

        let build_params = |depth: i64, parent: ::libsql::Value| -> Vec<::libsql::Value> {
            vec![
                input.name.clone().into(),
                input
                    .queue
                    .clone()
                    .unwrap_or_else(|| "default".to_string())
                    .into(),
                status.as_str().to_string().into(),
                i64::from(input.priority.unwrap_or(0)).into(),
                data_json.clone().into(),
                i64::from(input.max_attempts.unwrap_or(3)).into(),
                backoff_type.as_str().to_string().into(),
                i64::from(input.backoff_delay.unwrap_or(1000)).into(),
                input.backoff_jitter.unwrap_or(0.2).into(),
                i64::from(max_stalled).into(),
                delay_until.map_or(::libsql::Value::Null, ::libsql::Value::from),
                parent,
                depth.into(),
                on_child_fail.as_str().to_string().into(),
                input
                    .max_children
                    .map_or(::libsql::Value::Null, |v| ::libsql::Value::from(i64::from(v))),
                input
                    .timeout_ms
                    .map_or(::libsql::Value::Null, ::libsql::Value::from),
                i64::from(input.remove_on_complete.unwrap_or(false)).into(),
                i64::from(input.remove_on_fail.unwrap_or(false)).into(),
                input
                    .idempotency_key
                    .clone()
                    .map_or(::libsql::Value::Null, ::libsql::Value::from),
                now_iso.clone().into(),
                now_iso.clone().into(),
            ]
        };

        let new_id = if let Some(parent_id) = input.parent_job_id {
            conn.execute("BEGIN IMMEDIATE", ())
                .await
                .map_err(|e| Error::engine(format!("enqueue_job(child) BEGIN: {e}")))?;

            let result = async {
                // Load parent for depth + max_children validation.
                let parent = self.get_job(parent_id).await?.ok_or_else(|| {
                    crate::error::StructuredError::new(
                        "InvalidInput",
                        "invalid_input",
                        format!("parent_job_id {parent_id} not found"),
                    )
                })?;
                let depth = i64::from(parent.depth) + 1;
                if depth > MAX_SPAWN_DEPTH {
                    return Err(crate::error::StructuredError::new(
                        "InvalidInput",
                        "invalid_input",
                        format!("spawn depth {depth} exceeds maxSpawnDepth {MAX_SPAWN_DEPTH}"),
                    ));
                }
                if let Some(cap) = parent.max_children {
                    let live = libsql_select_ids(
                        &conn,
                        "SELECT id FROM minion_jobs WHERE parent_job_id = ?1 \
                         AND status NOT IN ('completed','failed','dead','cancelled')",
                        ::libsql::params![parent_id],
                    )
                    .await?
                    .len() as i64;
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

                conn.execute(
                    insert_sql,
                    ::libsql::params::Params::Positional(build_params(
                        depth,
                        ::libsql::Value::from(parent_id),
                    )),
                )
                .await
                .map_err(|e| Error::engine(format!("enqueue_job(child) INSERT: {e}")))?;
                let child_id = last_insert_rowid(&conn).await?;

                // Flip parent to waiting-children from a runnable state.
                conn.execute(
                    "UPDATE minion_jobs SET status = 'waiting-children', updated_at = ?1 \
                     WHERE id = ?2 AND status IN ('waiting','active','delayed')",
                    ::libsql::params![now_iso.clone(), parent_id],
                )
                .await
                .map_err(|e| Error::engine(format!("enqueue_job(child) parent flip: {e}")))?;

                Ok::<i64, Error>(child_id)
            }
            .await;

            match result {
                Ok(child_id) => {
                    conn.execute("COMMIT", ())
                        .await
                        .map_err(|e| Error::engine(format!("enqueue_job(child) COMMIT: {e}")))?;
                    child_id
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(e);
                }
            }
        } else {
            conn.execute(
                insert_sql,
                ::libsql::params::Params::Positional(build_params(0, ::libsql::Value::Null)),
            )
            .await
            .map_err(|e| Error::engine(format!("enqueue_job INSERT: {e}")))?;
            last_insert_rowid(&conn).await?
        };

        self.get_job(new_id)
            .await?
            .ok_or_else(|| Error::engine("enqueue_job: inserted row not found"))
    }

    async fn get_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                &format!("SELECT {MINION_JOB_COLUMNS} FROM minion_jobs WHERE id = ?1"),
                ::libsql::params![id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_job SELECT: {e}")))?;
        match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_job row: {e}")))?
        {
            Some(row) => Ok(Some(libsql_row_to_job(&row)?)),
            None => Ok(None),
        }
    }

    async fn get_jobs(
        &self,
        filters: &crate::minions::types::JobFilters,
    ) -> Result<Vec<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;

        let mut sql = format!("SELECT {MINION_JOB_COLUMNS} FROM minion_jobs");
        let mut clauses: Vec<&str> = Vec::new();
        let mut params: Vec<::libsql::Value> = Vec::new();
        if let Some(status) = filters.status {
            clauses.push("status = ?");
            params.push(status.as_str().to_string().into());
        }
        if let Some(ref queue) = filters.queue {
            clauses.push("queue = ?");
            params.push(queue.clone().into());
        }
        if let Some(ref name) = filters.name {
            clauses.push("name = ?");
            params.push(name.clone().into());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ? OFFSET ?");
        params.push(filters.limit.unwrap_or(50).max(0).into());
        params.push(filters.offset.unwrap_or(0).max(0).into());

        let mut rows = conn
            .query(&sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("get_jobs SELECT: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_jobs row: {e}")))?
        {
            out.push(libsql_row_to_job(&row)?);
        }
        Ok(out)
    }

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

        let conn = self.conn().await?;
        // BEGIN IMMEDIATE takes the write lock up front so a concurrent claimer
        // blocks here instead of racing the SELECT→UPDATE window. Single-writer
        // equivalent of FOR UPDATE SKIP LOCKED.
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| Error::engine(format!("claim_job BEGIN: {e}")))?;

        let result = claim_job_locked(&conn, lock_token, lock_duration_ms, queue, registered_names)
            .await;

        match result {
            Ok(job) => {
                conn.execute("COMMIT", ())
                    .await
                    .map_err(|e| Error::engine(format!("claim_job COMMIT: {e}")))?;
                Ok(job)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(e)
            }
        }
    }

    async fn complete_job(
        &self,
        id: i64,
        lock_token: &str,
        result: Option<&serde_json::Value>,
    ) -> Result<Option<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        let result_json = result.map(std::string::ToString::to_string);

        // Wrap the token-fenced flip + parent hook in a single writer txn so the
        // child_done insert, token rollup, and parent resolve commit atomically
        // with the child transition (SQLite single-writer analogue of the PG
        // FOR UPDATE on the parent row; mirrors claim_job / handle_stalled).
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| Error::engine(format!("complete_job BEGIN: {e}")))?;

        let outcome = async {
            let affected = conn
                .execute(
                    "UPDATE minion_jobs SET status = 'completed', result = ?1, \
                        finished_at = ?2, lock_token = NULL, lock_until = NULL, updated_at = ?2 \
                     WHERE id = ?3 AND status = 'active' AND lock_token = ?4",
                    ::libsql::params![
                        result_json
                            .clone()
                            .map_or(::libsql::Value::Null, ::libsql::Value::from),
                        now_iso.clone(),
                        id,
                        lock_token
                    ],
                )
                .await
                .map_err(|e| Error::engine(format!("complete_job UPDATE: {e}")))?;
            if affected == 0 {
                return Ok::<Option<crate::minions::types::MinionJob>, Error>(None);
            }

            let job = libsql_get_job(&conn, id)
                .await?
                .ok_or_else(|| Error::engine("complete_job: row vanished after UPDATE"))?;

            // D-layer parent hook: roll up tokens, emit child_done, resolve.
            if let Some(parent_id) = job.parent_job_id {
                if job.tokens_input > 0 || job.tokens_output > 0 || job.tokens_cache_read > 0 {
                    conn.execute(
                        "UPDATE minion_jobs SET tokens_input = tokens_input + ?1, \
                            tokens_output = tokens_output + ?2, \
                            tokens_cache_read = tokens_cache_read + ?3, updated_at = ?4 \
                         WHERE id = ?5 AND status NOT IN \
                            ('completed','failed','dead','cancelled')",
                        ::libsql::params![
                            job.tokens_input,
                            job.tokens_output,
                            job.tokens_cache_read,
                            now_iso.clone(),
                            parent_id
                        ],
                    )
                    .await
                    .map_err(|e| Error::engine(format!("complete_job token rollup: {e}")))?;
                }
                libsql_emit_child_done(
                    &conn,
                    parent_id,
                    job.id,
                    &job.name,
                    result.cloned().unwrap_or(serde_json::Value::Null),
                    crate::minions::types::ChildOutcome::Complete,
                    None,
                )
                .await?;
                libsql_resolve_parent(&conn, parent_id, &now_iso).await?;
            }

            if job.remove_on_complete {
                conn.execute(
                    "DELETE FROM minion_jobs WHERE id = ?1",
                    ::libsql::params![id],
                )
                .await
                .map_err(|e| Error::engine(format!("complete_job remove_on_complete: {e}")))?;
            }
            Ok(Some(job))
        }
        .await;

        match outcome {
            Ok(job) => {
                conn.execute("COMMIT", ())
                    .await
                    .map_err(|e| Error::engine(format!("complete_job COMMIT: {e}")))?;
                Ok(job)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(e)
            }
        }
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

        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        let now_ms = crate::time::now_epoch_ms();
        let new_status = outcome.as_status();

        // Delayed retry sets delay_until = now + backoff; terminal outcomes set
        // finished_at. stacktrace is a JSON array — append the error text.
        let (delay_until, finished_at): (::libsql::Value, ::libsql::Value) =
            if outcome == FailOutcome::Delayed {
                ((now_ms + backoff_ms).into(), ::libsql::Value::Null)
            } else {
                (::libsql::Value::Null, now_iso.clone().into())
            };
        let stacktrace_json = serde_json::json!([error_text]).to_string();

        // Single writer txn: the fail flip, child_done emit, and on_child_fail
        // policy commit atomically (see complete_job for the rationale).
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| Error::engine(format!("fail_job BEGIN: {e}")))?;

        let result = async {
            let affected = conn
                .execute(
                    "UPDATE minion_jobs SET status = ?1, error_text = ?2, \
                        attempts_made = attempts_made + 1, stacktrace = ?3, \
                        delay_until = ?4, finished_at = ?5, \
                        lock_token = NULL, lock_until = NULL, updated_at = ?6 \
                     WHERE id = ?7 AND status = 'active' AND lock_token = ?8",
                    ::libsql::params![
                        new_status.as_str(),
                        error_text,
                        stacktrace_json,
                        delay_until,
                        finished_at,
                        now_iso.clone(),
                        id,
                        lock_token
                    ],
                )
                .await
                .map_err(|e| Error::engine(format!("fail_job UPDATE: {e}")))?;
            if affected == 0 {
                return Ok::<Option<crate::minions::types::MinionJob>, Error>(None);
            }

            let job = libsql_get_job(&conn, id)
                .await?
                .ok_or_else(|| Error::engine("fail_job: row vanished after UPDATE"))?;

            // D-layer parent hook on terminal failure. Emit child_done BEFORE
            // any parent-terminal flip (the EXISTS guard on emit would drop the
            // message once the parent is failed), then apply on_child_fail.
            if outcome.is_terminal() {
                if let Some(parent_id) = job.parent_job_id {
                    let child_outcome = if outcome == FailOutcome::Dead {
                        crate::minions::types::ChildOutcome::Dead
                    } else {
                        crate::minions::types::ChildOutcome::Failed
                    };
                    libsql_emit_child_done(
                        &conn,
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
                            conn.execute(
                                "UPDATE minion_jobs SET status = 'failed', \
                                    error_text = ?1, finished_at = ?2, updated_at = ?2 \
                                 WHERE id = ?3 AND status = 'waiting-children'",
                                ::libsql::params![
                                    format!("child job {} failed: {error_text}", job.id),
                                    now_iso.clone(),
                                    parent_id
                                ],
                            )
                            .await
                            .map_err(|e| Error::engine(format!("fail_job fail_parent: {e}")))?;
                        }
                        ChildFailPolicy::RemoveDep => {
                            conn.execute(
                                "UPDATE minion_jobs SET parent_job_id = NULL, updated_at = ?1 \
                                 WHERE id = ?2",
                                ::libsql::params![now_iso.clone(), job.id],
                            )
                            .await
                            .map_err(|e| Error::engine(format!("fail_job remove_dep: {e}")))?;
                            libsql_resolve_parent(&conn, parent_id, &now_iso).await?;
                        }
                        ChildFailPolicy::Ignore | ChildFailPolicy::Continue => {
                            libsql_resolve_parent(&conn, parent_id, &now_iso).await?;
                        }
                    }
                }
            }

            if outcome.is_terminal() && job.remove_on_fail {
                conn.execute(
                    "DELETE FROM minion_jobs WHERE id = ?1",
                    ::libsql::params![id],
                )
                .await
                .map_err(|e| Error::engine(format!("fail_job remove_on_fail: {e}")))?;
            }
            Ok(Some(job))
        }
        .await;

        match result {
            Ok(job) => {
                conn.execute("COMMIT", ())
                    .await
                    .map_err(|e| Error::engine(format!("fail_job COMMIT: {e}")))?;
                Ok(job)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(e)
            }
        }
    }

    async fn renew_job_lock(
        &self,
        id: i64,
        lock_token: &str,
        lock_duration_ms: i64,
    ) -> Result<bool> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        let lock_until = crate::time::now_epoch_ms() + lock_duration_ms;
        let affected = conn
            .execute(
                "UPDATE minion_jobs SET lock_until = ?1, updated_at = ?2 \
                 WHERE id = ?3 AND lock_token = ?4 AND status = 'active'",
                ::libsql::params![lock_until, now_iso, id, lock_token],
            )
            .await
            .map_err(|e| Error::engine(format!("renew_job_lock UPDATE: {e}")))?;
        Ok(affected > 0)
    }

    async fn retry_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        let affected = conn
            .execute(
                "UPDATE minion_jobs SET status = 'waiting', error_text = NULL, \
                    lock_token = NULL, lock_until = NULL, delay_until = NULL, \
                    finished_at = NULL, updated_at = ?1 \
                 WHERE id = ?2 AND status IN ('failed', 'dead')",
                ::libsql::params![now_iso, id],
            )
            .await
            .map_err(|e| Error::engine(format!("retry_job UPDATE: {e}")))?;
        if affected == 0 {
            return Ok(None);
        }
        self.get_job(id).await
    }

    // --- Ops: pause / resume (1-1-3-3) ---

    async fn pause_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        let affected = conn
            .execute(
                "UPDATE minion_jobs SET status = 'paused', \
                    lock_token = NULL, lock_until = NULL, updated_at = ?1 \
                 WHERE id = ?2 AND status IN ('waiting', 'active', 'delayed')",
                ::libsql::params![now_iso, id],
            )
            .await
            .map_err(|e| Error::engine(format!("pause_job UPDATE: {e}")))?;
        if affected == 0 {
            return Ok(None);
        }
        self.get_job(id).await
    }

    async fn resume_job(&self, id: i64) -> Result<Option<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        let affected = conn
            .execute(
                "UPDATE minion_jobs SET status = 'waiting', \
                    lock_token = NULL, lock_until = NULL, updated_at = ?1 \
                 WHERE id = ?2 AND status = 'paused'",
                ::libsql::params![now_iso, id],
            )
            .await
            .map_err(|e| Error::engine(format!("resume_job UPDATE: {e}")))?;
        if affected == 0 {
            return Ok(None);
        }
        self.get_job(id).await
    }

    async fn prune_jobs(
        &self,
        statuses: &[crate::minions::types::MinionJobStatus],
        older_than_rfc3339: &str,
    ) -> Result<i64> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let conn = self.conn().await?;

        // Build `status IN (?, ?, ...)` with one placeholder per status. The
        // cutoff compares `updated_at` (TEXT RFC-3339); ISO-8601 lexical order
        // == time order. Child rows (inbox, attachments) go via ON DELETE
        // CASCADE (schema FKs + `PRAGMA foreign_keys = ON`).
        let placeholders = std::iter::repeat("?")
            .take(statuses.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "DELETE FROM minion_jobs \
             WHERE status IN ({placeholders}) AND updated_at < ?"
        );

        let mut params: Vec<::libsql::Value> = statuses
            .iter()
            .map(|s| ::libsql::Value::from(s.as_str().to_string()))
            .collect();
        params.push(::libsql::Value::from(older_than_rfc3339.to_string()));

        let affected = conn
            .execute(&sql, params)
            .await
            .map_err(|e| Error::engine(format!("prune_jobs DELETE: {e}")))?;
        Ok(affected as i64)
    }

    async fn get_stats(
        &self,
        since_rfc3339: &str,
    ) -> Result<crate::minions::types::QueueStats> {
        use crate::minions::types::{QueueHealth, QueueStats, QueueTypeStat};
        use std::collections::BTreeMap;

        let conn = self.conn().await?;

        // by_status: all-time count per status.
        let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
        let mut rows = conn
            .query("SELECT status, count(*) FROM minion_jobs GROUP BY status", ())
            .await
            .map_err(|e| Error::engine(format!("get_stats by_status: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_stats by_status row: {e}")))?
        {
            let status: String = row
                .get::<String>(0)
                .map_err(|e| Error::engine(format!("status read: {e}")))?;
            let count: i64 = row
                .get::<i64>(1)
                .map_err(|e| Error::engine(format!("count read: {e}")))?;
            by_status.insert(status, count);
        }

        // by_type: per-name breakdown in the `since` window. SQLite has no
        // FILTER / EXTRACT, so terminal counts use SUM(CASE ...) and the mean
        // runtime uses AVG over (julianday(finished) - julianday(started)) days
        // scaled to ms. AVG ignores NULLs, so the CASE yields NULL for rows
        // missing a timestamp — matching the TS FILTER semantics. `created_at`
        // is TEXT RFC-3339; ISO-8601 lexical order == time order.
        let mut by_type: Vec<QueueTypeStat> = Vec::new();
        let mut trows = conn
            .query(
                "SELECT name, \
                    count(*) AS total, \
                    SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) AS completed, \
                    SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) AS failed, \
                    SUM(CASE WHEN status = 'dead' THEN 1 ELSE 0 END) AS dead, \
                    AVG(CASE WHEN finished_at IS NOT NULL AND started_at IS NOT NULL \
                             THEN (julianday(finished_at) - julianday(started_at)) * 86400000.0 \
                             ELSE NULL END) AS avg_duration_ms \
                 FROM minion_jobs WHERE created_at >= ?1 \
                 GROUP BY name ORDER BY total DESC, name ASC",
                ::libsql::params![since_rfc3339],
            )
            .await
            .map_err(|e| Error::engine(format!("get_stats by_type: {e}")))?;
        while let Some(row) = trows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_stats by_type row: {e}")))?
        {
            // AVG returns REAL (or NULL); read as Option<f64> then round to ms.
            let avg_duration_ms = row
                .get::<Option<f64>>(5)
                .map_err(|e| Error::engine(format!("avg_duration read: {e}")))?
                .map(|v| v.round() as i64);
            by_type.push(QueueTypeStat {
                name: row
                    .get::<String>(0)
                    .map_err(|e| Error::engine(format!("name read: {e}")))?,
                total: row
                    .get::<i64>(1)
                    .map_err(|e| Error::engine(format!("total read: {e}")))?,
                completed: row
                    .get::<i64>(2)
                    .map_err(|e| Error::engine(format!("completed read: {e}")))?,
                failed: row
                    .get::<i64>(3)
                    .map_err(|e| Error::engine(format!("failed read: {e}")))?,
                dead: row
                    .get::<i64>(4)
                    .map_err(|e| Error::engine(format!("dead read: {e}")))?,
                avg_duration_ms,
            });
        }

        // queue_health: stalled = active jobs whose epoch-ms lease has expired.
        let now_ms = crate::time::now_epoch_ms();
        let mut srows = conn
            .query(
                "SELECT count(*) FROM minion_jobs \
                 WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < ?1",
                ::libsql::params![now_ms],
            )
            .await
            .map_err(|e| Error::engine(format!("get_stats stalled: {e}")))?;
        let stalled = match srows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_stats stalled row: {e}")))?
        {
            Some(row) => row
                .get::<i64>(0)
                .map_err(|e| Error::engine(format!("stalled read: {e}")))?,
            None => 0,
        };

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

    async fn get_brain_stats(&self) -> Result<BrainStats> {
        let conn = self.conn().await?;

        // Small helper: run a single-column integer count query.
        async fn scalar_count(conn: &::libsql::Connection, sql: &str) -> Result<i64> {
            let v = conn
                .query(sql, ())
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats count: {e}")))?
                .next()
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats count row: {e}")))?
                .map(|r| r.get::<i64>(0))
                .transpose()
                .map_err(|e| Error::engine(format!("get_brain_stats count decode: {e}")))?
                .unwrap_or(0);
            Ok(v)
        }

        let page_count =
            scalar_count(&conn, "SELECT COUNT(*) FROM pages WHERE deleted_at IS NULL").await?;

        // No content_chunks table in Rust — approximate chunk_count as live
        // pages carrying non-empty compiled_truth (same proxy as
        // get_full_stats). Registered in docs/plans/KNOWN-GAPS.md (G46).
        let chunk_count = scalar_count(
            &conn,
            "SELECT COUNT(*) FROM pages \
             WHERE compiled_truth IS NOT NULL AND compiled_truth != '' AND deleted_at IS NULL",
        )
        .await?;

        // embedded_count: page-level embedding (G24) — live pages whose
        // embedding BLOB is set.
        let embedded_count = scalar_count(
            &conn,
            "SELECT COUNT(*) FROM pages WHERE embedding IS NOT NULL AND deleted_at IS NULL",
        )
        .await?;

        let link_count = scalar_count(&conn, "SELECT COUNT(*) FROM links").await?;

        let tag_count =
            scalar_count(&conn, "SELECT COUNT(DISTINCT tag) FROM page_tags").await?;

        // timeline is a JSON-array string per page; sum array lengths on the
        // Rust side (no timeline_entries table).
        let timeline_entry_count = {
            let mut total = 0i64;
            let mut rows = conn
                .query("SELECT timeline FROM pages WHERE deleted_at IS NULL", ())
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats timeline: {e}")))?;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats timeline row: {e}")))?
            {
                let tl: String = row.get::<String>(0).unwrap_or_default();
                if let Ok(serde_json::Value::Array(arr)) =
                    serde_json::from_str::<serde_json::Value>(&tl)
                {
                    total += arr.len() as i64;
                }
            }
            total
        };

        // pages_by_type mirrors TS: grouped over ALL pages (no soft-delete
        // filter). page_count above is the only soft-delete-excluding count.
        let mut pages_by_type: std::collections::BTreeMap<String, i64> =
            std::collections::BTreeMap::new();
        {
            let mut rows = conn
                .query("SELECT type, COUNT(*) FROM pages GROUP BY type", ())
                .await
                .map_err(|e| Error::engine(format!("get_brain_stats pages_by_type: {e}")))?;
            while let Some(row) = rows.next().await.map_err(|e| {
                Error::engine(format!("get_brain_stats pages_by_type row: {e}"))
            })? {
                let ty: String = row.get::<String>(0).unwrap_or_default();
                let cnt: i64 = row.get::<i64>(1).unwrap_or(0);
                pages_by_type.insert(ty, cnt);
            }
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

        let conn = self.conn().await?;

        // ── Scalar metrics in one round-trip ──────────────────────────────
        // Backend-model note (see BrainStats docs / KNOWN-GAPS G24, G46):
        // the Rust production schema has NO content_chunks / timeline_entries
        // tables. So embedding coverage is computed at the PAGE level (one
        // embedding BLOB per page, G24) rather than TS's chunk level, and
        // timeline lives as a JSON-array string column parsed Rust-side below.
        // Everything else mirrors the InMemory `get_health` (engine.rs) and
        // the TS `getHealth` (pglite-engine.ts) semantics:
        //   * page_count / entity counts exclude soft-deleted pages
        //     (deleted_at IS NULL) — matches InMemory `live_pages`.
        //   * dead_links = links whose target page is missing OR soft-deleted
        //     (deleted-aware, matching InMemory `live_ids`; slightly stricter
        //     than TS which ignores deleted_at).
        //   * orphan_pages = islanded: no inbound AND no outbound link.
        let row = conn
            .query(
                "SELECT \
                   (SELECT COUNT(*) FROM pages WHERE deleted_at IS NULL), \
                   (SELECT COUNT(*) FROM pages WHERE deleted_at IS NULL AND embedding IS NULL), \
                   (SELECT COUNT(*) FROM links), \
                   (SELECT COUNT(*) FROM links l \
                      WHERE NOT EXISTS (SELECT 1 FROM pages p \
                        WHERE p.id = l.to_page_id AND p.deleted_at IS NULL)), \
                   (SELECT COUNT(*) FROM pages p WHERE p.deleted_at IS NULL \
                      AND NOT EXISTS (SELECT 1 FROM links l WHERE l.to_page_id = p.id) \
                      AND NOT EXISTS (SELECT 1 FROM links l WHERE l.from_page_id = p.id)), \
                   (SELECT COUNT(*) FROM pages \
                      WHERE deleted_at IS NULL AND type IN ('person', 'company')), \
                   (SELECT COUNT(*) FROM pages e WHERE e.deleted_at IS NULL \
                      AND e.type IN ('person', 'company') \
                      AND EXISTS (SELECT 1 FROM links l WHERE l.to_page_id = e.id))",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("get_health scalars: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_health scalars row: {e}")))?;

        let (
            page_count,
            missing_embeddings,
            link_count,
            dead_links,
            orphan_pages,
            entity_count,
            entities_with_inbound,
        ) = match row {
            Some(r) => (
                r.get::<i64>(0).unwrap_or(0),
                r.get::<i64>(1).unwrap_or(0),
                r.get::<i64>(2).unwrap_or(0),
                r.get::<i64>(3).unwrap_or(0),
                r.get::<i64>(4).unwrap_or(0),
                r.get::<i64>(5).unwrap_or(0),
                r.get::<i64>(6).unwrap_or(0),
            ),
            None => (0, 0, 0, 0, 0, 0, 0),
        };

        // ── Timeline metrics (Rust-side JSON parse) ───────────────────────
        // timeline is a JSON-array string per page; a page "has timeline" iff
        // that array is non-empty. Count both all live pages and entity pages.
        let (pages_with_timeline, entities_with_timeline) = {
            let mut all = 0i64;
            let mut entity = 0i64;
            let mut rows = conn
                .query(
                    "SELECT type, timeline FROM pages WHERE deleted_at IS NULL",
                    (),
                )
                .await
                .map_err(|e| Error::engine(format!("get_health timeline: {e}")))?;
            while let Some(r) = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("get_health timeline row: {e}")))?
            {
                let ty: String = r.get::<String>(0).unwrap_or_default();
                let tl: String = r.get::<String>(1).unwrap_or_default();
                let has_tl = matches!(
                    serde_json::from_str::<serde_json::Value>(&tl),
                    Ok(serde_json::Value::Array(ref a)) if !a.is_empty()
                );
                if has_tl {
                    all += 1;
                    if ty == "person" || ty == "company" {
                        entity += 1;
                    }
                }
            }
            (all, entity)
        };

        // ── most_connected: top 5 entities by (in + out) link count ───────
        // Excludes entities with zero links (matches InMemory's filter_map on
        // the link-count map). Deterministic tie-break by slug.
        let most_connected = {
            let mut out: Vec<MostConnectedEntry> = Vec::new();
            let mut rows = conn
                .query(
                    "SELECT slug, lc FROM ( \
                       SELECT p.slug AS slug, \
                              (SELECT COUNT(*) FROM links l \
                                 WHERE l.from_page_id = p.id OR l.to_page_id = p.id) AS lc \
                       FROM pages p \
                       WHERE p.deleted_at IS NULL AND p.type IN ('person', 'company') \
                     ) WHERE lc > 0 \
                     ORDER BY lc DESC, slug ASC LIMIT 5",
                    (),
                )
                .await
                .map_err(|e| Error::engine(format!("get_health most_connected: {e}")))?;
            while let Some(r) = rows.next().await.map_err(|e| {
                Error::engine(format!("get_health most_connected row: {e}"))
            })? {
                let slug: String = r.get::<String>(0).unwrap_or_default();
                let lc: i64 = r.get::<i64>(1).unwrap_or(0);
                out.push(MostConnectedEntry {
                    slug,
                    link_count: lc.max(0) as usize,
                });
            }
            out
        };

        // ── Derived ratios ────────────────────────────────────────────────
        let embedded_pages = (page_count - missing_embeddings).max(0);
        let embed_coverage = if page_count > 0 {
            embedded_pages as f64 / page_count as f64
        } else {
            1.0 // empty brain → nothing to embed (matches InMemory)
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

        // ── Score computation (mirrors InMemory engine.rs) ────────────────
        // v0.37.10.0: empty brains (page_count == 0) get FULL marks (100/100).
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
            stale_pages: 0, // no timeline_entries table (matches InMemory)
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


    // C-layer state transition (UPDATE), then reselects the mutated rows.
    // Scheduling columns are stored as INTEGER epoch-ms, compared directly
    // against `now_ms`. No inbox / parent unblock here (D-layer, 1-1-3).

    async fn promote_delayed(&self) -> Result<Vec<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_ms = crate::time::now_epoch_ms();
        let now_iso = current_utc_iso8601();

        let ids = libsql_select_ids(
            &conn,
            "SELECT id FROM minion_jobs \
             WHERE status = 'delayed' AND delay_until IS NOT NULL AND delay_until <= ?1",
            ::libsql::params![now_ms],
        )
        .await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        conn.execute(
            "UPDATE minion_jobs SET status = 'waiting', delay_until = NULL, \
                lock_token = NULL, lock_until = NULL, updated_at = ?1 \
             WHERE status = 'delayed' AND delay_until IS NOT NULL AND delay_until <= ?2",
            ::libsql::params![now_iso, now_ms],
        )
        .await
        .map_err(|e| Error::engine(format!("promote_delayed UPDATE: {e}")))?;

        libsql_reselect_jobs(&conn, &ids).await
    }

    async fn handle_stalled(&self) -> Result<crate::minions::types::StalledSweep> {
        use crate::minions::types::StalledSweep;

        let conn = self.conn().await?;
        let now_ms = crate::time::now_epoch_ms();
        let now_iso = current_utc_iso8601();

        // BEGIN IMMEDIATE grabs the write lock up front (SQLite analogue of
        // FOR UPDATE SKIP LOCKED); mirrors the claim_job_locked precedent.
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| Error::engine(format!("handle_stalled BEGIN: {e}")))?;

        let result = async {
            // Stalled candidates: active with an expired lease. Partition on
            // `stalled_counter + 1 < max_stalled` (requeue) vs `>=` (dead).
            let requeued_ids = libsql_select_ids(
                &conn,
                "SELECT id FROM minion_jobs \
                 WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < ?1 \
                   AND stalled_counter + 1 < max_stalled",
                ::libsql::params![now_ms],
            )
            .await?;
            let dead_ids = libsql_select_ids(
                &conn,
                "SELECT id FROM minion_jobs \
                 WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < ?1 \
                   AND stalled_counter + 1 >= max_stalled",
                ::libsql::params![now_ms],
            )
            .await?;

            if !requeued_ids.is_empty() {
                conn.execute(
                    "UPDATE minion_jobs SET status = 'waiting', \
                        stalled_counter = stalled_counter + 1, \
                        lock_token = NULL, lock_until = NULL, updated_at = ?1 \
                     WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < ?2 \
                       AND stalled_counter + 1 < max_stalled",
                    ::libsql::params![now_iso.clone(), now_ms],
                )
                .await
                .map_err(|e| Error::engine(format!("handle_stalled requeue UPDATE: {e}")))?;
            }
            if !dead_ids.is_empty() {
                conn.execute(
                    "UPDATE minion_jobs SET status = 'dead', \
                        stalled_counter = stalled_counter + 1, \
                        error_text = 'max stalled count exceeded', \
                        lock_token = NULL, lock_until = NULL, \
                        finished_at = ?1, updated_at = ?1 \
                     WHERE status = 'active' AND lock_until IS NOT NULL AND lock_until < ?2 \
                       AND stalled_counter + 1 >= max_stalled",
                    ::libsql::params![now_iso, now_ms],
                )
                .await
                .map_err(|e| Error::engine(format!("handle_stalled dead UPDATE: {e}")))?;
            }
            Ok::<_, Error>((requeued_ids, dead_ids))
        }
        .await;

        let (requeued_ids, dead_ids) = match result {
            Ok(v) => v,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                return Err(e);
            }
        };

        conn.execute("COMMIT", ())
            .await
            .map_err(|e| Error::engine(format!("handle_stalled COMMIT: {e}")))?;

        Ok(StalledSweep {
            requeued: libsql_reselect_jobs(&conn, &requeued_ids).await?,
            dead: libsql_reselect_jobs(&conn, &dead_ids).await?,
        })
    }

    async fn handle_timeouts(&self) -> Result<Vec<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_ms = crate::time::now_epoch_ms();
        let now_iso = current_utc_iso8601();

        // Active, per-job timeout elapsed, lease still held. A stalled job with
        // an expired lease (lock_until < now) is left for handle_stalled.
        let ids = libsql_select_ids(
            &conn,
            "SELECT id FROM minion_jobs \
             WHERE status = 'active' AND timeout_at IS NOT NULL AND timeout_at < ?1 \
               AND lock_until IS NOT NULL AND lock_until > ?1",
            ::libsql::params![now_ms],
        )
        .await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        conn.execute(
            "UPDATE minion_jobs SET status = 'dead', error_text = 'timeout exceeded', \
                lock_token = NULL, lock_until = NULL, finished_at = ?1, updated_at = ?1 \
             WHERE status = 'active' AND timeout_at IS NOT NULL AND timeout_at < ?2 \
               AND lock_until IS NOT NULL AND lock_until > ?2",
            ::libsql::params![now_iso, now_ms],
        )
        .await
        .map_err(|e| Error::engine(format!("handle_timeouts UPDATE: {e}")))?;

        libsql_reselect_jobs(&conn, &ids).await
    }

    async fn handle_wall_clock_timeouts(
        &self,
        lock_duration_ms: i64,
    ) -> Result<Vec<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();

        // Threshold is computed in SQL. `started_at` is an RFC-3339 string;
        // julianday() parses it and yields days, *86400*1000 -> elapsed ms.
        // CASE: timeout_ms present -> timeout_ms*2; else lock_duration_ms*2*
        // GREATEST(max_stalled, 1). Unlike handle_timeouts this ignores lease
        // state — it catches jobs wedged while holding a DB resource.
        let elapsed_ms = "(julianday(?1) - julianday(started_at)) * 86400000.0";
        let threshold = "CASE WHEN timeout_ms IS NOT NULL THEN timeout_ms * 2 \
             ELSE ?2 * 2 * MAX(max_stalled, 1) END";
        let where_clause = format!(
            "status = 'active' AND started_at IS NOT NULL AND {elapsed_ms} > {threshold}"
        );

        let select_sql = format!("SELECT id FROM minion_jobs WHERE {where_clause}");
        let ids = libsql_select_ids(
            &conn,
            &select_sql,
            ::libsql::params![now_iso.clone(), lock_duration_ms],
        )
        .await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let update_sql = format!(
            "UPDATE minion_jobs SET status = 'dead', \
                error_text = 'wall-clock timeout exceeded', \
                lock_token = NULL, lock_until = NULL, finished_at = ?1, updated_at = ?1 \
             WHERE {where_clause}"
        );
        conn.execute(
            &update_sql,
            ::libsql::params![now_iso, lock_duration_ms],
        )
        .await
        .map_err(|e| Error::engine(format!("handle_wall_clock_timeouts UPDATE: {e}")))?;

        libsql_reselect_jobs(&conn, &ids).await
    }

    async fn set_started_at_for_test(&self, id: i64, started_at_rfc3339: &str) -> Result<()> {
        let conn = self.conn().await?;
        conn.execute(
            "UPDATE minion_jobs SET started_at = ?1 WHERE id = ?2",
            ::libsql::params![started_at_rfc3339, id],
        )
        .await
        .map_err(|e| Error::engine(format!("set_started_at_for_test UPDATE: {e}")))?;
        Ok(())
    }

    async fn set_timeout_at_for_test(&self, id: i64, timeout_at_ms: i64) -> Result<()> {
        let conn = self.conn().await?;
        conn.execute(
            "UPDATE minion_jobs SET timeout_at = ?1 WHERE id = ?2",
            ::libsql::params![timeout_at_ms, id],
        )
        .await
        .map_err(|e| Error::engine(format!("set_timeout_at_for_test UPDATE: {e}")))?;
        Ok(())
    }

    // ─── D-layer: cancellation + inbox (parent/child coordination) ──────────

    async fn cancel_job(
        &self,
        id: i64,
    ) -> Result<Option<crate::minions::types::MinionJob>> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();

        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| Error::engine(format!("cancel_job BEGIN: {e}")))?;

        let result = async {
            // Collect the descendant subtree via a recursive CTE (depth-capped
            // at 100 to bound pathological cycles), then cancel only the
            // non-terminal rows. Capturing ids first mirrors the sweeps: SQLite
            // has no UPDATE ... RETURNING here.
            let subtree_ids = libsql_select_ids(
                &conn,
                "WITH RECURSIVE subtree(id, lvl) AS ( \
                    SELECT id, 0 FROM minion_jobs WHERE id = ?1 \
                    UNION ALL \
                    SELECT j.id, s.lvl + 1 FROM minion_jobs j \
                    JOIN subtree s ON j.parent_job_id = s.id \
                    WHERE s.lvl < 100 \
                 ) SELECT id FROM subtree",
                ::libsql::params![id],
            )
            .await?;
            if subtree_ids.is_empty() {
                return Ok::<Option<crate::minions::types::MinionJob>, Error>(None);
            }

            // Non-terminal rows in the subtree that will actually transition.
            // Capture (id, parent_job_id, name) BEFORE the UPDATE so we can emit
            // child_done + resolve after.
            let mut affected: Vec<(i64, Option<i64>, String)> = Vec::new();
            let mut root_transitioned = false;
            for cid in &subtree_ids {
                if let Some(job) = libsql_get_job(&conn, *cid).await? {
                    if !job.status.is_terminal() {
                        affected.push((job.id, job.parent_job_id, job.name.clone()));
                        if job.id == id {
                            root_transitioned = true;
                        }
                    }
                }
            }

            // TS contract: an already-terminal root yields None (nothing moved).
            if !root_transitioned {
                return Ok(None);
            }

            for (cid, _, _) in &affected {
                conn.execute(
                    "UPDATE minion_jobs SET status = 'cancelled', \
                        lock_token = NULL, lock_until = NULL, \
                        finished_at = ?1, updated_at = ?1 \
                     WHERE id = ?2 AND status NOT IN \
                        ('completed','failed','dead','cancelled')",
                    ::libsql::params![now_iso.clone(), cid],
                )
                .await
                .map_err(|e| Error::engine(format!("cancel_job UPDATE: {e}")))?;
            }

            // Emit child_done(cancelled) to each affected parent, then resolve.
            let mut parent_ids: Vec<i64> = Vec::new();
            for (cid, parent, name) in &affected {
                if let Some(pid) = parent {
                    libsql_emit_child_done(
                        &conn,
                        *pid,
                        *cid,
                        name,
                        serde_json::Value::Null,
                        crate::minions::types::ChildOutcome::Cancelled,
                        Some("cancelled".to_string()),
                    )
                    .await?;
                    if !parent_ids.contains(pid) {
                        parent_ids.push(*pid);
                    }
                }
            }
            for pid in parent_ids {
                libsql_resolve_parent(&conn, pid, &now_iso).await?;
            }

            libsql_get_job(&conn, id).await
        }
        .await;

        match result {
            Ok(job) => {
                conn.execute("COMMIT", ())
                    .await
                    .map_err(|e| Error::engine(format!("cancel_job COMMIT: {e}")))?;
                Ok(job)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(e)
            }
        }
    }

    async fn send_message(
        &self,
        job_id: i64,
        payload: &serde_json::Value,
        sender: &str,
    ) -> Result<Option<crate::minions::types::InboxMessage>> {
        let conn = self.conn().await?;

        // Target must exist and be non-terminal; sender must be 'admin' or the
        // job's parent id string.
        let Some(job) = libsql_get_job(&conn, job_id).await? else {
            return Ok(None);
        };
        if job.status.is_terminal() {
            return Ok(None);
        }
        let parent_str = job.parent_job_id.map(|p| p.to_string());
        if sender != "admin" && Some(sender.to_string()) != parent_str {
            return Ok(None);
        }

        let payload_json = payload.to_string();
        let now_iso = current_utc_iso8601();
        conn.execute(
            "INSERT INTO minion_inbox (job_id, sender, payload, sent_at) \
             VALUES (?1, ?2, ?3, ?4)",
            ::libsql::params![job_id, sender, payload_json, now_iso.clone()],
        )
        .await
        .map_err(|e| Error::engine(format!("send_message INSERT: {e}")))?;
        let msg_id = last_insert_rowid(&conn).await?;

        Ok(Some(crate::minions::types::InboxMessage {
            id: msg_id,
            job_id,
            sender: sender.to_string(),
            payload: payload.clone(),
            sent_at: now_iso,
            read_at: None,
        }))
    }

    async fn read_inbox(
        &self,
        job_id: i64,
        lock_token: &str,
    ) -> Result<Vec<crate::minions::types::InboxMessage>> {
        let conn = self.conn().await?;

        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| Error::engine(format!("read_inbox BEGIN: {e}")))?;

        let result = async {
            // Token fence: caller must hold the active lease.
            let held = !libsql_select_ids(
                &conn,
                "SELECT id FROM minion_jobs \
                 WHERE id = ?1 AND status = 'active' AND lock_token = ?2",
                ::libsql::params![job_id, lock_token],
            )
            .await?
            .is_empty();
            if !held {
                return Ok::<Vec<crate::minions::types::InboxMessage>, Error>(Vec::new());
            }

            // Capture unread ids in send order, mark them read, then reselect.
            let unread_ids = libsql_select_ids(
                &conn,
                "SELECT id FROM minion_inbox \
                 WHERE job_id = ?1 AND read_at IS NULL ORDER BY sent_at, id",
                ::libsql::params![job_id],
            )
            .await?;
            if unread_ids.is_empty() {
                return Ok(Vec::new());
            }
            let now_iso = current_utc_iso8601();
            conn.execute(
                "UPDATE minion_inbox SET read_at = ?1 \
                 WHERE job_id = ?2 AND read_at IS NULL",
                ::libsql::params![now_iso, job_id],
            )
            .await
            .map_err(|e| Error::engine(format!("read_inbox mark read: {e}")))?;

            let mut out = Vec::with_capacity(unread_ids.len());
            for mid in &unread_ids {
                if let Some(m) = libsql_get_inbox_message(&conn, *mid).await? {
                    out.push(m);
                }
            }
            Ok(out)
        }
        .await;

        match result {
            Ok(msgs) => {
                conn.execute("COMMIT", ())
                    .await
                    .map_err(|e| Error::engine(format!("read_inbox COMMIT: {e}")))?;
                Ok(msgs)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(e)
            }
        }
    }

    async fn read_child_completions(
        &self,
        parent_id: i64,
        lock_token: &str,
        since_rfc3339: Option<&str>,
    ) -> Result<Vec<crate::minions::types::ChildDoneMessage>> {
        let conn = self.conn().await?;

        // Same token fence as read_inbox. No marking read — this is a cursor
        // read over child_done envelopes, filtered by an optional `since`.
        let held = !libsql_select_ids(
            &conn,
            "SELECT id FROM minion_jobs \
             WHERE id = ?1 AND status = 'active' AND lock_token = ?2",
            ::libsql::params![parent_id, lock_token],
        )
        .await?
        .is_empty();
        if !held {
            return Ok(Vec::new());
        }

        let (sql, params): (&str, Vec<::libsql::Value>) = match since_rfc3339 {
            Some(since) => (
                "SELECT payload FROM minion_inbox \
                 WHERE job_id = ?1 AND payload->>'type' = 'child_done' AND sent_at > ?2 \
                 ORDER BY sent_at, id",
                vec![parent_id.into(), since.to_string().into()],
            ),
            None => (
                "SELECT payload FROM minion_inbox \
                 WHERE job_id = ?1 AND payload->>'type' = 'child_done' \
                 ORDER BY sent_at, id",
                vec![parent_id.into()],
            ),
        };
        let mut rows = conn
            .query(sql, ::libsql::params::Params::Positional(params))
            .await
            .map_err(|e| Error::engine(format!("read_child_completions SELECT: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("read_child_completions row: {e}")))?
        {
            let payload: String = row
                .get::<String>(0)
                .map_err(|e| Error::engine(format!("read_child_completions decode: {e}")))?;
            if let Ok(msg) = serde_json::from_str(&payload) {
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
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        // Token-fenced: only an active job holding this lease accrues tokens.
        let affected = conn
            .execute(
                "UPDATE minion_jobs SET tokens_input = tokens_input + ?1, \
                    tokens_output = tokens_output + ?2, \
                    tokens_cache_read = tokens_cache_read + ?3, updated_at = ?4 \
                 WHERE id = ?5 AND status = 'active' AND lock_token = ?6",
                ::libsql::params![
                    tokens.input.unwrap_or(0),
                    tokens.output.unwrap_or(0),
                    tokens.cache_read.unwrap_or(0),
                    now_iso,
                    id,
                    lock_token
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("update_tokens UPDATE: {e}")))?;
        Ok(affected > 0)
    }

    async fn remove_child_dependency(&self, child_id: i64) -> Result<()> {
        let conn = self.conn().await?;
        let now_iso = current_utc_iso8601();
        conn.execute(
            "UPDATE minion_jobs SET parent_job_id = NULL, updated_at = ?1 WHERE id = ?2",
            ::libsql::params![now_iso, child_id],
        )
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
        let conn = self.conn().await?;

        // Verify the parent job exists (explicit clearer error than the FK).
        if libsql_get_job(&conn, job_id).await?.is_none() {
            return Err(Error::new(
                "NotFound",
                "not_found",
                format!("job {job_id} not found"),
            ));
        }

        // storage_uri is always NULL for this port (inline content only).
        // External-storage path registered in docs/plans/KNOWN-GAPS.md (G27).
        conn.execute(
            "INSERT INTO minion_attachments \
             (job_id, filename, content_type, content, size_bytes, sha256) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            ::libsql::params![
                job_id,
                att.filename.clone(),
                att.content_type.clone(),
                ::libsql::Value::Blob(att.bytes.clone()),
                att.size_bytes,
                att.sha256.clone(),
            ],
        )
        .await
        .map_err(|e| Error::engine(format!("insert_attachment INSERT: {e}")))?;
        let id = last_insert_rowid(&conn).await?;

        // Read back created_at (DB default) for a faithful metadata row.
        let mut rows = conn
            .query(
                "SELECT created_at FROM minion_attachments WHERE id = ?1",
                ::libsql::params![id],
            )
            .await
            .map_err(|e| Error::engine(format!("insert_attachment reselect: {e}")))?;
        let created_at: String = match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("insert_attachment reselect row: {e}")))?
        {
            Some(row) => row
                .get::<String>(0)
                .map_err(|e| Error::engine(format!("attachment created_at: {e}")))?,
            None => current_utc_iso8601(),
        };

        Ok(crate::minions::types::Attachment {
            id,
            job_id,
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            storage_uri: None,
            size_bytes: att.size_bytes,
            sha256: att.sha256.clone(),
            created_at,
        })
    }

    async fn list_attachment_filenames(&self, job_id: i64) -> Result<Vec<String>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT filename FROM minion_attachments WHERE job_id = ?1",
                ::libsql::params![job_id],
            )
            .await
            .map_err(|e| Error::engine(format!("list_attachment_filenames: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_attachment_filenames row: {e}")))?
        {
            out.push(
                row.get::<String>(0)
                    .map_err(|e| Error::engine(format!("attachment filename: {e}")))?,
            );
        }
        Ok(out)
    }

    async fn list_attachments(
        &self,
        job_id: i64,
    ) -> Result<Vec<crate::minions::types::Attachment>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, job_id, filename, content_type, storage_uri, size_bytes, sha256, created_at \
                 FROM minion_attachments WHERE job_id = ?1 ORDER BY created_at ASC, id ASC",
                ::libsql::params![job_id],
            )
            .await
            .map_err(|e| Error::engine(format!("list_attachments: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_attachments row: {e}")))?
        {
            out.push(libsql_row_to_attachment(&row)?);
        }
        Ok(out)
    }

    async fn get_attachment(
        &self,
        job_id: i64,
        filename: &str,
    ) -> Result<Option<(crate::minions::types::Attachment, Vec<u8>)>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, job_id, filename, content_type, storage_uri, size_bytes, sha256, created_at, content \
                 FROM minion_attachments WHERE job_id = ?1 AND filename = ?2",
                ::libsql::params![job_id, filename],
            )
            .await
            .map_err(|e| Error::engine(format!("get_attachment: {e}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_attachment row: {e}")))?
        else {
            return Ok(None);
        };
        let meta = libsql_row_to_attachment(&row)?;
        // content is column 8; NULL → empty bytes (external-storage rows).
        let bytes = match row
            .get_value(8)
            .map_err(|e| Error::engine(format!("attachment content: {e}")))?
        {
            ::libsql::Value::Blob(b) => b,
            ::libsql::Value::Null => Vec::new(),
            ::libsql::Value::Text(s) => s.into_bytes(),
            other => {
                return Err(Error::engine(format!(
                    "attachment content: unexpected column type {other:?}"
                )))
            }
        };
        Ok(Some((meta, bytes)))
    }

    async fn delete_attachment(&self, job_id: i64, filename: &str) -> Result<bool> {
        let conn = self.conn().await?;
        let affected = conn
            .execute(
                "DELETE FROM minion_attachments WHERE job_id = ?1 AND filename = ?2",
                ::libsql::params![job_id, filename],
            )
            .await
            .map_err(|e| Error::engine(format!("delete_attachment: {e}")))?;
        Ok(affected > 0)
    }
}

/// UNION `code_edges_chunk` + `code_edges_symbol` on a symbol column
/// (`to_symbol_qualified` for callers, `from_symbol_qualified` for callees).
/// Mirrors TS `getCallersOf` / `getCalleesOf`. Free function (not a trait
/// method) so it can be shared by both `get_callers_of` / `get_callees_of`.
async fn code_edge_symbol_query(
    engine: &LibsqlEngine,
    symbol_col: &str,
    qualified_name: &str,
    opts: &crate::import::CodeGraphQueryOpts,
) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
    let conn = engine.conn().await?;
    let limit = (opts.limit.unwrap_or(100) as i64).min(500);

    let mut params: Vec<::libsql::Value> = Vec::new();
    params.push(::libsql::Value::from(qualified_name.to_string()));

    let mut sql = format!(
        "SELECT id, from_chunk_id, to_chunk_id, from_symbol_qualified, to_symbol_qualified, \
                edge_type, edge_metadata, source_id, 1 AS resolved \
           FROM code_edges_chunk WHERE {sym} = ?1",
        sym = symbol_col,
    );
    let mut sym_sql = format!(
        "SELECT id, from_chunk_id, NULL AS to_chunk_id, from_symbol_qualified, to_symbol_qualified, \
                edge_type, edge_metadata, source_id, 0 AS resolved \
           FROM code_edges_symbol WHERE {sym} = ?1",
        sym = symbol_col,
    );
    if !opts.all_sources {
        if let Some(sid) = &opts.source_id {
            sql.push_str(" AND source_id = ?2");
            sym_sql.push_str(" AND source_id = ?2");
            params.push(::libsql::Value::from(sid.clone()));
        }
    }
        let limit_ph = params.len() + 1;
        sql.push_str(&format!(" UNION ALL {sym_sql} LIMIT ?{limit_ph}"));
        params.push(::libsql::Value::from(limit));

    let mut rows = conn
        .query(&sql, ::libsql::params_from_iter(params))
        .await
        .map_err(|e| crate::Error::engine(format!("get_callers/callees ({symbol_col}) query failed: {e}")))?;

    let mut out = Vec::new();
    loop {
        let next = rows
            .next()
            .await
            .map_err(|e| crate::Error::engine(format!("get_callers/callees ({symbol_col}) row fetch failed: {e}")))?;
        match next {
            Some(row) => out.push(code_edge_row_to_result(&row)?),
            None => break,
        }
    }
    Ok(out)
}

/// Map a libsql code-edge result row to the public `CodeEdgeResult` contract.
/// Column order matches the SELECTs in `code_edge_symbol_query` /
/// `get_edges_by_chunk`: id, from_chunk_id, to_chunk_id, from_symbol_qualified,
/// to_symbol_qualified, edge_type, edge_metadata, source_id, resolved.
fn code_edge_row_to_result(row: &::libsql::Row) -> crate::Result<crate::import::CodeEdgeResult> {
    let id: i64 = row
        .get(0)
        .map_err(|e| crate::Error::engine(format!("code_edge decode id: {e}")))?;
    let from_chunk_id: i64 = row
        .get(1)
        .map_err(|e| crate::Error::engine(format!("code_edge decode from_chunk_id: {e}")))?;
    let to_chunk_id: Option<i64> = row.get(2).unwrap_or(None);
    let from_symbol_qualified: String = row
        .get(3)
        .map_err(|e| crate::Error::engine(format!("code_edge decode from_symbol_qualified: {e}")))?;
    let to_symbol_qualified: String = row
        .get(4)
        .map_err(|e| crate::Error::engine(format!("code_edge decode to_symbol_qualified: {e}")))?;
    let edge_type: String = row
        .get(5)
        .map_err(|e| crate::Error::engine(format!("code_edge decode edge_type: {e}")))?;
    let meta_text: Option<String> = row.get(6).unwrap_or(None);
    let edge_metadata = meta_text
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let source_id: Option<String> = row.get(7).unwrap_or(None);
    let resolved_raw: i64 = row
        .get(8)
        .map_err(|e| crate::Error::engine(format!("code_edge decode resolved: {e}")))?;
    Ok(crate::import::CodeEdgeResult {
        id,
        from_chunk_id,
        to_chunk_id,
        from_symbol_qualified,
        to_symbol_qualified,
        edge_type,
        edge_metadata,
        source_id,
        resolved: resolved_raw != 0,
    })
}

// ─── 1-6-7-10-3: code-graph symbol queries (libsql) ────────────────────────

/// Definition-site symbol types, aligned with TS `DEF_TYPES` in
/// `src/commands/code-def.ts`. The list is a fixed, trusted literal set — it
/// is interpolated into the SQL `IN (...)` clause (no user input reaches it).
const CODE_DEF_TYPES: &[&str] = &[
    "function", "class", "interface", "type", "enum", "struct", "trait", "module", "contract",
    "table", "view", "index", "procedure", "schema", "database", "trigger", "export statement",
];

/// Mirror of TS `findCodeDef` (`src/commands/code-def.ts`): exact
/// `symbol_name` match, restricted to `symbol_type IN (DEF_TYPES)` on
/// `page_kind = 'code'` pages, joined to `pages` for slug + file.
async fn code_def_query(
    conn: &::libsql::Connection,
    symbol: &str,
    opts: &crate::import::CodeSymbolQueryOpts,
) -> Result<Vec<crate::import::CodeDefResult>> {
    let limit = (opts.limit.unwrap_or(20) as i64).min(500);
    let mut params: Vec<::libsql::Value> = vec![::libsql::Value::from(symbol.to_string())];
    let mut lang_clause = String::new();
    if let Some(lang) = &opts.language {
        lang_clause = " AND cc.language = ?2".to_string();
        params.push(::libsql::Value::from(lang.clone()));
    }
    let limit_ph = params.len() + 1;
    params.push(::libsql::Value::from(limit));

    let types_list = CODE_DEF_TYPES
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        "SELECT p.slug, json_extract(p.frontmatter, '$.file') AS file, cc.language, \
                cc.symbol_type, cc.start_line, cc.end_line, cc.chunk_text \
           FROM content_chunks cc \
           JOIN pages p ON p.id = cc.page_id \
          WHERE cc.symbol_name = ?1 \
            {lang} \
            AND p.page_kind = 'code' \
            AND cc.symbol_type IN ({types}) \
          ORDER BY CASE cc.symbol_type \
                     WHEN 'function' THEN 1 WHEN 'class' THEN 2 WHEN 'interface' THEN 3 \
                     WHEN 'type' THEN 4 WHEN 'enum' THEN 5 WHEN 'struct' THEN 6 \
                     ELSE 7 END, \
                   p.slug, cc.start_line \
          LIMIT ?{limit_ph}",
        lang = lang_clause,
        types = types_list,
        limit_ph = limit_ph,
    );

    let mut rows = conn
        .query(&sql, ::libsql::params_from_iter(params))
        .await
        .map_err(|e| Error::engine(format!("find_code_def query failed: {e}")))?;
    let mut out = Vec::new();
    loop {
        let next = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_code_def row fetch failed: {e}")))?;
        match next {
            Some(row) => out.push(code_def_row_to_result(&row)?),
            None => break,
        }
    }
    Ok(out)
}

/// Mirror of TS `findCodeRefs` (`src/commands/code-refs.ts`): `chunk_text
/// ILIKE '%symbol%'` over `page_kind = 'code'` pages, joined to `pages` for
/// slug + file. Returns every matching chunk (no DISTINCT ON page).
async fn code_ref_query(
    conn: &::libsql::Connection,
    symbol: &str,
    opts: &crate::import::CodeSymbolQueryOpts,
) -> Result<Vec<crate::import::CodeRefResult>> {
    let limit = (opts.limit.unwrap_or(50) as i64).min(500);
    let mut params: Vec<::libsql::Value> =
        vec![::libsql::Value::from(format!("%{symbol}%"))];
    let mut lang_clause = String::new();
    if let Some(lang) = &opts.language {
        lang_clause = format!(" AND cc.language = ?{}", params.len() + 1);
        params.push(::libsql::Value::from(lang.clone()));
    }
    let limit_ph = params.len() + 1;
    params.push(::libsql::Value::from(limit));

    let sql = format!(
        "SELECT p.slug, json_extract(p.frontmatter, '$.file') AS file, cc.language, \
                cc.symbol_name, cc.symbol_type, cc.start_line, cc.end_line, cc.chunk_text \
           FROM content_chunks cc \
           JOIN pages p ON p.id = cc.page_id \
          WHERE p.page_kind = 'code' \
            AND cc.chunk_text LIKE ?1 \
            {lang} \
          ORDER BY p.slug, cc.start_line \
          LIMIT ?{limit_ph}",
        lang = lang_clause,
        limit_ph = limit_ph,
    );

    let mut rows = conn
        .query(&sql, ::libsql::params_from_iter(params))
        .await
        .map_err(|e| Error::engine(format!("find_code_refs query failed: {e}")))?;
    let mut out = Vec::new();
    loop {
        let next = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("find_code_refs row fetch failed: {e}")))?;
        match next {
            Some(row) => out.push(code_ref_row_to_result(&row)?),
            None => break,
        }
    }
    Ok(out)
}

fn code_def_row_to_result(row: &::libsql::Row) -> crate::Result<crate::import::CodeDefResult> {
    let slug: String = row
        .get(0)
        .map_err(|e| crate::Error::engine(format!("code_def decode slug: {e}")))?;
    let file: Option<String> = row.get(1).unwrap_or(None);
    let language: Option<String> = row.get(2).unwrap_or(None);
    let symbol_type: Option<String> = row.get(3).unwrap_or(None);
    let start_line: Option<i64> = row.get(4).unwrap_or(None);
    let end_line: Option<i64> = row.get(5).unwrap_or(None);
    let chunk_text: String = row
        .get(6)
        .map_err(|e| crate::Error::engine(format!("code_def decode chunk_text: {e}")))?;
    Ok(crate::import::CodeDefResult {
        slug,
        file,
        language,
        symbol_type,
        start_line,
        end_line,
        snippet: chunk_text.chars().take(500).collect(),
    })
}

fn code_ref_row_to_result(row: &::libsql::Row) -> crate::Result<crate::import::CodeRefResult> {
    let slug: String = row
        .get(0)
        .map_err(|e| crate::Error::engine(format!("code_ref decode slug: {e}")))?;
    let file: Option<String> = row.get(1).unwrap_or(None);
    let language: Option<String> = row.get(2).unwrap_or(None);
    let symbol_name: Option<String> = row.get(3).unwrap_or(None);
    let symbol_type: Option<String> = row.get(4).unwrap_or(None);
    let start_line: Option<i64> = row.get(5).unwrap_or(None);
    let end_line: Option<i64> = row.get(6).unwrap_or(None);
    let chunk_text: String = row
        .get(7)
        .map_err(|e| crate::Error::engine(format!("code_ref decode chunk_text: {e}")))?;
    Ok(crate::import::CodeRefResult {
        slug,
        file,
        language,
        symbol_name,
        symbol_type,
        start_line,
        end_line,
        snippet: chunk_text.chars().take(500).collect(),
    })
}

/// 1-6-7-10-4 符号消歧，对齐 TS `disambiguateSymbol`
/// (`src/core/code-intel/recursive-walk.ts:77`)。
///
/// 阶段一（精确）：`symbol_name = bare OR symbol_name_qualified = bare` 取
/// `DISTINCT symbol_name_qualified`（LIMIT 25）。有命中即返回 `matches`。
/// 阶段二（近似）：仅在无精确命中时，按 `symbol_name_qualified LIKE '%bare%'`
/// （SQLite `LIKE` 对 ASCII 默认大小写不敏感，等价 TS `ILIKE`）取
/// `did_you_mean` 候选（LIMIT 5）。两阶段均限定 `pages.source_id` 且
/// `symbol_name_qualified IS NOT NULL`。
async fn code_disambiguate_query(
    conn: &::libsql::Connection,
    bare: &str,
    source_id: &str,
) -> Result<crate::import::SymbolDisambiguation> {
    let exact_sql = "SELECT DISTINCT cc.symbol_name_qualified \
                       FROM content_chunks cc \
                       JOIN pages p ON p.id = cc.page_id \
                      WHERE p.source_id = ?1 \
                        AND cc.symbol_name_qualified IS NOT NULL \
                        AND (cc.symbol_name = ?2 OR cc.symbol_name_qualified = ?2) \
                      LIMIT 25";
    let mut rows = conn
        .query(exact_sql, libsql::params![source_id, bare])
        .await
        .map_err(|e| Error::engine(format!("disambiguate (exact) query failed: {e}")))?;
    let mut matches: Vec<String> = Vec::new();
    loop {
        let next = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("disambiguate (exact) row fetch failed: {e}")))?;
        match next {
            Some(row) => {
                let q: Option<String> = row.get(0).unwrap_or(None);
                if let Some(q) = q {
                    matches.push(q);
                }
            }
            None => break,
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
                      WHERE p.source_id = ?1 \
                        AND cc.symbol_name_qualified IS NOT NULL \
                        AND cc.symbol_name_qualified LIKE ?2 \
                      LIMIT 5";
    let mut rows = conn
        .query(fuzzy_sql, libsql::params![source_id, like])
        .await
        .map_err(|e| Error::engine(format!("disambiguate (fuzzy) query failed: {e}")))?;
    let mut suggestions: Vec<String> = Vec::new();
    loop {
        let next = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("disambiguate (fuzzy) row fetch failed: {e}")))?;
        match next {
            Some(row) => {
                let q: Option<String> = row.get(0).unwrap_or(None);
                if let Some(q) = q {
                    suggestions.push(q);
                }
            }
            None => break,
        }
    }
    Ok(crate::import::SymbolDisambiguation {
        matches: Vec::new(),
        suggestions,
    })
}

/// 1-6-7-10-5 递归遍历 BFS，对齐 TS `runRecursiveWalk`。
///
/// This implementation:
/// 1. Re-uses `disambiguate_symbol` for starting symbol resolution
/// 2. Performs BFS in Rust (doesn't try to do it recursively in SQL)
/// 3. For each frontier node, queries `get_callers_of` / `get_callees_of` using the
///    existing `code_edge_symbol_query` helper that's already implemented.
/// 4. Same cycle detection, truncation, confidence calculation as InMemory.
async fn code_recursive_walk_query(
    engine: &LibsqlEngine,
    conn: &::libsql::Connection,
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
        let disambig = code_disambiguate_query(conn, symbol, source_id).await?;
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

    // Step 2: language gate — get starting symbol's language from content_chunks
    let start_lang = 'find_lang: {
        let sql = "SELECT cc.language \
                     FROM content_chunks cc \
                     JOIN pages p ON p.id = cc.page_id \
                    WHERE p.source_id = ?1 \
                      AND cc.symbol_name_qualified = ?2 \
                    LIMIT 1";
        let mut rows = conn
            .query(sql, libsql::params![source_id, qualified_start.clone()])
            .await
            .map_err(|e| Error::engine(format!("recursive-walk language lookup failed: {e}")))?;
        while let Some(row) = rows.next().await.map_err(|e| Error::engine(e.to_string()))? {
            let lang: Option<String> = row.get(0).unwrap_or(None);
            break 'find_lang lang;
        }
        None
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
            let edges = code_edge_symbol_query(
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

// ─── libsql facts helpers ─────────────────────────────────────────────────

/// Map a libsql Row to a FactRow. Positional column order must match
/// the SELECT in `list_facts_by_entity`.
fn row_to_fact(row: &::libsql::Row) -> Result<FactRow> {
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
        id: row.get(0).map_err(|e| Error::engine(format!("fact id: {e}")))?,
        source_id: row.get(1).map_err(|e| Error::engine(format!("fact source_id: {e}")))?,
        entity_slug: row.get(2).map_err(|e| Error::engine(format!("fact entity_slug: {e}")))?,
        fact: row.get(3).map_err(|e| Error::engine(format!("fact fact: {e}")))?,
        kind: {
            let s: String =
                row.get(4)
                    .map_err(|e| Error::engine(format!("fact kind: {e}")))?;
            parse_kind(&s)
        },
        visibility: {
            let s: String =
                row.get(5)
                    .map_err(|e| Error::engine(format!("fact visibility: {e}")))?;
            parse_visibility(&s)
        },
        notability: row
            .get(6)
            .map_err(|e| Error::engine(format!("fact notability: {e}")))?,
        context: row
            .get(7)
            .map_err(|e| Error::engine(format!("fact context: {e}")))?,
        valid_from: row
            .get(8)
            .map_err(|e| Error::engine(format!("fact valid_from: {e}")))?,
        valid_until: row
            .get(9)
            .map_err(|e| Error::engine(format!("fact valid_until: {e}")))?,
        expired_at: row
            .get(10)
            .map_err(|e| Error::engine(format!("fact expired_at: {e}")))?,
        superseded_by: row
            .get(11)
            .map_err(|e| Error::engine(format!("fact superseded_by: {e}")))?,
        consolidated_at: row
            .get(12)
            .map_err(|e| Error::engine(format!("fact consolidated_at: {e}")))?,
        consolidated_into: row
            .get(13)
            .map_err(|e| Error::engine(format!("fact consolidated_into: {e}")))?,
        source: row
            .get(14)
            .map_err(|e| Error::engine(format!("fact source: {e}")))?,
        source_session: row
            .get(15)
            .map_err(|e| Error::engine(format!("fact source_session: {e}")))?,
        confidence: row
            .get(16)
            .map_err(|e| Error::engine(format!("fact confidence: {e}")))?,
        created_at: row
            .get(17)
            .map_err(|e| Error::engine(format!("fact created_at: {e}")))?,
        row_num: row.get(18).map_err(|e| Error::engine(format!("fact row_num: {e}")))?,
        source_markdown_slug: row
            .get(19)
            .map_err(|e| Error::engine(format!("fact source_markdown_slug: {e}")))?,
    })
}

// ─── libsql extract_facts cycle-phase helpers (1-1-1) ─────────────────────────

impl LibsqlEngine {
    async fn delete_facts_for_page_impl(
        &self,
        slug: &str,
        source_id: &str,
    ) -> Result<i64> {
        let conn = self.conn().await?;
        let res = conn
            .execute(
                "DELETE FROM facts WHERE source_markdown_slug = ?1 AND source_id = ?2",
                ::libsql::params![slug, source_id],
            )
            .await
            .map_err(|e| Error::engine(format!("delete_facts_for_page: {e}")))?;
        Ok(res as i64)
    }

    async fn count_legacy_fact_rows_impl(&self) -> Result<i64> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM facts WHERE row_num IS NULL AND entity_slug IS NOT NULL",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("count_legacy_fact_rows: {e}")))?;
        let count: i64 = match rows.next().await {
            Ok(Some(row)) => row
                .get(0)
                .map_err(|e| Error::engine(format!("count_legacy_fact_rows count: {e}")))?,
            Ok(None) => 0,
            Err(e) => return Err(Error::engine(format!("count_legacy_fact_rows row: {e}"))),
        };
        Ok(count)
    }
}

// ─── libsql minion job helpers ─────────────────────────────────────────────

/// Column list for `minion_jobs` SELECTs. The positional order here MUST match
/// the decode indices in [`libsql_row_to_job`].
const MINION_JOB_COLUMNS: &str = "id, name, queue, status, priority, data, \
    max_attempts, attempts_made, attempts_started, backoff_type, backoff_delay, \
    backoff_jitter, stalled_counter, max_stalled, lock_token, lock_until, \
    delay_until, parent_job_id, on_child_fail, tokens_input, tokens_output, \
    tokens_cache_read, depth, max_children, timeout_ms, timeout_at, \
    remove_on_complete, remove_on_fail, idempotency_key, quiet_hours, stagger_key, \
    result, progress, error_text, stacktrace, created_at, started_at, finished_at, \
    updated_at";

/// Fetch `last_insert_rowid()` on the given connection.
async fn last_insert_rowid(conn: &::libsql::Connection) -> Result<i64> {
    let mut rows = conn
        .query("SELECT last_insert_rowid()", ())
        .await
        .map_err(|e| Error::engine(format!("last_insert_rowid query: {e}")))?;
    let row = rows
        .next()
        .await
        .map_err(|e| Error::engine(format!("last_insert_rowid row: {e}")))?
        .ok_or_else(|| Error::engine("last_insert_rowid returned no row"))?;
    row.get::<i64>(0)
        .map_err(|e| Error::engine(format!("last_insert_rowid decode: {e}")))
}

/// Map a `minion_jobs` row to a [`MinionJob`]. Positional column order MUST
/// match [`MINION_JOB_COLUMNS`]. SQLite stores bools as INTEGER 0/1, JSON as
/// TEXT, and scheduling columns as INTEGER epoch-ms (already the target type).
fn libsql_row_to_job(row: &::libsql::Row) -> Result<crate::minions::types::MinionJob> {
    use crate::minions::types::{BackoffType, ChildFailPolicy, MinionJob, MinionJobStatus};

    macro_rules! get {
        ($idx:expr, $ty:ty, $name:literal) => {
            row.get::<$ty>($idx)
                .map_err(|e| Error::engine(format!(concat!("job decode ", $name, ": {}"), e)))?
        };
    }

    // JSON TEXT -> Value; malformed text degrades to the given default rather
    // than failing the whole decode.
    fn json_or<'a>(
        row: &::libsql::Row,
        idx: i32,
        default: fn() -> serde_json::Value,
    ) -> serde_json::Value {
        match row.get::<Option<String>>(idx) {
            Ok(Some(s)) => serde_json::from_str(&s).unwrap_or_else(|_| default()),
            _ => default(),
        }
    }

    let status_str = get!(3, String, "status");
    let backoff_str = get!(9, String, "backoff_type");
    let on_child_fail_str = get!(18, String, "on_child_fail");

    let stacktrace: Vec<String> = match row.get::<Option<String>>(34) {
        Ok(Some(s)) => serde_json::from_str(&s).unwrap_or_default(),
        _ => Vec::new(),
    };

    Ok(MinionJob {
        id: get!(0, i64, "id"),
        name: get!(1, String, "name"),
        queue: get!(2, String, "queue"),
        status: MinionJobStatus::parse(&status_str)
            .ok_or_else(|| Error::engine(format!("job decode status: unknown '{status_str}'")))?,
        priority: get!(4, i64, "priority") as i32,
        data: json_or(row, 5, || serde_json::json!({})),
        max_attempts: get!(6, i64, "max_attempts") as i32,
        attempts_made: get!(7, i64, "attempts_made") as i32,
        attempts_started: get!(8, i64, "attempts_started") as i32,
        backoff_type: BackoffType::parse(&backoff_str).ok_or_else(|| {
            Error::engine(format!("job decode backoff_type: unknown '{backoff_str}'"))
        })?,
        backoff_delay: get!(10, i64, "backoff_delay") as i32,
        backoff_jitter: get!(11, f64, "backoff_jitter"),
        stalled_counter: get!(12, i64, "stalled_counter") as i32,
        max_stalled: get!(13, i64, "max_stalled") as i32,
        lock_token: get!(14, Option<String>, "lock_token"),
        lock_until: get!(15, Option<i64>, "lock_until"),
        delay_until: get!(16, Option<i64>, "delay_until"),
        parent_job_id: get!(17, Option<i64>, "parent_job_id"),
        on_child_fail: ChildFailPolicy::parse(&on_child_fail_str).ok_or_else(|| {
            Error::engine(format!(
                "job decode on_child_fail: unknown '{on_child_fail_str}'"
            ))
        })?,
        tokens_input: get!(19, i64, "tokens_input"),
        tokens_output: get!(20, i64, "tokens_output"),
        tokens_cache_read: get!(21, i64, "tokens_cache_read"),
        depth: get!(22, i64, "depth") as i32,
        max_children: get!(23, Option<i64>, "max_children").map(|v| v as i32),
        timeout_ms: get!(24, Option<i64>, "timeout_ms"),
        timeout_at: get!(25, Option<i64>, "timeout_at"),
        remove_on_complete: get!(26, i64, "remove_on_complete") != 0,
        remove_on_fail: get!(27, i64, "remove_on_fail") != 0,
        idempotency_key: get!(28, Option<String>, "idempotency_key"),
        quiet_hours: match row.get::<Option<String>>(29) {
            Ok(Some(s)) => serde_json::from_str(&s).ok(),
            _ => None,
        },
        stagger_key: get!(30, Option<String>, "stagger_key"),
        result: match row.get::<Option<String>>(31) {
            Ok(Some(s)) => serde_json::from_str(&s).ok(),
            _ => None,
        },
        progress: match row.get::<Option<String>>(32) {
            Ok(Some(s)) => serde_json::from_str(&s).ok(),
            _ => None,
        },
        error_text: get!(33, Option<String>, "error_text"),
        stacktrace,
        created_at: get!(35, String, "created_at"),
        started_at: get!(36, Option<String>, "started_at"),
        finished_at: get!(37, Option<String>, "finished_at"),
        updated_at: get!(38, String, "updated_at"),
    })
}

/// Fetch a single job by id on a caller-supplied connection. Same decode as
/// [`LibsqlEngine::get_job`], but takes the connection so it can run INSIDE an
/// open transaction (reading a row the same txn just UPDATE'd). The method form
/// acquires a fresh connection, which wouldn't see uncommitted writes.
async fn libsql_get_job(
    conn: &::libsql::Connection,
    id: i64,
) -> Result<Option<crate::minions::types::MinionJob>> {
    let mut rows = conn
        .query(
            &format!("SELECT {MINION_JOB_COLUMNS} FROM minion_jobs WHERE id = ?1"),
            ::libsql::params![id],
        )
        .await
        .map_err(|e| Error::engine(format!("libsql_get_job SELECT: {e}")))?;
    match rows
        .next()
        .await
        .map_err(|e| Error::engine(format!("libsql_get_job row: {e}")))?
    {
        Some(row) => Ok(Some(libsql_row_to_job(&row)?)),
        None => Ok(None),
    }
}

/// Fetch a single inbox message by id and decode it to [`InboxMessage`]. Used
/// by read_inbox to reselect the rows it just marked read (SQLite has no
/// UPDATE ... RETURNING in this build).
async fn libsql_get_inbox_message(
    conn: &::libsql::Connection,
    id: i64,
) -> Result<Option<crate::minions::types::InboxMessage>> {
    let mut rows = conn
        .query(
            "SELECT id, job_id, sender, payload, sent_at, read_at \
             FROM minion_inbox WHERE id = ?1",
            ::libsql::params![id],
        )
        .await
        .map_err(|e| Error::engine(format!("get_inbox_message SELECT: {e}")))?;
    match rows
        .next()
        .await
        .map_err(|e| Error::engine(format!("get_inbox_message row: {e}")))?
    {
        Some(row) => {
            let payload_str: String = row
                .get::<String>(3)
                .map_err(|e| Error::engine(format!("inbox payload decode: {e}")))?;
            let payload: serde_json::Value =
                serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);
            Ok(Some(crate::minions::types::InboxMessage {
                id: row
                    .get::<i64>(0)
                    .map_err(|e| Error::engine(format!("inbox id decode: {e}")))?,
                job_id: row
                    .get::<i64>(1)
                    .map_err(|e| Error::engine(format!("inbox job_id decode: {e}")))?,
                sender: row
                    .get::<String>(2)
                    .map_err(|e| Error::engine(format!("inbox sender decode: {e}")))?,
                payload,
                sent_at: row
                    .get::<String>(4)
                    .map_err(|e| Error::engine(format!("inbox sent_at decode: {e}")))?,
                read_at: row
                    .get::<Option<String>>(5)
                    .map_err(|e| Error::engine(format!("inbox read_at decode: {e}")))?,
            }))
        }
        None => Ok(None),
    }
}

/// Map a libsql row to an [`Attachment`](crate::minions::types::Attachment)
/// metadata struct. Positional columns must match the SELECT:
/// `id, job_id, filename, content_type, storage_uri, size_bytes, sha256, created_at`.
/// An empty `storage_uri` string is normalized to `None` (mirrors the TS
/// `rowToAttachment` `(row.storage_uri as string) || null`).
fn libsql_row_to_attachment(
    row: &::libsql::Row,
) -> Result<crate::minions::types::Attachment> {
    let storage_uri = row
        .get::<Option<String>>(4)
        .map_err(|e| Error::engine(format!("attachment storage_uri: {e}")))?
        .filter(|s| !s.is_empty());
    Ok(crate::minions::types::Attachment {
        id: row
            .get::<i64>(0)
            .map_err(|e| Error::engine(format!("attachment id: {e}")))?,
        job_id: row
            .get::<i64>(1)
            .map_err(|e| Error::engine(format!("attachment job_id: {e}")))?,
        filename: row
            .get::<String>(2)
            .map_err(|e| Error::engine(format!("attachment filename: {e}")))?,
        content_type: row
            .get::<String>(3)
            .map_err(|e| Error::engine(format!("attachment content_type: {e}")))?,
        storage_uri,
        size_bytes: row
            .get::<i64>(5)
            .map_err(|e| Error::engine(format!("attachment size_bytes: {e}")))?,
        sha256: row
            .get::<String>(6)
            .map_err(|e| Error::engine(format!("attachment sha256: {e}")))?,
        created_at: row
            .get::<String>(7)
            .map_err(|e| Error::engine(format!("attachment created_at: {e}")))?,
    })
}
/// parent is still non-terminal (the SQL `WHERE EXISTS(... NOT IN terminal)`
/// guard). A no-op INSERT if the parent already finished, which is why callers
/// on the fail path must emit BEFORE flipping the parent terminal.
async fn libsql_emit_child_done(
    conn: &::libsql::Connection,
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
        .map_err(|e| Error::engine(format!("child_done serialize: {e}")))?
        .to_string();
    let now_iso = current_utc_iso8601();
    conn.execute(
        "INSERT INTO minion_inbox (job_id, sender, payload, sent_at) \
         SELECT ?1, 'minions', ?2, ?3 \
         WHERE EXISTS (SELECT 1 FROM minion_jobs \
             WHERE id = ?1 AND status NOT IN \
                ('completed','failed','dead','cancelled'))",
        ::libsql::params![parent_id, payload, now_iso],
    )
    .await
    .map_err(|e| Error::engine(format!("emit_child_done INSERT: {e}")))?;
    Ok(())
}

/// Flip a parent out of `waiting-children` back to `waiting` once none of its
/// children remain non-terminal (the SQL `resolve_parent` UPDATE). No-op unless
/// the parent is waiting-children and all its kids are terminal.
async fn libsql_resolve_parent(
    conn: &::libsql::Connection,
    parent_id: i64,
    now_iso: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE minion_jobs SET status = 'waiting', updated_at = ?1 \
         WHERE id = ?2 AND status = 'waiting-children' \
           AND NOT EXISTS (SELECT 1 FROM minion_jobs child \
               WHERE child.parent_job_id = ?2 AND child.status NOT IN \
                  ('completed','failed','dead','cancelled'))",
        ::libsql::params![now_iso, parent_id],
    )
    .await
    .map_err(|e| Error::engine(format!("resolve_parent UPDATE: {e}")))?;
    Ok(())
}

/// Reselect a set of jobs by id and decode them to [`MinionJob`]. Used by the
/// background sweeps to return the post-mutation rows. Ids not found are
/// silently skipped; the result order follows `ids`.
async fn libsql_reselect_jobs(
    conn: &::libsql::Connection,
    ids: &[i64],
) -> Result<Vec<crate::minions::types::MinionJob>> {
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        let mut rows = conn
            .query(
                &format!("SELECT {MINION_JOB_COLUMNS} FROM minion_jobs WHERE id = ?1"),
                ::libsql::params![id],
            )
            .await
            .map_err(|e| Error::engine(format!("reselect job {id}: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("reselect job {id} row: {e}")))?
        {
            out.push(libsql_row_to_job(&row)?);
        }
    }
    Ok(out)
}

/// SELECT the ids of rows matching a WHERE clause. Small helper so the sweeps
/// can capture the affected set before mutating (SQLite has no `UPDATE ...
/// RETURNING` in the libsql build used here).
async fn libsql_select_ids(
    conn: &::libsql::Connection,
    sql: &str,
    params: impl ::libsql::params::IntoParams,
) -> Result<Vec<i64>> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|e| Error::engine(format!("select ids: {e}")))?;
    let mut ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| Error::engine(format!("select ids row: {e}")))?
    {
        ids.push(
            row.get::<i64>(0)
                .map_err(|e| Error::engine(format!("select ids decode: {e}")))?,
        );
    }
    Ok(ids)
}

/// The SELECT→UPDATE claim body, run inside a `BEGIN IMMEDIATE` transaction.
/// Picks the highest-priority (lowest number), oldest waiting job in `queue`
/// whose name is in `registered_names`, then flips it to active with the
/// worker's lock. Returns `None` when nothing is claimable.
async fn claim_job_locked(
    conn: &::libsql::Connection,
    lock_token: &str,
    lock_duration_ms: i64,
    queue: &str,
    registered_names: &[String],
) -> Result<Option<crate::minions::types::MinionJob>> {
    // Build the `name IN (?, ?, ...)` placeholder list.
    let placeholders = registered_names
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!(
        "SELECT id FROM minion_jobs \
         WHERE queue = ? AND status = 'waiting' AND name IN ({placeholders}) \
         ORDER BY priority ASC, created_at ASC, id ASC LIMIT 1"
    );

    let mut params: Vec<::libsql::Value> = Vec::with_capacity(registered_names.len() + 1);
    params.push(queue.into());
    for n in registered_names {
        params.push(n.clone().into());
    }

    let mut rows = conn
        .query(&select_sql, ::libsql::params::Params::Positional(params))
        .await
        .map_err(|e| Error::engine(format!("claim_job SELECT: {e}")))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|e| Error::engine(format!("claim_job SELECT row: {e}")))?
    else {
        return Ok(None);
    };
    let id: i64 = row
        .get(0)
        .map_err(|e| Error::engine(format!("claim_job id decode: {e}")))?;
    // Drop the ResultSet borrow before the UPDATE reuses `conn`.
    drop(rows);

    let now_iso = current_utc_iso8601();
    let now_ms = crate::time::now_epoch_ms();
    let lock_until = now_ms + lock_duration_ms;

    conn.execute(
        "UPDATE minion_jobs SET \
            status = 'active', \
            lock_token = ?1, \
            lock_until = ?2, \
            timeout_at = CASE WHEN timeout_ms IS NOT NULL THEN ?3 + timeout_ms ELSE NULL END, \
            attempts_started = attempts_started + 1, \
            started_at = COALESCE(started_at, ?4), \
            updated_at = ?4 \
         WHERE id = ?5",
        ::libsql::params![lock_token, lock_until, now_ms, now_iso, id],
    )
    .await
    .map_err(|e| Error::engine(format!("claim_job UPDATE: {e}")))?;

    let mut rows = conn
        .query(
            &format!("SELECT {MINION_JOB_COLUMNS} FROM minion_jobs WHERE id = ?1"),
            ::libsql::params![id],
        )
        .await
        .map_err(|e| Error::engine(format!("claim_job reselect: {e}")))?;
    match rows
        .next()
        .await
        .map_err(|e| Error::engine(format!("claim_job reselect row: {e}")))?
    {
        Some(row) => Ok(Some(libsql_row_to_job(&row)?)),
        None => Ok(None),
    }
}

// ─── AdminQueries impl ────────────────────────────────────────────────────

/// Current Unix timestamp (seconds).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Midnight UTC today as a Unix timestamp (seconds since epoch).
fn today_utc_unix() -> i64 {
    let now = now_unix() as i64;
    now - (now % 86400)
}

#[async_trait]
impl AdminQueries for LibsqlEngine {
    async fn get_stats(&self) -> Result<Stats> {
        let conn = self.conn().await?;
        let today = current_utc_iso8601().chars().take(10).collect::<String>();

        // connected_agents: active oauth clients with registered tokens
        let connected_agents: i64 = conn
            .query(
                "SELECT COUNT(DISTINCT c.client_id) FROM oauth_clients c
                 JOIN oauth_tokens t ON t.client_id = c.client_id
                 WHERE c.deleted_at IS NULL AND t.expires_at > ?1",
                ::libsql::params![now_unix() as i64],
            )
            .await
            .map_err(|e| Error::engine(format!("get_stats connected_agents: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_stats connected_agents row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("get_stats connected_agents decode: {e}")))?
            .unwrap_or(0);

        // active_tokens: active oauth tokens
        let active_tokens: i64 = conn
            .query(
                "SELECT COUNT(*) FROM oauth_tokens WHERE expires_at > ?1",
                ::libsql::params![now_unix() as i64],
            )
            .await
            .map_err(|e| Error::engine(format!("get_stats active_tokens: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_stats active_tokens row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("get_stats active_tokens decode: {e}")))?
            .unwrap_or(0);

        // active_api_keys: non-revoked access_tokens
        let active_api_keys: i64 = conn
            .query(
                "SELECT COUNT(*) FROM access_tokens WHERE revoked_at IS NULL",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("get_stats active_api_keys: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_stats active_api_keys row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("get_stats active_api_keys decode: {e}")))?
            .unwrap_or(0);

        // requests_today: count of mcp_request_log today
        let requests_today: i64 = conn
            .query(
                "SELECT COUNT(*) FROM mcp_request_log WHERE created_at >= ?1",
                ::libsql::params![format!("{today}T00:00:00")],
            )
            .await
            .map_err(|e| Error::engine(format!("get_stats requests_today: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_stats requests_today row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("get_stats requests_today decode: {e}")))?
            .unwrap_or(0);

        Ok(Stats {
            connected_agents,
            active_tokens,
            active_api_keys,
            requests_today,
        })
    }

    async fn get_full_stats(&self) -> Result<FullStats> {
        let conn = self.conn().await?;

        let page_count: i64 = conn
            .query(
                "SELECT COUNT(*) FROM pages WHERE deleted_at IS NULL",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("get_full_stats page_count: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_full_stats page_count row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("get_full_stats page_count decode: {e}")))?
            .unwrap_or(0);

        // chunk_count: rough estimate — pages with non-trivial compiled_truth
        let chunk_count: i64 = conn
            .query(
                "SELECT COUNT(*) FROM pages WHERE compiled_truth IS NOT NULL AND compiled_truth != '' AND deleted_at IS NULL",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("get_full_stats chunk_count: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_full_stats chunk_count row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("get_full_stats chunk_count decode: {e}")))?
            .unwrap_or(0);

        Ok(FullStats {
            page_count,
            chunk_count,
            engine_ok: true,
        })
    }

    async fn check_health_indicators(&self) -> Result<HealthIndicators> {
        let conn = self.conn().await?;
        let now = now_unix() as i64;
        // Warnings start 72h before expiry
        let warning_cutoff = now + 72 * 3600;

        let expiring_soon: i64 = conn
            .query(
                "SELECT COUNT(*) FROM oauth_tokens WHERE expires_at > ?1 AND expires_at < ?2",
                ::libsql::params![now, warning_cutoff],
            )
            .await
            .map_err(|e| Error::engine(format!("health_indicators expiring: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("health_indicators expiring row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("health_indicators expiring decode: {e}")))?
            .unwrap_or(0);

        // error_rate: ratio of error requests in past 24h
        let total_24h: i64 = conn
            .query(
                "SELECT COUNT(*) FROM mcp_request_log WHERE created_at >= datetime('now', '-1 day')",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("health_indicators total: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("health_indicators total row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("health_indicators total decode: {e}")))?
            .unwrap_or(0);

        let error_count: i64 = if total_24h > 0 {
            conn.query(
                "SELECT COUNT(*) FROM mcp_request_log WHERE created_at >= datetime('now', '-1 day') AND status = 'error'",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("health_indicators errors: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("health_indicators errors row: {e}")))?
            .map(|r| r.get::<i64>(0))
            .transpose()
            .map_err(|e| Error::engine(format!("health_indicators errors decode: {e}")))?
            .unwrap_or(0)
        } else {
            0
        };

        #[allow(clippy::cast_precision_loss)]
        let error_rate = if total_24h > 0 {
            error_count as f64 / total_24h as f64
        } else {
            0.0
        };

        Ok(HealthIndicators {
            expiring_soon,
            error_rate,
        })
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>> {
        let conn = self.conn().await?;

        // Union: OAuth clients with active tokens + legacy API keys
        let mut rows = conn
            .query(
                "SELECT c.client_id, c.client_name, 'oauth' as auth_type,
                        MAX(t.created_at) as last_used_at,
                        NULL as method
                 FROM oauth_clients c
                 LEFT JOIN oauth_tokens t ON t.client_id = c.client_id
                 WHERE c.deleted_at IS NULL
                 GROUP BY c.client_id, c.client_name
                 UNION ALL
                 SELECT a.id::text, a.name, 'api_key' as auth_type,
                        a.last_used_at,
                        NULL as method
                 FROM access_tokens a
                 WHERE a.revoked_at IS NULL
                 ORDER BY auth_type, client_name",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("list_agents: {e}")))?;

        let mut agents = Vec::new();
        loop {
            let next = rows.next().await
                .map_err(|e| Error::engine(format!("list_agents row: {e}")))?;
            match next {
                Some(row) => {
                    agents.push(AgentInfo {
                        id: row.get::<String>(0)
                            .map_err(|e| Error::engine(format!("list_agents id: {e}")))?,
                        name: row.get::<String>(1)
                            .map_err(|e| Error::engine(format!("list_agents name: {e}")))?,
                        auth_type: row.get::<String>(2)
                            .map_err(|e| Error::engine(format!("list_agents auth_type: {e}")))?,
                        last_used_at: row.get::<Option<String>>(3)
                            .map_err(|e| Error::engine(format!("list_agents last_used_at: {e}")))?,
                        method: row.get::<Option<String>>(4)
                            .map_err(|e| Error::engine(format!("list_agents method: {e}")))?,
                    });
                }
                None => break,
            }
        }
        Ok(agents)
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT id::text, name, created_at, last_used_at, revoked_at
                 FROM access_tokens
                 WHERE revoked_at IS NULL
                 ORDER BY created_at DESC",
                (),
            )
            .await
            .map_err(|e| Error::engine(format!("list_api_keys: {e}")))?;

        let mut keys = Vec::new();
        loop {
            let next = rows.next().await
                .map_err(|e| Error::engine(format!("list_api_keys row: {e}")))?;
            match next {
                Some(row) => {
                    keys.push(ApiKey {
                        id: row.get::<String>(0)
                            .map_err(|e| Error::engine(format!("list_api_keys id: {e}")))?,
                        name: row.get::<String>(1)
                            .map_err(|e| Error::engine(format!("list_api_keys name: {e}")))?,
                        created_at: row.get::<String>(2)
                            .map_err(|e| Error::engine(format!("list_api_keys created_at: {e}")))?,
                        last_used_at: row.get::<Option<String>>(3)
                            .map_err(|e| Error::engine(format!("list_api_keys last_used_at: {e}")))?,
                        revoked_at: row.get::<Option<String>>(4)
                            .map_err(|e| Error::engine(format!("list_api_keys revoked_at: {e}")))?,
                    });
                }
                None => break,
            }
        }
        Ok(keys)
    }

    async fn create_api_key(&self, name: &str) -> Result<ApiKey> {
        let conn = self.conn().await?;
        let now = current_utc_iso8601();
        // Generate a UUID-like id via SQLite's randomblob + hex
        let mut rows = conn
            .query("SELECT hex(randomblob(16))", ())
            .await
            .map_err(|e| Error::engine(format!("create_api_key random id: {e}")))?;
        let id: String = rows.next().await
            .map_err(|e| Error::engine(format!("create_api_key random id row: {e}")))?
            .and_then(|r| r.get::<String>(0).ok())
            .unwrap_or_else(|| format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));

        conn.execute(
            "INSERT INTO access_tokens (id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            ::libsql::params![id.clone(), name, "pending", now.clone()],
        )
        .await
        .map_err(|e| Error::engine(format!("create_api_key insert: {e}")))?;

        Ok(ApiKey {
            id,
            name: name.to_string(),
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        })
    }

    async fn revoke_api_key(&self, name: &str) -> Result<()> {
        let conn = self.conn().await?;
        let now = current_utc_iso8601();
        conn.execute(
            "UPDATE access_tokens SET revoked_at = ?1 WHERE name = ?2 AND revoked_at IS NULL",
            ::libsql::params![now, name],
        )
        .await
        .map_err(|e| Error::engine(format!("revoke_api_key update: {e}")))?;
        Ok(())
    }

    async fn list_requests(&self, filters: &RequestLogFilters) -> Result<Paginated<RequestLogEntry>> {
        let conn = self.conn().await?;

        // Count total with filters
        let (where_clause, params) = build_request_filters_sql(filters);
        let count_sql = format!("SELECT COUNT(*) FROM mcp_request_log {where_clause}");
        let total: u64 = conn
            .query(&count_sql, params.clone())
            .await
            .map_err(|e| Error::engine(format!("list_requests count: {e}")))?
            .next()
            .await
            .map_err(|e| Error::engine(format!("list_requests count row: {e}")))?
            .map(|r| {
                let raw: i64 = r.get(0).unwrap_or(0);
                raw as u64
            })
            .unwrap_or(0);

        // Fetch page
        let offset = filters.offset();
        let limit_plus_one = filters.limit() as i64 + 1;
        let data_sql = format!(
            "SELECT id, token_name, agent_name, operation, latency_ms, status, error_message, created_at
             FROM mcp_request_log {where_clause}
             ORDER BY created_at DESC
             LIMIT {limit_plus_one} OFFSET {offset}"
        );

        let mut rows = conn
            .query(&data_sql, params)
            .await
            .map_err(|e| Error::engine(format!("list_requests query: {e}")))?;

        let mut items = Vec::new();
        loop {
            let next = rows.next().await
                .map_err(|e| Error::engine(format!("list_requests row: {e}")))?;
            match next {
                Some(row) => {
                    items.push(RequestLogEntry {
                        id: row.get::<i64>(0)
                            .map_err(|e| Error::engine(format!("list_requests id: {e}")))?,
                        token_name: row.get::<Option<String>>(1)
                            .map_err(|e| Error::engine(format!("list_requests token_name: {e}")))?,
                        agent_name: row.get::<Option<String>>(2)
                            .map_err(|e| Error::engine(format!("list_requests agent_name: {e}")))?,
                        operation: row.get::<String>(3)
                            .map_err(|e| Error::engine(format!("list_requests operation: {e}")))?,
                        latency_ms: row.get::<Option<i64>>(4)
                            .map_err(|e| Error::engine(format!("list_requests latency_ms: {e}")))?,
                        status: row.get::<String>(5)
                            .map_err(|e| Error::engine(format!("list_requests status: {e}")))?,
                        error_message: row.get::<Option<String>>(6)
                            .map_err(|e| Error::engine(format!("list_requests error_message: {e}")))?,
                        created_at: row.get::<String>(7)
                            .map_err(|e| Error::engine(format!("list_requests created_at: {e}")))?,
                    });
                }
                None => break,
            }
        }

        Ok(Paginated {
            items,
            total,
            page: filters.page(),
            limit: filters.limit(),
        })
    }

    async fn list_agent_client_spend(&self) -> Result<Vec<AgentClientSpend>> {
        let conn = self.conn().await?;

        // Graceful degradation: if oauth_clients or mcp_spend_log tables
        // don't exist yet (no Rust migration), return empty vec.
        let result = conn
            .query(
                "SELECT c.client_id, c.client_name, c.metadata,
                        COALESCE(SUM(sl.amount_cents) FILTER (WHERE sl.created_at >= ?1), 0) as spent_today,
                        COALESCE(SUM(sr.amount_cents), 0) as pending_cents,
                        COALESCE((SELECT COUNT(*) FROM minion_jobs mj
                                  WHERE json_extract(mj.data, '$.__owner_client_id') = c.client_id
                                  AND mj.status NOT IN ('completed','failed','dead','cancelled')
                                  AND mj.deleted_at IS NULL), 0) as inflight_count
                 FROM oauth_clients c
                 LEFT JOIN mcp_spend_log sl ON sl.client_id = c.client_id
                 LEFT JOIN mcp_spend_reservations sr ON sr.client_id = c.client_id
                   AND sr.status = 'pending'
                 WHERE c.deleted_at IS NULL
                   AND ' ' || c.scope || ' ' LIKE '% agent %'
                 GROUP BY c.client_id, c.client_name, c.metadata
                 ORDER BY spent_today DESC",
                ::libsql::params![today_utc_unix()],
            )
            .await;

        match result {
            Ok(mut rows) => {
                let mut items = Vec::new();
                loop {
                    let next = rows.next().await
                        .map_err(|e| Error::engine(format!("list_agent_client_spend row: {e}")))?;
                    match next {
                        Some(row) => {
                            // Extract daily cap from metadata JSON if present.
                            let metadata_str: Option<String> = row.get::<Option<String>>(2)
                                .map_err(|e| Error::engine(format!("list_agent_client_spend metadata: {e}")))?;
                            let cap_usd_per_day = metadata_str
                                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                                .and_then(|v| v.get("daily_spend_cap_usd").cloned())
                                .and_then(|v| v.as_f64());

                            items.push(AgentClientSpend {
                                client_id: row.get::<String>(0)
                                    .map_err(|e| Error::engine(format!("list_agent_client_spend client_id: {e}")))?,
                                client_name: row.get::<String>(1)
                                    .map_err(|e| Error::engine(format!("list_agent_client_spend client_name: {e}")))?,
                                cap_usd_per_day,
                                spent_cents_today: row.get::<i64>(3)
                                    .map_err(|e| Error::engine(format!("list_agent_client_spend spent_today: {e}")))?,
                                pending_cents: row.get::<i64>(4)
                                    .map_err(|e| Error::engine(format!("list_agent_client_spend pending: {e}")))?,
                                inflight_count: row.get::<i64>(5)
                                    .map_err(|e| Error::engine(format!("list_agent_client_spend inflight: {e}")))?,
                            });
                        }
                        None => break,
                    }
                }
                Ok(items)
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") {
                    Ok(vec![])
                } else {
                    Err(Error::engine(format!("list_agent_client_spend: {msg}")))
                }
            }
        }
    }

    async fn get_watch_snapshot(&self) -> Result<WatchSnapshot> {
        let conn = self.conn().await?;
        let now_sec = now_unix() as i64;
        let day_ago = now_sec - 86400;
        let hour_ago = now_sec - 3600;

        // Each sub-query is independently tried; failures are logged and
        // the field stays at its default (zero/empty).

        // 1. Queue health: by_status counts + stalled count
        let (waiting, active, stalled) = match conn.query(
                "SELECT
                   COALESCE(SUM(CASE WHEN status = 'waiting' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN status = 'active' AND lock_until < ?1 THEN 1 ELSE 0 END) +
                            SUM(CASE WHEN status = 'stalled' THEN 1 ELSE 0 END), 0)
                 FROM minion_jobs
                 WHERE deleted_at IS NULL",
                ::libsql::params![now_sec],
            ).await {
            Ok(mut rows) => {
                match rows.next().await {
                    Ok(Some(row)) => (
                        row.get::<i64>(0).unwrap_or(0),
                        row.get::<i64>(1).unwrap_or(0),
                        row.get::<i64>(2).unwrap_or(0),
                    ),
                    _ => (0, 0, 0),
                }
            }
            Err(_) => (0, 0, 0),
        };

        // 2. Per-type summary (last 24h)
        let by_type: Vec<JobTypeSummary> = match conn.query(
            "SELECT COALESCE(type, 'unknown'), COUNT(*) as total,
                    COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'dead' THEN 1 ELSE 0 END), 0)
             FROM minion_jobs
             WHERE deleted_at IS NULL AND created_at >= ?1
             GROUP BY type
             ORDER BY total DESC",
            ::libsql::params![day_ago],
        ).await {
            Ok(mut rows) => collect_job_type_summaries(&mut rows).await.unwrap_or_default(),
            Err(_) => vec![],
        };

        // 3. Lease pressure (last 1h)
        let lease_pressure_1h: i64 = match conn.query(
            "SELECT COUNT(*) FROM minion_lease_pressure_log WHERE bounced_at > ?1",
            ::libsql::params![hour_ago],
        ).await {
            Ok(mut rows) => rows.next().await.ok().flatten()
                .and_then(|r| r.get::<i64>(0).ok()).unwrap_or(0),
            Err(_) => 0,
        };

        // 4. Error clusters — fetch error texts then classify
        let top_errors: Vec<ErrorClusterCount> = match conn.query(
            "SELECT error_text FROM minion_jobs
             WHERE error_text IS NOT NULL AND error_text != ''
               AND status IN ('failed', 'dead')
               AND created_at >= ?1
               AND deleted_at IS NULL",
            ::libsql::params![day_ago],
        ).await {
            Ok(mut rows) => {
                let mut errors: Vec<String> = Vec::new();
                loop {
                    match rows.next().await {
                        Ok(Some(row)) => {
                            if let Ok(text) = row.get::<String>(0) {
                                errors.push(text);
                            }
                        }
                        _ => break,
                    }
                }
                cluster_errors(&errors)
            }
            Err(_) => vec![],
        };

        // 5. Budget owners (active jobs with budgets)
        let budget_owners: Vec<BudgetOwner> = match conn.query(
            "SELECT mj.id, COALESCE(mj.budget_remaining_cents, 0),
                    COALESCE((SELECT SUM(bl.amount_cents) FROM minion_budget_log bl
                              WHERE bl.job_id = mj.id), 0) - COALESCE(mj.budget_remaining_cents, 0)
             FROM minion_jobs mj
             WHERE mj.budget_remaining_cents IS NOT NULL
               AND mj.budget_owner_job_id = mj.id
               AND mj.status NOT IN ('completed', 'failed', 'dead', 'cancelled')
               AND mj.deleted_at IS NULL",
            (),
        ).await {
            Ok(mut rows) => {
                let mut owners = Vec::new();
                loop {
                    match rows.next().await {
                        Ok(Some(row)) => {
                            let _ = row.get::<i64>(0).map(|owner_id| {
                                let remaining = row.get::<i64>(1).unwrap_or(0);
                                let total_spent = row.get::<i64>(2).unwrap_or(0);
                                owners.push(BudgetOwner { owner_id, remaining_cents: remaining, total_spent_cents: total_spent });
                            });
                        }
                        _ => break,
                    }
                }
                owners
            }
            Err(_) => vec![],
        };

        Ok(WatchSnapshot {
            ts_ms: now_sec * 1000,
            by_type,
            queue_health: QueueHealth { waiting, active, stalled },
            lease_pressure_1h,
            top_errors,
            budget_owners,
        })
    }
}


/// Collect JobTypeSummary rows from a query result.
async fn collect_job_type_summaries(rows: &mut ::libsql::Rows) -> std::result::Result<Vec<JobTypeSummary>, Error> {
    let mut out = Vec::new();
    loop {
        let next = rows.next().await
            .map_err(|e| Error::engine(format!("job_type_summary row: {e}")))?;
        match next {
            Some(row) => {
                out.push(JobTypeSummary {
                    name: row.get::<String>(0)
                        .map_err(|e| Error::engine(format!("job_type name: {e}")))?,
                    total: row.get::<i64>(1)
                        .map_err(|e| Error::engine(format!("job_type total: {e}")))?,
                    completed: row.get::<i64>(2)
                        .map_err(|e| Error::engine(format!("job_type completed: {e}")))?,
                    failed: row.get::<i64>(3)
                        .map_err(|e| Error::engine(format!("job_type failed: {e}")))?,
                    dead: row.get::<i64>(4)
                        .map_err(|e| Error::engine(format!("job_type dead: {e}")))?,
                });
            }
            None => break,
        }
    }
    Ok(out)
}

/// Cluster error texts into 13 named buckets.
/// Ported from TS `error-classify.ts`.
fn cluster_errors(errors: &[String]) -> Vec<ErrorClusterCount> {
    use std::collections::BTreeMap;

    let mut counts: BTreeMap<&str, i64> = BTreeMap::new();

    for text in errors {
        let lower = text.to_lowercase();
        let cluster = if lower.contains("rate lease full") {
            "rate_lease_full"
        } else if lower.contains("tool not in registry") || lower.contains("tool not available") {
            "tool_unavailable"
        } else if lower.contains("tool permission") || lower.contains("not allowed") || lower.contains("forbidden") || lower.contains("denied") {
            "tool_permission"
        } else if lower.contains("tool") && (lower.contains("failed") || lower.contains("crashed") || lower.contains("threw") || lower.contains("tool.execute")) {
            "tool_crash"
        } else if lower.contains("invalid") || lower.contains("malformed") || lower.contains("missing input")
            || lower.contains("missing argument") || lower.contains("missing param")
            || lower.contains("schema") || lower.contains("tool_use validation")
        {
            "tool_schema_mismatch"
        } else if lower.contains("parse") || lower.contains("invalid json") || lower.contains("malformed json")
            || lower.contains("expected json") || lower.contains("unexpected token")
        {
            "malformed_json"
        } else if lower.contains("prompt is too long") || lower.contains("context length") || lower.contains("context.*length") {
            "prompt_too_long"
        } else if lower.contains("timeout") || lower.contains("timed out") || lower.contains("aborted: timeout") {
            "timeout"
        } else if lower.contains("401") || lower.contains("unauthorized") || lower.contains("api key invalid") {
            "auth"
        } else if lower.contains("429") || lower.contains("rate limit") || lower.contains("too many requests") {
            "rate_limit"
        } else if lower.contains("50") && (lower.contains("bad gateway") || lower.contains("service unavailable") || lower.contains("overloaded")) {
            "http_5xx"
        } else if lower.contains("budget exceeded") || lower.contains("budget.*exceeded") {
            "budget_exceeded"
        } else if lower.contains("content filter") || lower.contains("content.*filter") {
            "content_filter"
        } else if lower.contains("context length exceeded") || lower.contains("context.*length.*exceeded") {
            "context_length_exceeded"
        } else if lower.contains("bad request") {
            "bad_request"
        } else if lower.contains("server error") || lower.contains("server.*error") {
            "server_error"
        } else if lower.contains("connection") || lower.contains("aborted: cancel") || lower.contains("signal aborted") || lower.contains("context canceled") {
            "connection"
        } else if lower.contains("unknown tool") {
            "unknown_tool"
        } else {
            "other"
        };

        *counts.entry(cluster).or_insert(0) += 1;
    }

    counts
        .into_iter()
        .map(|(cluster, count)| ErrorClusterCount {
            cluster: cluster.to_string(),
            count,
        })
        .collect()
}

/// Build WHERE clause and params for request log filters.
fn build_request_filters_sql(filters: &RequestLogFilters) -> (String, Vec<::libsql::Value>) {
    let mut clauses = Vec::new();
    let mut params: Vec<::libsql::Value> = Vec::new();

    if let Some(ref source) = filters.source {
        clauses.push(format!("token_name = ?{}", params.len() + 1));
        params.push(source.clone().into());
    }
    if let Some(ref method) = filters.method {
        clauses.push(format!("operation = ?{}", params.len() + 1));
        params.push(method.clone().into());
    }
    if let Some(ref status) = filters.status {
        clauses.push(format!("status = ?{}", params.len() + 1));
        params.push(status.clone().into());
    }

    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };

    (where_clause, params)
}

async fn collect_slug_rows(rows: &mut ::libsql::Rows) -> Result<Vec<String>> {
    let mut out = Vec::new();
    loop {
        let next = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("resolve_slugs row fetch failed: {e}")))?;
        match next {
            Some(row) => {
                let slug: String = row
                    .get(0)
                    .map_err(|e| Error::engine(format!("resolve_slugs decode failed: {e}")))?;
                out.push(slug);
            }
            None => break,
        }
    }
    Ok(out)
}

/// Decode one full-width `pages` row into [`Page`]. Column order mirrors the
/// complete 6a page shape used by full-page read paths.
fn full_row_to_page(row: &::libsql::Row) -> Result<Page> {
    macro_rules! get_col {
        ($idx:expr, $name:literal, $ty:ty) => {
            row.get::<$ty>($idx)
                .map_err(|e| Error::engine(format!("row decode {}: {e}", $name)))?
        };
    }

    let id = get_col!(0, "id", i64);
    let slug = get_col!(1, "slug", String);
    let page_type = get_col!(2, "type", String);
    let page_kind_str = get_col!(3, "page_kind", String);
    let title = get_col!(4, "title", String);
    let compiled_truth = get_col!(5, "compiled_truth", String);
    let timeline = get_col!(6, "timeline", String);
    let frontmatter_raw = get_col!(7, "frontmatter", String);
    let content_hash = get_col!(8, "content_hash", Option<String>);
    let emotional_weight = get_col!(9, "emotional_weight", Option<f64>);
    let created_at = get_col!(10, "created_at", String);
    let updated_at = get_col!(11, "updated_at", String);
    let deleted_at = get_col!(12, "deleted_at", Option<String>);
    let last_retrieved_at = get_col!(13, "last_retrieved_at", Option<String>);
    let effective_date = get_col!(14, "effective_date", Option<String>);
    let effective_date_source_raw = get_col!(15, "effective_date_source", Option<String>);
    let import_filename = get_col!(16, "import_filename", Option<String>);
    let salience_touched_at = get_col!(17, "salience_touched_at", Option<String>);
    let salience_score = get_col!(18, "salience_score", Option<f64>);
    let generation = get_col!(19, "generation", i64);
    let embedding = get_col!(20, "embedding", Option<Vec<u8>>);
    let chunker_version_raw = get_col!(21, "chunker_version", Option<i64>);
    let source_path = get_col!(22, "source_path", Option<String>);
    let source_id = get_col!(23, "source_id", String);
    let source_kind = get_col!(24, "source_kind", Option<String>);
    let source_uri = get_col!(25, "source_uri", Option<String>);
    let ingested_via = get_col!(26, "ingested_via", Option<String>);
    let ingested_at = get_col!(27, "ingested_at", Option<String>);
    let contextual_retrieval_mode_raw = get_col!(28, "contextual_retrieval_mode", Option<String>);
    let corpus_generation = get_col!(29, "corpus_generation", Option<String>);

    let page_kind = decode_page_kind(&page_kind_str)?;
    let id_u64 = u64::try_from(id)
        .map_err(|_| Error::engine(format!("page id {id} negative; corrupt row")))?;
    let chunker_version = chunker_version_raw.map_or(Ok(1), |v| {
        i32::try_from(v).map_err(|_| Error::engine(format!("chunker_version {v} overflows i32")))
    })?;
    let frontmatter = serde_json::from_str(&frontmatter_raw)
        .map_err(|e| Error::engine(format!("row decode frontmatter json: {e}")))?;
    let effective_date_source = effective_date_source_raw
        .as_deref()
        .map(decode_effective_date_source)
        .transpose()?;
    let contextual_retrieval_mode = contextual_retrieval_mode_raw
        .as_deref()
        .map(decode_cr_mode)
        .transpose()?;

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
        created_at,
        updated_at,
        deleted_at,
        last_retrieved_at,
        effective_date,
        effective_date_source,
        import_filename,
        salience_touched_at,
        salience_score,
        generation,
        embedding,
        chunker_version,
        source_path,
        source_id,
        source_kind,
        source_uri,
        ingested_via,
        ingested_at,
        contextual_retrieval_mode,
        corpus_generation,
    })
}

/// Encode [`PageKind`] to its `SQLite` wire value (lowercase TEXT).
/// Inverse of [`decode_page_kind`]; panics on new variants (add them here!).
fn encode_page_kind(kind: PageKind) -> &'static str {
    match kind {
        PageKind::Markdown => "markdown",
        PageKind::Code => "code",
        PageKind::Image => "image",
    }
}

/// Encode [`EffectiveDateSource`] to its `SQLite` wire value (`snake_case` TEXT).
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

/// Map the `SQLite` `pages.page_kind` TEXT column (constrained by CHECK) to
/// the [`PageKind`] enum. Kept private and duplicated with `postgres.rs`
/// on purpose — when the column moves into `types.rs` (slice 9?) both
/// callers update together.
fn decode_page_kind(value: &str) -> Result<PageKind> {
    match value {
        "markdown" => Ok(PageKind::Markdown),
        "code" => Ok(PageKind::Code),
        "image" => Ok(PageKind::Image),
        other => Err(Error::engine(format!(
            "unknown page_kind value {other:?}; SQLite CHECK should prevent this"
        ))),
    }
}

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

// ─── CalibrationWaveQueries impl for LibsqlEngine ──────────────────────
//
// Placeholder bodies (return 0) — each step's real SQL is filled in by its
// TDD slice against the libsql behavior tests. `undo_wave` drives these.

#[async_trait]
impl CalibrationWaveQueries for LibsqlEngine {
    async fn revert_wave_resolutions(
        &self,
        wave_version: &str,
        resolved_by: &str,
        dry_run: bool,
    ) -> Result<u64> {
        let conn = self.conn().await?;

        // Locate the takes this wave auto-resolved via the grade cache
        // (applied=true + wave match), mirroring canonical TS `undoWave`
        // Step 1. The resolved_by cross-check below protects takes that a
        // manual `takes resolve` overrode after grade_takes wrote them.
        let mut rows = conn
            .query(
                "SELECT DISTINCT take_id FROM take_grade_cache \
                 WHERE wave_version = ?1 AND applied = true",
                ::libsql::params![wave_version],
            )
            .await
            .map_err(|e| Error::engine(format!("revert_wave_resolutions (targets): {e}")))?;
        let mut take_ids: Vec<i64> = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("revert_wave_resolutions (targets): {e}")))?
        {
            take_ids.push(
                row.get(0)
                    .map_err(|e| Error::engine(format!("revert_wave_resolutions (targets): {e}")))?,
            );
        }
        if take_ids.is_empty() {
            return Ok(0);
        }

        // Build a parameterized IN list: ?1 = resolved_by, ?2.. = take ids.
        let placeholders = (0..take_ids.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let mut params: Vec<::libsql::Value> =
            vec![::libsql::Value::Text(resolved_by.to_string())];
        params.extend(take_ids.into_iter().map(::libsql::Value::Integer));

        if dry_run {
            let sql = format!(
                "SELECT COUNT(*) FROM takes WHERE resolved_by = ?1 AND id IN ({placeholders})"
            );
            let mut rows = conn
                .query(&sql, params)
                .await
                .map_err(|e| Error::engine(format!("revert_wave_resolutions (dry): {e}")))?;
            let row = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("revert_wave_resolutions (dry): {e}")))?
                .ok_or_else(|| Error::engine("revert_wave_resolutions: COUNT returned no row"))?;
            let n: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("revert_wave_resolutions (dry): {e}")))?;
            return Ok(u64::try_from(n).unwrap_or(0));
        }

        // NOTE: the canonical TS UPDATE also NULLs `resolved_source`; the
        // Rust takes schema (0012_takes_full_columns) has no such column,
        // so the reset covers the six resolved_* columns that exist here.
        let sql = format!(
            "UPDATE takes SET \
                resolved_at = NULL, \
                resolved_outcome = NULL, \
                resolved_quality = NULL, \
                resolved_value = NULL, \
                resolved_unit = NULL, \
                resolved_by = NULL \
             WHERE resolved_by = ?1 AND id IN ({placeholders})"
        );
        let affected = conn
            .execute(&sql, params)
            .await
            .map_err(|e| Error::engine(format!("revert_wave_resolutions: {e}")))?;
        Ok(affected)
    }

    async fn unapply_wave_grade_cache(&self, wave_version: &str, dry_run: bool) -> Result<u64> {
        let conn = self.conn().await?;
        if dry_run {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM take_grade_cache \
                     WHERE wave_version = ?1 AND applied = true",
                    ::libsql::params![wave_version],
                )
                .await
                .map_err(|e| Error::engine(format!("unapply_wave_grade_cache (dry): {e}")))?;
            let row = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("unapply_wave_grade_cache (dry): {e}")))?
                .ok_or_else(|| Error::engine("unapply_wave_grade_cache: COUNT returned no row"))?;
            let n: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("unapply_wave_grade_cache (dry): {e}")))?;
            return Ok(u64::try_from(n).unwrap_or(0));
        }
        let affected = conn
            .execute(
                "UPDATE take_grade_cache SET applied = false \
                 WHERE wave_version = ?1 AND applied = true",
                ::libsql::params![wave_version],
            )
            .await
            .map_err(|e| Error::engine(format!("unapply_wave_grade_cache: {e}")))?;
        Ok(affected)
    }

    async fn delete_calibration_profiles_for_wave(
        &self,
        wave_version: &str,
        dry_run: bool,
    ) -> Result<u64> {
        let conn = self.conn().await?;
        if dry_run {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM calibration_profiles WHERE wave_version = ?1",
                    ::libsql::params![wave_version],
                )
                .await
                .map_err(|e| Error::engine(format!("delete_calibration_profiles_for_wave (dry): {e}")))?;
            let row = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("delete_calibration_profiles_for_wave (dry): {e}")))?
                .ok_or_else(|| Error::engine("delete_calibration_profiles_for_wave: COUNT returned no row"))?;
            let n: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("delete_calibration_profiles_for_wave (dry): {e}")))?;
            return Ok(u64::try_from(n).unwrap_or(0));
        }
        let affected = conn
            .execute(
                "DELETE FROM calibration_profiles WHERE wave_version = ?1",
                ::libsql::params![wave_version],
            )
            .await
            .map_err(|e| Error::engine(format!("delete_calibration_profiles_for_wave: {e}")))?;
        Ok(affected)
    }

    async fn purge_nudge_log_for_wave(&self, wave_version: &str, dry_run: bool) -> Result<u64> {
        let conn = self.conn().await?;
        if dry_run {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM take_nudge_log WHERE wave_version = ?1",
                    ::libsql::params![wave_version],
                )
                .await
                .map_err(|e| Error::engine(format!("purge_nudge_log_for_wave (dry): {e}")))?;
            let row = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("purge_nudge_log_for_wave (dry): {e}")))?
                .ok_or_else(|| Error::engine("purge_nudge_log_for_wave: COUNT returned no row"))?;
            let n: i64 = row
                .get(0)
                .map_err(|e| Error::engine(format!("purge_nudge_log_for_wave (dry): {e}")))?;
            return Ok(u64::try_from(n).unwrap_or(0));
        }
        let affected = conn
            .execute(
                "DELETE FROM take_nudge_log WHERE wave_version = ?1",
                ::libsql::params![wave_version],
            )
            .await
            .map_err(|e| Error::engine(format!("purge_nudge_log_for_wave: {e}")))?;
        Ok(affected)
    }
}

// ─── CalibrationQueries impl for LibsqlEngine ───────────────────────────

#[async_trait]
impl CalibrationQueries for LibsqlEngine {
    /// Aggregated scoring stats from resolved takes.
    ///
    /// Pulls the minimal scoped rows (`kind`/`weight`/`resolved_quality`) then
    /// delegates the canonical math to `aggregate_scorecard`, so InMemory,
    /// Libsql, and Postgres are bit-identical. Scoping mirrors canonical TS
    /// `getScorecard`: holder + optional slug-prefix domain via
    /// `EXISTS(pages.slug LIKE prefix%)` + `since_date` window + allow-list.
    async fn get_scorecard(&self, query: &ScorecardQuery<'_>) -> Result<TakesScorecard> {
        let conn = self.conn().await?;

        // Canonical `WHERE 1=1` base; every scope clause is optional and appends
        // its own positional param so the bind order below stays aligned.
        let mut sql = String::from(
            "SELECT t.kind, t.weight, t.resolved_quality FROM takes t WHERE 1=1",
        );
        let mut params: Vec<::libsql::Value> = Vec::new();
        // Optional single-holder filter (omitted when None, canonical parity).
        if let Some(holder) = query.holder {
            params.push(::libsql::Value::from(holder.to_string()));
            sql.push_str(&format!(" AND t.holder = ?{}", params.len()));
        }
        if let Some(prefix) = query.domain_prefix {
            params.push(::libsql::Value::from(format!("{prefix}%")));
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM pages p WHERE p.id = t.page_id AND p.slug LIKE ?{})",
                params.len()
            ));
        }
        if let Some(since) = query.since {
            params.push(::libsql::Value::from(since.to_string()));
            sql.push_str(&format!(" AND t.since_date >= ?{}", params.len()));
        }
        if let Some(until) = query.until {
            params.push(::libsql::Value::from(until.to_string()));
            sql.push_str(&format!(" AND t.since_date <= ?{}", params.len()));
        }
        // Allow-list membership (`AND holder = ANY($list)`, expanded to an IN
        // list on SQLite). Empty list fails closed.
        if let Some(list) = query.holders_allow_list {
            if list.is_empty() {
                sql.push_str(" AND 0=1");
            } else {
                let start = params.len();
                let placeholders: Vec<String> =
                    (0..list.len()).map(|i| format!("?{}", start + i + 1)).collect();
                sql.push_str(&format!(" AND t.holder IN ({})", placeholders.join(", ")));
                for h in list {
                    params.push(::libsql::Value::from(h.clone()));
                }
            }
        }

        let result = conn.query(&sql, ::libsql::params_from_iter(params)).await;

        match result {
            Err(e) => {
                let msg = e.to_string();
                // Graceful degradation: `takes`/`pages` table or a column may
                // not exist in the current schema — degrade to the zero card.
                if msg.contains("no such table") || msg.contains("no such column") {
                    return Ok(aggregate_scorecard(std::iter::empty()));
                }
                Err(Error::engine(format!("get_scorecard: {msg}")))
            }
            Ok(mut rows) => {
                let mut scored: Vec<ScorecardRow> = Vec::new();
                while let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| Error::engine(format!("get_scorecard row: {e}")))?
                {
                    scored.push(ScorecardRow {
                        kind: row.get::<String>(0).unwrap_or_default(),
                        weight: row.get::<f64>(1).unwrap_or(0.0),
                        resolved_quality: row.get::<Option<String>>(2).unwrap_or(None),
                    });
                }
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
        let conn = self.conn().await?;

        let mut sql = String::from(
            "SELECT t.weight, t.resolved_quality FROM takes t WHERE 1=1",
        );
        let mut params: Vec<::libsql::Value> = Vec::new();
        // Optional single-holder filter (omitted when None, canonical parity).
        if let Some(holder) = query.holder {
            params.push(::libsql::Value::from(holder.to_string()));
            sql.push_str(&format!(" AND t.holder = ?{}", params.len()));
        }
        // Allow-list membership (`AND holder = ANY($list)`, expanded to an IN
        // list on SQLite). Empty list fails closed (no rows).
        if let Some(list) = query.holders_allow_list {
            if list.is_empty() {
                sql.push_str(" AND 0=1");
            } else {
                let start = params.len();
                let placeholders: Vec<String> =
                    (0..list.len()).map(|i| format!("?{}", start + i + 1)).collect();
                sql.push_str(&format!(" AND t.holder IN ({})", placeholders.join(", ")));
                for h in list {
                    params.push(::libsql::Value::from(h.clone()));
                }
            }
        }
        sql.push_str(" AND t.resolved_quality IN ('correct','incorrect')");

        let result = conn.query(&sql, ::libsql::params_from_iter(params)).await;

        match result {
            Err(e) => {
                let msg = e.to_string();
                // Graceful degradation: `takes` table or a column may not exist
                // in the current schema — degrade to the empty curve.
                if msg.contains("no such table") || msg.contains("no such column") {
                    return Ok(Vec::new());
                }
                Err(Error::engine(format!("get_calibration_curve: {msg}")))
            }
            Ok(mut rows) => {
                let mut scored: Vec<CalibrationRow> = Vec::new();
                while let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| Error::engine(format!("get_calibration_curve row: {e}")))?
                {
                    scored.push(CalibrationRow {
                        weight: row.get::<f64>(0).unwrap_or(0.0),
                        resolved_quality: row.get::<Option<String>>(1).unwrap_or(None),
                    });
                }
                Ok(aggregate_calibration_curve(
                    scored,
                    query.bucket_size.unwrap_or(0.1),
                ))
            }
        }
    }

    /// Latest calibration profile for a holder.
    async fn get_latest_profile(&self, holder: &str, source_id: Option<&str>, source_ids: Option<&[String]>) -> Result<Option<CalibrationProfileRow>> {
        let conn = self.conn().await?;

        use libsql::Value;
        let mut sql = String::from(
            "SELECT \
                    id, source_id, holder, wave_version, generated_at, published, \
                    total_resolved, brier, accuracy, partial_rate, grade_completion, \
                    domain_scorecards, pattern_statements, voice_gate_passed, \
                    voice_gate_attempts, active_bias_tags, model_id, cost_usd, \
                    judge_model_agreement \
             FROM calibration_profiles \
             WHERE holder = ?1 "
        );
        let mut params: Vec<Value> = vec![Value::Text(holder.to_string())];
        let mut param_idx = 2;

        if let Some(s) = source_id {
            sql.push_str(&format!(" AND source_id = ?{} ", param_idx));
            params.push(Value::Text(s.to_string()));
            param_idx += 1;
        }
        if let Some(sids) = source_ids {
            if !sids.is_empty() {
                sql.push_str(" AND source_id IN (");
                for (i, sid) in sids.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push_str(&format!("?{}", param_idx));
                    params.push(Value::Text(sid.to_string()));
                    param_idx += 1;
                }
                sql.push_str(") ");
            }
        }

        sql.push_str(" ORDER BY generated_at DESC LIMIT 1");

        let result = conn
            .query(
                &sql,
                params,
            )
            .await;

        match result {
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") {
                    return Ok(None);
                }
                Err(Error::engine(format!(
                    "get_latest_profile: {msg}"
                )))
            }
            Ok(mut rows) => {
                if let Some(row) = rows.next().await.map_err(|e| Error::engine(format!("get_latest_profile row: {e}")))? {
                    // SQLite arrays stored as JSON strings. Map by SELECT column
                    // index: 11=domain_scorecards, 12=pattern_statements,
                    // 15=active_bias_tags (see the SELECT column order above).
                    let domain_json: String = row.get(11).unwrap_or_else(|_| "{}".into());
                    let pattern_json: String = row.get(12).unwrap_or_else(|_| "[]".into());
                    let bias_json: String = row.get(15).unwrap_or_else(|_| "[]".into());

                    let pattern_statements: Vec<String> = serde_json::from_str(&pattern_json).unwrap_or_default();
                    let active_bias_tags: Vec<String> = serde_json::from_str(&bias_json).unwrap_or_default();
                    let domain_scorecards: serde_json::Value = serde_json::from_str(&domain_json).unwrap_or_else(|_| serde_json::json!({}));

                    Ok(Some(CalibrationProfileRow {
                        id: row.get::<i64>(0).unwrap_or_default(),
                        source_id: row.get::<String>(1).unwrap_or_default(),
                        holder: row.get::<String>(2).unwrap_or_default(),
                        wave_version: row.get::<String>(3).unwrap_or_default(),
                        generated_at: row.get::<String>(4).unwrap_or_default(),
                        // SQLite stores BOOLEAN as INTEGER; libsql's `get::<bool>`
                        // rejects integer values, so read as i64 and coerce.
                        published: row.get::<i64>(5).unwrap_or_default() != 0,
                        total_resolved: row.get::<i32>(6).unwrap_or_default(),
                        brier: row.get::<Option<f64>>(7).unwrap_or(None),
                        accuracy: row.get::<Option<f64>>(8).unwrap_or(None),
                        partial_rate: row.get::<Option<f64>>(9).unwrap_or(None),
                        grade_completion: row.get::<f64>(10).unwrap_or(1.0),
                        domain_scorecards,
                        pattern_statements,
                        voice_gate_passed: row.get::<i64>(13).unwrap_or_default() != 0,
                        voice_gate_attempts: row.get::<i32>(14).unwrap_or_default() as i16,
                        active_bias_tags,
                        model_id: row.get::<String>(16).unwrap_or_default(),
                        cost_usd: row.get::<Option<f64>>(17).unwrap_or(None),
                        judge_model_agreement: row.get::<Option<f64>>(18).unwrap_or(None),
                    }))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Pattern text + top-25 resolved takes for drill-down.
    async fn get_pattern_detail(
        &self,
        holder: &str,
        pattern_index: usize,
    ) -> Result<Option<PatternDetail>> {
        let conn = self.conn().await?;

        // 1) Get the pattern statement from the latest calibration profile.
        let profile_result = conn
            .query(
                "SELECT pattern_statements FROM calibration_profiles \
                 WHERE holder = ?1 ORDER BY generated_at DESC LIMIT 1",
                ::libsql::params![holder],
            )
            .await;

        let pattern_text = match profile_result {
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") {
                    return Ok(None);
                }
                return Err(Error::engine(format!("get_pattern_detail profile: {msg}")));
            }
            Ok(mut rows) => {
                if let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| Error::engine(format!("get_pattern_detail row: {e}")))?
                {
                    let json_str: Option<String> = row.get(0).unwrap_or(None);
                    json_str
                        .and_then(|s| serde_json::from_str::<Vec<String>>(s.as_str()).ok())
                        .and_then(|v| v.into_iter().nth(pattern_index.saturating_sub(1)))
                        .unwrap_or_default()
                } else {
                    return Ok(None);
                }
            }
        };
        if pattern_text.is_empty() {
            return Ok(None);
        }

        // 2) Get top-25 resolved takes (ordered by most recently resolved).
        let takes_result = conn
            .query(
                "SELECT slug, claim, resolution, brier \
                 FROM takes \
                 WHERE holder = ?1 AND resolved_at IS NOT NULL \
                 ORDER BY resolved_at DESC \
                 LIMIT 25",
                ::libsql::params![holder],
            )
            .await;

        let top_takes: Vec<TakeSummary> = match takes_result {
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") {
                    vec![]
                } else {
                    return Err(Error::engine(format!(
                        "get_pattern_detail takes: {msg}"
                    )));
                }
            }
            Ok(mut rows) => {
                let mut out = Vec::new();
                while let Some(row) = rows.next().await.map_err(|e| Error::engine(format!("get_pattern_detail take row: {e}")))? {
                    out.push(TakeSummary {
                        slug: row.get::<String>(0).unwrap_or_default(),
                        claim: row.get::<String>(1).unwrap_or_default(),
                        resolution: row.get::<Option<String>>(2).unwrap_or(None),
                        brier: row.get::<Option<f64>>(3).unwrap_or(None),
                    });
                }
                out
            }
        };

        Ok(Some(PatternDetail {
            pattern_text,
            top_takes,
        }))
    }

    /// Insert one A/B trial row (1-3-3-6). FK violations (unknown source_id)
    /// surface as errors — we never fabricate a source (G52).
    async fn insert_think_ab_result(&self, row: &ThinkAbInsert<'_>) -> Result<Option<i64>> {
        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "INSERT INTO think_ab_results \
                 (source_id, question, baseline_answer, with_calibration_answer, preferred, model_id, notes) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
                ::libsql::params![
                    row.source_id,
                    row.question,
                    row.baseline_answer,
                    row.with_calibration_answer,
                    row.preferred,
                    row.model_id.map(|s| s.to_string()),
                    row.notes.map(|s| s.to_string()),
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("insert_think_ab_result: {e}")))?;
        let id = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("insert_think_ab_result id row: {e}")))?
            .and_then(|r| r.get::<i64>(0).ok());
        Ok(id)
    }

    /// `(preferred, count)` pairs since `cutoff_iso`. `ran_at` is ISO8601 TEXT
    /// on this backend, so the lexicographic `>=` matches chronological order.
    async fn think_ab_preference_counts(&self, cutoff_iso: &str) -> Result<Vec<(String, u64)>> {
        let conn = self.conn().await?;
        let result = conn
            .query(
                "SELECT preferred, COUNT(*) FROM think_ab_results \
                 WHERE ran_at >= ?1 GROUP BY preferred",
                ::libsql::params![cutoff_iso],
            )
            .await;
        match result {
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") {
                    return Ok(Vec::new());
                }
                Err(Error::engine(format!("think_ab_preference_counts: {msg}")))
            }
            Ok(mut rows) => {
                let mut out = Vec::new();
                while let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| Error::engine(format!("think_ab_preference_counts row: {e}")))?
                {
                    let preferred: String = row.get(0).unwrap_or_default();
                    let count: i64 = row.get(1).unwrap_or(0);
                    out.push((preferred, count.max(0) as u64));
                }
                Ok(out)
            }
        }
    }

    /// Insert one calibration-profile row (1-3-3-7). `generated_at` defaults to
    /// now() and `published` to false (SQLite `DEFAULT`); `cost_usd` /
    /// `judge_model_agreement` are NULL — matching the canonical TS INSERT.
    /// `source_id` is a NOT NULL FK to `sources(id)`; an unknown source surfaces
    /// as an error (G52), never a fabricated row.
    async fn insert_calibration_profile(&self, row: &CalibrationProfileInsert<'_>) -> Result<i64> {
        let conn = self.conn().await?;
        let pattern_json = serde_json::to_string(&row.pattern_statements)
            .map_err(|e| Error::engine(format!("insert_calibration_profile ser: {e}")))?;
        let bias_json = serde_json::to_string(&row.active_bias_tags)
            .map_err(|e| Error::engine(format!("insert_calibration_profile ser: {e}")))?;
        let domain_json = serde_json::to_string(&row.domain_scorecards)
            .map_err(|e| Error::engine(format!("insert_calibration_profile ser: {e}")))?;
        let mut rows = conn
            .query(
                "INSERT INTO calibration_profiles \
                 (source_id, holder, total_resolved, brier, accuracy, partial_rate, grade_completion, domain_scorecards, pattern_statements, voice_gate_passed, voice_gate_attempts, active_bias_tags, model_id, cost_usd, judge_model_agreement) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, NULL) RETURNING id",
                ::libsql::params![
                    row.source_id,
                    row.holder,
                    row.total_resolved,
                    row.brier,
                    row.accuracy,
                    row.partial_rate,
                    row.grade_completion,
                    domain_json,
                    pattern_json,
                    row.voice_gate_passed,
                    row.voice_gate_attempts,
                    bias_json,
                    row.model_id,
                ],
            )
            .await
            .map_err(|e| Error::engine(format!("insert_calibration_profile: {e}")))?;
        let id = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("insert_calibration_profile id row: {e}")))?
            .and_then(|r| r.get::<i64>(0).ok());
        id.ok_or_else(|| Error::engine("insert_calibration_profile: no id returned"))
    }
}

// ── OAuthQueries LibsqlEngine implementation ──────────────────────────

#[async_trait]
impl OAuthQueries for LibsqlEngine {
    async fn register_client(
        &self,
        req: RegisterClientRequest,
    ) -> Result<RegisterClientResponse> {
        let client_id = uuid::Uuid::new_v4().to_string();
        let client_secret = uuid::Uuid::new_v4().to_string();
        let secret_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(client_secret.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let conn = self.conn().await?;
        let grant_types_json = serde_json::to_string(&req.grant_types)
            .map_err(|e| Error::engine(format!("serialize grant_types: {e}")))?;
        let redirect_uris_json = serde_json::to_string(&req.redirect_uris)
            .map_err(|e| Error::engine(format!("serialize redirect_uris: {e}")))?;

        // Normalise federated_read: default to [source_id] when empty.
        let federated_read = if req.federated_read.is_empty() {
            vec![req.source_id.clone()]
        } else {
            req.federated_read.clone()
        };
        let federated_read_json = serde_json::to_string(&federated_read)
            .map_err(|e| Error::engine(format!("serialize federated_read: {e}")))?;

        conn.execute(
            "INSERT INTO oauth_clients \
             (client_id, client_secret_hash, client_name, redirect_uris, grant_types, scope, \
              token_endpoint_auth_method, token_ttl, client_id_issued_at, \
              source_id, federated_read) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            ::libsql::params![
                client_id.clone(),
                secret_hash,
                req.name,
                redirect_uris_json,
                grant_types_json,
                req.scope,
                req.token_endpoint_auth_method,
                req.token_ttl,
                // client_id_issued_at: seconds since epoch
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
                req.source_id,
                federated_read_json,
            ],
        )
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
        let conn = self.conn().await?;

        // normalize: 0 means "no TTL" (NULL in DB)
        let db_ttl = ttl.filter(|&v| v > 0);

        conn.execute(
            "UPDATE oauth_clients SET token_ttl = ?1 WHERE client_id = ?2",
            ::libsql::params![db_ttl, client_id],
        )
        .await
        .map_err(|e| Error::engine(format!("update_client_ttl: {e}")))?;

        Ok(UpdateClientTtlResponse {
            updated: true,
            token_ttl: db_ttl,
        })
    }

    async fn revoke_client(&self, client_id: &str) -> Result<RevokeClientResponse> {
        let conn = self.conn().await?;

        // Soft-delete the client (only if not already deleted)
        conn.execute(
            "UPDATE oauth_clients SET deleted_at = ?1 \
             WHERE client_id = ?2 AND deleted_at IS NULL",
            ::libsql::params![current_utc_iso8601(), client_id],
        )
        .await
        .map_err(|e| Error::engine(format!("revoke_client soft-delete: {e}")))?;

        // Revoke all active tokens for this client
        conn.execute(
            "DELETE FROM oauth_tokens WHERE client_id = ?1",
            ::libsql::params![client_id],
        )
        .await
        .map_err(|e| Error::engine(format!("revoke_client delete tokens: {e}")))?;

        Ok(RevokeClientResponse { revoked: true })
    }

    async fn get_client(
        &self,
        client_id: &str,
    ) -> Result<Option<crate::oauth_queries::OAuthClientInfo>> {
        use crate::oauth_queries::OAuthClientInfo;

        let conn = self.conn().await?;
        let mut rows = conn
            .query(
                "SELECT client_id, client_secret_hash, client_name, redirect_uris, \
                 grant_types, scope, token_endpoint_auth_method, \
                 client_id_issued_at, client_secret_expires_at, token_ttl \
                 FROM oauth_clients WHERE client_id = ?1",
                ::libsql::params![client_id],
            )
            .await
            .map_err(|e| Error::engine(format!("get_client query: {e}")))?;

        let row = match rows.next().await.map_err(|e| Error::engine(format!("get_client next: {e}")))? {
            Some(r) => r,
            None => return Ok(None),
        };

        let redirect_uris_json = row.get::<String>(3).unwrap_or_default();
        let grant_types_json = row.get::<String>(4)
            .unwrap_or_else(|_| "[\"client_credentials\"]".to_string());
        let redirect_uris: Vec<String> =
            serde_json::from_str(&redirect_uris_json).unwrap_or_default();
        let grant_types: Vec<String> = serde_json::from_str(&grant_types_json)
            .unwrap_or_else(|_| vec!["client_credentials".to_string()]);

        Ok(Some(OAuthClientInfo {
            client_id: row.get::<String>(0).unwrap_or_default(),
            client_secret_hash: row.get::<Option<String>>(1).unwrap_or_default(),
            client_name: row.get::<String>(2).unwrap_or_default(),
            redirect_uris,
            grant_types,
            scope: row.get::<Option<String>>(5).unwrap_or_default(),
            token_endpoint_auth_method: row.get::<Option<String>>(6).unwrap_or_default(),
            client_id_issued_at: row.get::<Option<i64>>(7).unwrap_or_default(),
            client_secret_expires_at: row.get::<Option<i64>>(8).unwrap_or_default(),
            token_ttl: row.get::<Option<i64>>(9).unwrap_or_default(),
        }))
    }

    async fn exchange_client_credentials(
        &self,
        client_id: &str,
        client_secret: &str,
        requested_scope: Option<&str>,
    ) -> Result<crate::oauth_queries::ExchangeTokens> {
        use crate::oauth_queries::{ExchangeTokens, OAuthClientInfo};
        use crate::scope::{has_scope, parse_scope_string};

        // Look up the client.
        let client = self
            .get_client(client_id)
            .await?
            .ok_or_else(|| Error::engine("Client not found"))?;

        // Check revoked (soft-deleted).
        {
            let conn = self.conn().await?;
            let mut rows = conn
                .query(
                    "SELECT 1 FROM oauth_clients WHERE client_id = ?1 AND deleted_at IS NOT NULL",
                    ::libsql::params![client_id],
                )
                .await
                .map_err(|e| Error::engine(format!("check revoked: {e}")))?;
            let deleted = rows.next().await.ok().flatten().is_some();
            if deleted {
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

        // Determine scopes — clamp against registered scope using has_scope.
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

        // Per-client TTL override (graceful fallback if column missing).
        let ttl_override = client.token_ttl.filter(|&t| t > 0);

        // Issue access token only (no refresh for client_credentials per RFC 6749 §4.4.3).
        let tokens = self
            .issue_oauth_tokens(client_id, &granted_scopes, false, ttl_override)
            .await?;
        Ok(tokens)
    }

    async fn verify_confidential_client_secret(
        &self,
        client_id: &str,
        presented_secret: &str,
    ) -> Result<crate::oauth_queries::OAuthClientInfo> {
        use crate::oauth_queries::OAuthClientInfo;

        let client = self
            .get_client(client_id)
            .await?
            .ok_or_else(|| Error::engine("Invalid client"))?;

        // Public client — refuse hash-compare path.
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
            let conn = self.conn().await?;
            let mut rows = conn
                .query(
                    "SELECT 1 FROM oauth_clients WHERE client_id = ?1 AND deleted_at IS NOT NULL",
                    ::libsql::params![client_id],
                )
                .await
                .map_err(|e| Error::engine(format!("check revoked: {e}")))?;
            let deleted = rows.next().await.ok().flatten().is_some();
            if deleted {
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
    ) -> Result<crate::oauth_queries::ExchangeTokens> {
        use crate::oauth_queries::ExchangeTokens;
        use crate::scope::parse_scope_string;

        let code_hash = sha256_hex(authorization_code.as_bytes());
        let now = unix_now_secs();
        let conn = self.conn().await?;

        // Atomically DELETE the code row (single-use), validate client_id + redirect_uri.
        let rows = if let Some(redirect) = redirect_uri {
            conn.query(
                "DELETE FROM oauth_codes \
                 WHERE code_hash = ?1 AND client_id = ?2 AND redirect_uri = ?3 AND expires_at > ?4 \
                 RETURNING client_id, scopes, resource",
                ::libsql::params![code_hash, client_id, redirect, now],
            )
            .await
        } else {
            conn.query(
                "DELETE FROM oauth_codes \
                 WHERE code_hash = ?1 AND client_id = ?2 AND expires_at > ?3 \
                 RETURNING client_id, scopes, resource",
                ::libsql::params![code_hash, client_id, now],
            )
            .await
        }
        .map_err(|e| Error::engine(format!("exchange_authorization_code delete: {e}")))?;

        let mut rows = rows;
        let row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("exchange_authorization_code next: {e}")))?
            .ok_or_else(|| Error::engine("Authorization code not found or expired"))?;

        let scopes_json = row.get::<String>(1).unwrap_or_default();
        let scopes: Vec<String> =
            serde_json::from_str(&scopes_json).unwrap_or_default();

        let granted: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();
        self.issue_oauth_tokens(client_id, &granted, true, None).await
    }

    async fn exchange_refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        requested_scopes: Option<&[String]>,
    ) -> Result<crate::oauth_queries::ExchangeTokens> {
        use crate::oauth_queries::ExchangeTokens;
        use crate::scope::has_scope;

        let token_hash = sha256_hex(refresh_token.as_bytes());
        let now = unix_now_secs();
        let conn = self.conn().await?;

        // Atomically DELETE the refresh token row (rotation).
        let mut rows = conn
            .query(
                "DELETE FROM oauth_tokens \
                 WHERE token_hash = ?1 AND token_type = 'refresh' AND client_id = ?2 \
                 RETURNING client_id, scopes, expires_at",
                ::libsql::params![token_hash, client_id],
            )
            .await
            .map_err(|e| Error::engine(format!("exchange_refresh_token delete: {e}")))?;

        let row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("exchange_refresh_token next: {e}")))?
            .ok_or_else(|| Error::engine("Refresh token not found"))?;

        // Check expiration.
        let expires_at: i64 = row.get(2).unwrap_or(0);
        if expires_at < now {
            return Err(Error::engine("Refresh token expired"));
        }

        // Scope subset enforcement (RFC 6749 §6).
        let scopes_json: String = row.get::<String>(1).unwrap_or_default();
        let granted_scopes: Vec<String> =
            serde_json::from_str(&scopes_json).unwrap_or_default();

        let token_scopes: Vec<String> = match requested_scopes {
            Some(req) if !req.is_empty() => {
                // All requested scopes must be a subset of the granted scopes.
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
        let conn = self.conn().await?;
        let now = unix_now_secs();

        // Delete expired access + refresh tokens.
        let tokens_deleted = conn
            .execute(
                "DELETE FROM oauth_tokens WHERE expires_at < ?1",
                ::libsql::params![now],
            )
            .await
            .map_err(|e| Error::engine(format!("sweep_expired_tokens oauth_tokens: {e}")))?;

        // Delete expired authorization codes.
        let codes_deleted = conn
            .execute(
                "DELETE FROM oauth_codes WHERE expires_at < ?1",
                ::libsql::params![now],
            )
            .await
            .map_err(|e| Error::engine(format!("sweep_expired_tokens oauth_codes: {e}")))?;

        Ok(tokens_deleted + codes_deleted)
    }
}

// ── TokenQueries LibsqlEngine implementation ────────────────────────────

use crate::{TokenQueries, AuthInfo, TokenError};

#[async_trait::async_trait]
impl TokenQueries for LibsqlEngine {
    async fn verify_access_token(
        &self,
        token: &str,
    ) -> std::result::Result<AuthInfo, TokenError> {
        let token_hash = sha256_hex(token.as_bytes());
        let now_secs = unix_now_secs();

        let mut conn = self.conn().await.map_err(|e| TokenError::Storage(e.to_string()))?;

        let mut rows = conn
            .query(
                "SELECT t.client_id, t.scopes, t.expires_at, \
                    c.client_name, c.source_id, t.resource, c.federated_read \
                 FROM oauth_tokens t \
                 LEFT JOIN oauth_clients c ON c.client_id = t.client_id \
                 WHERE t.token_hash = ?1 AND t.token_type = 'access'",
                ::libsql::params![token_hash.clone()],
            )
            .await
            .map_err(|e| TokenError::Storage(e.to_string()))?;

        match rows.next().await {
            Ok(Some(row)) => {
            let expires_at: i64 = row.get(2).unwrap_or(0);
            if expires_at == 0 || expires_at < now_secs {
                return Err(TokenError::Expired);
            }

            let scopes_raw: String = row.get(3).unwrap_or_default();
            let scopes: Vec<String> = serde_json::from_str(&scopes_raw).unwrap_or_default();

            let client_id: String = row.get(0).unwrap_or_default();
            let client_name: Option<String> = row.get(4).ok();
            let source_id: Option<String> = row.get(5).ok();
            let resource: Option<String> = row.get(6).ok();
            let federated_read_raw: Option<String> = row.get(7).ok();
            let allowed_sources: Option<Vec<String>> = federated_read_raw
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok());

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
            Ok(None) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such table") || msg.contains("does not exist") {
                    return Err(TokenError::Invalid);
                } else {
                    return Err(TokenError::Storage(msg));
                }
            }
        }

        // Fallback: legacy access_tokens table.
        let mut legacy_rows = conn
            .query(
                "SELECT name FROM access_tokens \
                 WHERE token_hash = ?1 AND revoked_at IS NULL",
                ::libsql::params![token_hash.clone()],
            )
            .await;

        match legacy_rows {
            Ok(mut rows) => {
                match rows.next().await {
                    Ok(Some(row)) => {
                    let name: String = row.get(0).unwrap_or_default();
                    // Update last_used_at (best-effort).
                    let _ = conn
                        .execute(
                    "UPDATE access_tokens SET last_used_at = CURRENT_TIMESTAMP WHERE token_hash = ?1",
                    ::libsql::params![token_hash.clone()],
                        )
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

// ── Internal: OAuth token issuance helper ─────────────────────────────────────

impl LibsqlEngine {
    // ─── recall batch helpers (shared fact-list WHERE tail) ───────────────
    //
    // Shared WHERE tail for the fact-list family: active_only, kinds IN (..),
    // visibility IN (..), ORDER BY created_at DESC, id DESC, LIMIT/OFFSET.
    // `params` must already hold the base bindings (source_id + any
    // method-specific conditions) in positional order.
    fn append_fact_list_filters(
        sql: &mut String,
        params: &mut Vec<::libsql::Value>,
        opts: &FactListOpts,
    ) {
        if opts.active_only.unwrap_or(false) {
            sql.push_str(" AND expired_at IS NULL AND superseded_by IS NULL");
        }
        if let Some(ref kinds) = opts.kinds {
            if !kinds.is_empty() {
                sql.push_str(" AND kind IN (");
                for (i, _) in kinds.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                }
                sql.push(')');
            }
        }
        if let Some(ref vs) = opts.visibility {
            if !vs.is_empty() {
                sql.push_str(" AND visibility IN (");
                for (i, _) in vs.iter().enumerate() {
                    if i > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                }
                sql.push(')');
            }
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC");
        if let Some(ref limit) = opts.limit {
            sql.push_str(" LIMIT ?");
            params.push(::libsql::Value::from(*limit));
        }
        if let Some(ref offset) = opts.offset {
            sql.push_str(" OFFSET ?");
            params.push(::libsql::Value::from(*offset));
        }
    }

    /// Issue access (and optionally refresh) tokens for a client.
    /// Inserts rows into `oauth_tokens` and returns the wire-format response.
    async fn issue_oauth_tokens(
        &self,
        client_id: &str,
        scopes: &[&str],
        include_refresh: bool,
        ttl_override: Option<i64>,
    ) -> Result<crate::oauth_queries::ExchangeTokens> {
        use crate::oauth_queries::ExchangeTokens;

        let access_token = format!("zbrain_at_{}", uuid::Uuid::new_v4().simple());
        let access_hash = sha256_hex(access_token.as_bytes());
        let now = unix_now_secs();
        let effective_ttl = ttl_override.unwrap_or(3600);
        let access_expiry = now + effective_ttl;
        let scopes_json = serde_json::to_string(&scopes)
            .map_err(|e| Error::engine(format!("serialize scopes: {e}")))?;
        let scope_string = scopes.join(" ");

        let conn = self.conn().await?;

        conn.execute(
            "INSERT INTO oauth_tokens (token_hash, token_type, client_id, scopes, expires_at) \
             VALUES (?1, 'access', ?2, ?3, ?4)",
            ::libsql::params![access_hash, client_id, scopes_json.clone(), access_expiry],
        )
        .await
        .map_err(|e| Error::engine(format!("issue_oauth_tokens insert access: {e}")))?;

        let mut refresh_token: Option<String> = None;

        if include_refresh {
            let rt = format!("zbrain_rt_{}", uuid::Uuid::new_v4().simple());
            let rt_hash = sha256_hex(rt.as_bytes());
            let refresh_expiry = now + 30 * 24 * 3600; // 30 days

            conn.execute(
                "INSERT INTO oauth_tokens (token_hash, token_type, client_id, scopes, expires_at) \
                 VALUES (?1, 'refresh', ?2, ?3, ?4)",
                ::libsql::params![rt_hash, client_id, scopes_json, refresh_expiry],
            )
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

    async fn execute_raw(
        &self,
        sql: &str,
        params: &[&(dyn erased_serde::Serialize + Sync)],
    ) -> crate::Result<Vec<serde_json::Value>> {
        use libsql::Value;

        // Convert erased_serde parameters to libsql Values via JSON serialization.
        let mut libsql_params: Vec<Value> = Vec::with_capacity(params.len());
        for p in params {
            let json = serde_json::to_value(p)
                .map_err(|e| crate::Error::engine(format!("serialize parameter: {e}")))?;
            let val = match json {
                serde_json::Value::Null => Value::Null,
                serde_json::Value::Bool(b) => Value::Integer(b as i64),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Value::Integer(i)
                    } else if let Some(f) = n.as_f64() {
                        Value::Real(f)
                    } else {
                        Value::Text(n.to_string())
                    }
                }
                serde_json::Value::String(s) => Value::Text(s),
                // For complex types we just serialize them back to JSON text
                _ => Value::Text(json.to_string()),
            };
            libsql_params.push(val);
        }

        let conn = self.conn().await?;
        let mut rows = conn.query(sql, libsql_params).await
            .map_err(|e| crate::Error::engine(format!("execute_raw query: {e}")))?;

        let mut result = Vec::new();
        while let Some(row) = rows.next().await
            .map_err(|e| crate::Error::engine(format!("execute_raw read row: {e}")))?
        {
            // For each column, extract its value into a JSON Value
            let mut map = serde_json::Map::new();
            let mut col_idx = 0;
            while let Some(col_name) = rows.column_name(col_idx) {
                let val: Value = row.get(col_idx as i32)
                    .map_err(|e| crate::Error::engine(format!("get column index {}: {}", col_idx, e)))?;
                let json_val = match val {
                    Value::Null => serde_json::Value::Null,
                    Value::Integer(i) => serde_json::Value::Number(i.into()),
                    Value::Real(f) => serde_json::Value::Number(
                        serde_json::Number::from_f64(f).unwrap_or(serde_json::Number::from(0))
                    ),
                    Value::Text(t) => serde_json::Value::String(t),
                    Value::Blob(b) => serde_json::Value::Array(
                        b.iter().map(|&x| serde_json::Value::Number(x.into())).collect()
                    ),
                };
                map.insert(col_name.to_string(), json_val);
                col_idx += 1;
            }
            result.push(serde_json::Value::Object(map));
        }

        Ok(result)
    }
}

// ── Helper: SHA-256 hex ──────────────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Current Unix timestamp in seconds (for token expiry calculations).
fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── TokenQueries ──────────────────────────────────────────────────────────────

