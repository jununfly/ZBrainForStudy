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

use crate::engine::{
    page_sort_sql, BrainEngine, EngineConfig, EngineKind, GetPageOpts, Page, PageFilters,
    PageInput, PageSort, ResolveSlugsOpts,
};
use crate::error::{Error, Result};
use crate::migration::{Migration, MigrationRegistry};
use crate::time::current_utc_iso8601;
use crate::types::{
    CRMode, DuplicatePage, EffectiveDateSource, FileRow, FileSpec, FindDuplicatePageOpts,
    OrphanPage, PageKind, PageRef, PageVersion, PurgeResult, RawData, RefreshPageBodyArgs,
    UpsertFileResult,
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
];

/// Legacy version constant — REMOVED in favor of MigrationRegistry.
/// Use LIBQL_MIGRATIONS.latest_version() instead.
#[deprecated(note = "Use LIBQL_MIGRATIONS.latest_version() instead")]
const SCHEMA_VERSION: i64 = 8;

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
}

impl LibsqlEngine {
    /// Construct a disconnected engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            db: OnceLock::new(),
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

    /// Read current migration version from `rust_schema_version` table.
    /// Always returns 0 for fresh databases (guaranteed by bootstrap migration).
    async fn read_rust_schema_version(conn: &::libsql::Connection) -> Result<i64> {
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
        conn.execute("UPDATE rust_schema_version SET version = ?", (ver,))
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

#[async_trait]
impl BrainEngine for LibsqlEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Libsql
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

        let conn = self.conn().await?;

        // Step 1: Bootstrap the version tracking table. Hard cutover from
        // TS-era PRAGMA user_version per 1-2-3 Q4 decision.
        conn.execute_batch(RUST_SCHEMA_VERSION_BOOTSTRAP)
            .await
            .map_err(|e| Error::engine(format!("rust_schema_version bootstrap failed: {e}")))?;

        // Step 2: Read current version from the table
        let current = Self::read_rust_schema_version(&conn).await?;
        let latest = LIBQL_MIGRATIONS.latest_version();
        if current >= latest {
            return Ok(());
        }

        // Step 3: Apply all migrations in a single transaction (Q2 = C).
        // All-or-nothing atomicity: either all migrations apply successfully
        // or nothing changes. Version number is set once at the end.
        conn.execute_batch("BEGIN TRANSACTION")
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
                    let _ = conn.execute_batch("ROLLBACK").await;
                    return Err(Error::engine(format!("migration {ver} failed: {e}")));
                }
            }
        }

        // Version number updated once at the end (single transaction mode)
        if applied_any {
            let latest = LIBQL_MIGRATIONS.latest_version();
            Self::set_rust_schema_version(&conn, latest).await?;
        }

        conn.execute_batch("COMMIT")
            .await
            .map_err(|e| Error::engine(format!("migration batch COMMIT failed: {e}")))?;

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

        let sql = "INSERT INTO pages (\
                source_id, slug, type, page_kind, title, compiled_truth, timeline, \
                frontmatter, content_hash, updated_at, effective_date, \
                effective_date_source, import_filename, chunker_version, \
                source_path, source_kind, source_uri, ingested_via, ingested_at\
            ) VALUES (\
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                COALESCE(?14, 1), ?15, ?16, ?17, ?18, ?19\
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
                ingested_at = COALESCE(excluded.ingested_at, pages.ingested_at) \
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
        // libsql `params!` macro requires concrete types; we build a Vec of
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
