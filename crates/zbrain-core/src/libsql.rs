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

use std::sync::OnceLock;

use async_trait::async_trait;

use crate::engine::{
    BrainEngine, EngineConfig, EngineKind, GetPageOpts, Page, PageFilters, PageInput,
};
use crate::error::{Error, Result};
use crate::types::PageKind;

/// Embedded `SQLite` schema. Mirrors the PG migration semantics:
/// `sources` (with seeded 'default' row), `pages` with `UNIQUE (source_id,
/// slug)` and a `page_kind` CHECK, plus an index on `type`.
const MIGRATION_0001: &str = include_str!("../migrations-sqlite/0001_init.sql");

/// Slice 6a — fills in every `pages` column the 12 Page-CRUD methods need
/// (`frontmatter`, `emotional_weight`, `deleted_at`, …, `embedding`) plus the
/// generation-bump triggers. See file header in `0002_pages_full_columns.sql`.
const MIGRATION_0002: &str = include_str!("../migrations-sqlite/0002_pages_full_columns.sql");

/// Ordered list of migrations. Index `i` is applied when `user_version <= i`,
/// then `user_version` is set to `i + 1`. Append-only — never reorder.
const MIGRATIONS: &[&str] = &[MIGRATION_0001, MIGRATION_0002];

/// Highest schema version we know how to produce. Equals `MIGRATIONS.len()`.
/// Guarded via `PRAGMA user_version`. Hand-kept in sync with `MIGRATIONS`
/// because `len() as i64` is not const-evaluable in stable Rust and casting
/// `usize → i64` trips `clippy::cast_possible_wrap`.
const SCHEMA_VERSION: i64 = 2;

