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
    page_sort_sql, BrainEngine, EngineConfig, EngineKind, GetPageOpts, Page, PageFilters,
    PageInput,
};
use crate::error::{Error, Result};
use crate::types::{CRMode, EffectiveDateSource, FindDuplicatePageOpts, PageKind};

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

/// Ordered list of migrations. Index `i` is applied when `user_version <= i`,
/// then `user_version` is set to `i + 1`. Append-only — never reorder.
const MIGRATIONS: &[&str] = &[MIGRATION_0001, MIGRATION_0002, MIGRATION_0003];

/// Highest schema version we know how to produce. Equals `MIGRATIONS.len()`.
/// Guarded via `PRAGMA user_version`. Hand-kept in sync with `MIGRATIONS`
/// because `len() as i64` is not const-evaluable in stable Rust and casting
/// `usize → i64` trips `clippy::cast_possible_wrap`.
const SCHEMA_VERSION: i64 = 3;

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
        // Slice 6a S6-T4: full 30-column projection backed by
        // `full_row_to_page`, with `deleted_at` filtering and `source_id`
        // scoping that mirror `soft_delete_page` / `find_duplicate_page`.
        //
        // Filters:
        // - `slug = ?1` (primary key after source_id scoping)
        // - `(?2 IS NULL OR source_id = ?2)` – optional scope match
        // - `(?3 = 1 OR deleted_at IS NULL)` – default hides soft-deleted
        //
        // `include_deleted` is bound as an INTEGER (0/1) because libsql /
        // SQLite type affinity coerces TEXT booleans loosely; explicit i64
        // avoids any surprise.
        let conn = self.conn()?;
        let include_deleted_flag: i64 = i64::from(opts.include_deleted);
        let source_id_param = opts.source_id.clone();
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

    #[allow(clippy::too_many_lines)] // Dynamic SQL builder for 9 filters — extracting helpers is deferred until S6-T5c lands tag JOIN.
    async fn list_pages(&self, filters: &PageFilters) -> Result<Vec<Page>> {
        let conn = self.conn()?;

        // ── Empty source_ids short-circuit ───────────────────────────────
        // `source_ids: Some(vec![])` semantically means "match no source",
        // which would produce an invalid `IN ()` SQL clause.  Return empty
        // immediately so we never round-trip a degenerate query.
        if let Some(ids) = filters.source_ids.as_ref() {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
        }

        // ── Build dynamic SQL ────────────────────────────────────────────
        // Full 30-column projection, same column order as `full_row_to_page`.
        // We alias `pages AS p` so the whitelisted ORDER BY fragments from
        // `page_sort_sql` (which use `p.` prefixes for future JOIN reuse)
        // bind cleanly.
        let mut sql = "SELECT p.id, p.slug, p.type, p.page_kind, p.title, p.compiled_truth, \
                       p.timeline, p.frontmatter, p.content_hash, p.emotional_weight, \
                       p.created_at, p.updated_at, p.deleted_at, p.last_retrieved_at, \
                       p.effective_date, p.effective_date_source, p.import_filename, \
                       p.salience_touched_at, p.salience_score, p.generation, p.embedding, \
                       p.chunker_version, p.source_path, p.source_id, p.source_kind, \
                       p.source_uri, p.ingested_via, p.ingested_at, \
                       p.contextual_retrieval_mode, p.corpus_generation \
                       FROM pages AS p WHERE 1=1"
            .to_owned();

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

        // Filter: source_id (single)
        let source_id_param = if filters.source_id.is_some() {
            let frag = format!(" AND p.source_id = ?{param_idx}");
            param_idx += 1;
            Some(frag)
        } else {
            None
        };
        if let Some(ref frag) = source_id_param {
            sql.push_str(frag);
        }

        // Filter: source_ids (IN clause, dynamic length)
        // Empty vec was already short-circuited above; here len >= 1.
        let source_id_in_param = if let Some(ids) = filters.source_ids.as_ref() {
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

        // Filter: include_deleted (default = false → exclude soft-deleted rows)
        if !filters.include_deleted {
            sql.push_str(" AND p.deleted_at IS NULL");
        }

        // ORDER BY — default UpdatedDesc when sort is None
        let sort_sql = page_sort_sql(filters.sort.unwrap_or_default());
        sql.push_str(" ORDER BY ");
        sql.push_str(sort_sql);

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
        // bumps above — page_type → slug_prefix → source_id → source_ids
        // → updated_after → limit → offset.  Reordering either side without
        // the other will silently misbind.
        let mut param_vals: Vec<::libsql::Value> = Vec::new();

        if let Some(ref pt) = filters.page_type {
            param_vals.push(::libsql::Value::from(pt.clone()));
        }
        if let Some(ref prefix) = filters.slug_prefix {
            param_vals.push(::libsql::Value::from(prefix.clone()));
        }
        if let Some(ref sid) = filters.source_id {
            param_vals.push(::libsql::Value::from(sid.clone()));
        }
        if let Some(ref ids) = filters.source_ids {
            for id in ids {
                param_vals.push(::libsql::Value::from(id.clone()));
            }
        }
        if let Some(ref ts) = filters.updated_after {
            param_vals.push(::libsql::Value::from(ts.clone()));
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

    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> Result<Option<Page>> {
        let conn = self.conn()?;
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
            Some(row) => Ok(Some(full_row_to_page(&row)?)),
            None => Ok(None),
        }
    }

    async fn soft_delete_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = self.conn()?;
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

    // S2 placeholder: only the 7 columns the legacy SELECT projects are
    // populated. Slice 6a S3 widens this SELECT + decoder to cover the new
    // 0002 columns (frontmatter/content_hash/timestamps/effective-date chain/
    // salience/source/contextual-retrieval). Until then the new fields land
    // with safe defaults so callers can still observe the legacy 7-column
    // surface without behaviour change.
    //
    // Slice 6a S5 added five more columns to `Page` (`last_retrieved_at`,
    // `generation`, `embedding`, `chunker_version`, `source_path`). Same
    // placeholder pattern: defaults that mirror the PG defaults
    // (`generation = 1`, `chunker_version = 1`, optionals `None`) so the
    // shape is correct even though the SELECT still does not project them.
    // Slice 6a S6 (libsql leg) replaces this with a real read.
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
        source_id: "default".to_string(),
        source_kind: None,
        source_uri: None,
        ingested_via: None,
        ingested_at: None,
        contextual_retrieval_mode: None,
        corpus_generation: None,
    })
}

/// Decode one full-width `pages` row into [`Page`]. Column order is the SELECT
/// projection used by `find_duplicate_page` and mirrors the complete 6a page
/// shape.
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
    let chunker_version = chunker_version_raw
        .map_or(Ok(1), |v| i32::try_from(v).map_err(|_| Error::engine(format!("chunker_version {v} overflows i32"))))?;
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