/// Embedded `SQLite` engine. Use [`LibsqlEngine::new`] then [`connect`] before
/// any other method. Calling `connect` twice on the same instance is
/// rejected to keep ownership of the underlying `Database` handle clean.
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

    /// Open a fresh connection on the live database. `libsql::Connection`
    /// is cheap to acquire (just an FFI handle bound to the open file).
    fn conn(&self) -> Result<::libsql::Connection> {
        self.database()?
            .connect()
            .map_err(|e| Error::engine(format!("libsql connect failed: {e}")))
    }
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
            .ok_or_else(|| {
                Error::engine("LibsqlEngine requires EngineConfig.database_path")
            })?;

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
        let conn = self.conn()?;

        // PRAGMA user_version is SQLite's standard "schema version" slot.
        // 0 = fresh database, N = migration N has been applied.
        let mut rows = conn
            .query("PRAGMA user_version", ())
            .await
            .map_err(|e| Error::engine(format!("read user_version failed: {e}")))?;
        let current: i64 = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("user_version row fetch failed: {e}")))?
            .ok_or_else(|| Error::engine("PRAGMA user_version returned no row"))?
            .get(0)
            .map_err(|e| Error::engine(format!("user_version decode failed: {e}")))?;

        if current >= SCHEMA_VERSION {
            return Ok(());
        }

        // Apply each migration whose 1-based version > current user_version.
        // Migrations are `execute_batch`-compatible (multiple `;`-separated
        // statements). After each one, bump `user_version` so a crash
        // mid-way resumes from the right slot on the next `init_schema`.
        for (idx, sql) in MIGRATIONS.iter().enumerate() {
            // `idx + 1` is the schema version this migration produces.
            // `usize → i64` via `try_from` keeps clippy happy and guards
            // the (impossible-in-practice) overflow case explicitly.
            let ver = i64::try_from(idx + 1)
                .map_err(|_| Error::engine("MIGRATIONS length overflows i64"))?;
            if ver <= current {
                continue;
            }
            conn.execute_batch(sql)
                .await
                .map_err(|e| Error::engine(format!("migration {ver} failed: {e}")))?;
            conn.execute_batch(&format!("PRAGMA user_version = {ver}"))
                .await
                .map_err(|e| Error::engine(format!("set user_version = {ver} failed: {e}")))?;
        }

        Ok(())
    }

    // ── Page CRUD — slice 5 ───────────────────────────────────────────────
    // Same contract as PostgresEngine slice 4b. Differences live only in
    // dialect: `?N` placeholders, `INSERT … ON CONFLICT(source_id, slug)
    // DO UPDATE` (SQLite spelling), `LIMIT -1` as the unbounded sentinel.

    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> Result<Option<Page>> {
        if opts.include_deleted {
            // FixMe: soft-delete column lands in slice 6.5a; the schema has
            // no `deleted_at` to filter on, so honoring this flag would be a
            // lie. Surface it explicitly until the column exists.
            return Err(Error::unsupported(
                "GetPageOpts.include_deleted requires a deleted_at column (slice 6.5a)",
            ));
        }
        let conn = self.conn()?;
        let mut rows = conn
            .query(
                "SELECT id, slug, type, page_kind, title, compiled_truth, timeline \
                 FROM pages WHERE slug = ?1",
                ::libsql::params![slug],
            )
            .await
            .map_err(|e| Error::engine(format!("get_page query failed: {e}")))?;

        match rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("get_page row fetch failed: {e}")))?
        {
            Some(row) => Ok(Some(row_to_page(&row)?)),
            None => Ok(None),
        }
    }

    async fn put_page(&self, slug: &str, input: &PageInput) -> Result<Page> {
        let conn = self.conn()?;
        // Upsert keyed by (source_id, slug). source_id defaults to 'default'
        // in the migration. SQLite's ON CONFLICT names the conflicting
        // columns directly (no constraint-name spelling).
        // RETURNING is supported by SQLite 3.35+, which libsql ships.
        let mut rows = conn
            .query(
                "INSERT INTO pages (slug, type, title, compiled_truth) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(source_id, slug) DO UPDATE SET \
                     type = excluded.type, \
                     title = excluded.title, \
                     compiled_truth = excluded.compiled_truth, \
                     updated_at = CURRENT_TIMESTAMP \
                 RETURNING id, slug, type, page_kind, title, compiled_truth, timeline",
                ::libsql::params![slug, input.page_type.clone(), input.title.clone(), input.compiled_truth.clone()],
            )
            .await
            .map_err(|e| Error::engine(format!("put_page upsert failed: {e}")))?;

        let row = rows
            .next()
            .await
            .map_err(|e| Error::engine(format!("put_page row fetch failed: {e}")))?
            .ok_or_else(|| Error::engine("put_page RETURNING produced no row"))?;
        row_to_page(&row)
    }

    async fn delete_page(&self, slug: &str) -> Result<()> {
        let conn = self.conn()?;
        // No-op on missing slug (matches PG + InMemory contracts).
        conn.execute(
            "DELETE FROM pages WHERE slug = ?1",
            ::libsql::params![slug],
        )
        .await
        .map_err(|e| Error::engine(format!("delete_page failed: {e}")))?;
        Ok(())
    }

    async fn list_pages(&self, filters: &PageFilters) -> Result<Vec<Page>> {
        let conn = self.conn()?;
        // SQLite parses `?1 IS NULL` happily; we bind the optional type and
        // an optional limit (NULL → no upper bound, matching the PG
        // behaviour via LIMIT -1 trick).
        let limit: i64 = filters
            .limit
            .map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
        let mut rows = conn
            .query(
                "SELECT id, slug, type, page_kind, title, compiled_truth, timeline \
                 FROM pages \
                 WHERE (?1 IS NULL OR type = ?1) \
                 ORDER BY id ASC \
                 LIMIT ?2",
                ::libsql::params![filters.page_type.clone(), limit],
            )
            .await
            .map_err(|e| Error::engine(format!("list_pages query failed: {e}")))?;

        let mut out = Vec::new();
        loop {
            let next = rows
                .next()
                .await
                .map_err(|e| Error::engine(format!("list_pages row fetch failed: {e}")))?;
            match next {
                Some(row) => out.push(row_to_page(&row)?),
                None => break,
            }
        }
        Ok(out)
    }

    async fn resolve_slugs(&self, partial: &str) -> Result<Vec<String>> {
        // FixMe: fuzzy LIKE %partial% lands in slice 6.5c — keep exact
        // matching in lockstep with PostgresEngine so the two engines are
        // observationally identical at this slice.
        let conn = self.conn()?;
        let mut rows = conn
            .query(
                "SELECT slug FROM pages WHERE slug = ?1 ORDER BY slug ASC",
                ::libsql::params![partial],
            )
            .await
            .map_err(|e| Error::engine(format!("resolve_slugs query failed: {e}")))?;

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
}

/// Decode one `pages` row into [`Page`]. Mirrors `postgres::row_to_page`
/// but uses libsql's positional `Row::get` API instead of sqlx's named
/// `try_get`. Column order is the literal SELECT projection above.
fn row_to_page(row: &::libsql::Row) -> Result<Page> {
    let id: i64 = row
        .get(0)
        .map_err(|e| Error::engine(format!("row decode id: {e}")))?;
    let slug: String = row
        .get(1)
        .map_err(|e| Error::engine(format!("row decode slug: {e}")))?;
    let page_type: String = row
        .get(2)
        .map_err(|e| Error::engine(format!("row decode type: {e}")))?;
    let page_kind_str: String = row
        .get(3)
        .map_err(|e| Error::engine(format!("row decode page_kind: {e}")))?;
    let title: String = row
        .get(4)
        .map_err(|e| Error::engine(format!("row decode title: {e}")))?;
    let compiled_truth: String = row
        .get(5)
        .map_err(|e| Error::engine(format!("row decode compiled_truth: {e}")))?;
    let timeline: String = row
        .get(6)
        .map_err(|e| Error::engine(format!("row decode timeline: {e}")))?;

    let page_kind = decode_page_kind(&page_kind_str)?;
    let id_u64 = u64::try_from(id)
        .map_err(|_| Error::engine(format!("page id {id} negative; corrupt row")))?;

    Ok(Page {
        id: id_u64,
        slug,
        page_type,
        page_kind,
        title,
        compiled_truth,
        timeline,
    })
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
