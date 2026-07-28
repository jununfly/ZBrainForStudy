//! Slice 5 — `LibsqlEngine` lifecycle integration tests.
//!
//! Mirror of `postgres_engine_lifecycle.rs` against the libsql (embedded
//! `SQLite`) backend. Unlike the Postgres suite, libsql needs no external
//! daemon: each test allocates its own temp file via `tempfile::NamedTempFile`
//! and tears it down on drop, so the tests run unconditionally in CI.
//!
//! Schema-verification queries (`sqlite_master`) talk to the file directly
//! through a second libsql connection, mirroring the PG side's "fresh pool"
//! pattern so internal `LibsqlEngine` state is never leaked.

use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, EngineKind};
use zbrain_core::libsql::LibsqlEngine;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}


/// Allocate a fresh temp file path. Returned `NamedTempFile` must outlive
/// the engine — dropping it deletes the underlying file.
fn temp_db() -> NamedTempFile {
    NamedTempFile::new().expect("alloc temp db file")
}

#[tokio::test]
async fn kind_reports_libsql() {
    let _guard = libsql_test_guard();
    let engine = LibsqlEngine::new();
    assert_eq!(engine.kind(), EngineKind::Libsql);
}

#[tokio::test]
async fn connect_succeeds_against_local_file() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect should succeed");
    engine
        .disconnect()
        .await
        .expect("disconnect should succeed");
}

#[tokio::test]
async fn connect_without_path_errors() {
    let _guard = libsql_test_guard();
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig::default();
    let result = engine.connect(&cfg).await;
    assert!(
        result.is_err(),
        "connect without database_path must error, got {result:?}"
    );
}

#[tokio::test]
async fn init_schema_creates_pages_and_sources_tables() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path_str.clone()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");

    // Verify schema landed by opening a side-channel connection to the
    // same file. Mirrors the PG suite's "fresh verification pool" pattern.
    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            libsql::params!["pages"],
        )
        .await
        .expect("pages existence query");
    let pages_row = rows.next().await.expect("rows iter");
    assert!(
        pages_row.is_some(),
        "pages table must exist after init_schema"
    );

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            libsql::params!["sources"],
        )
        .await
        .expect("sources existence query");
    let sources_row = rows.next().await.expect("rows iter");
    assert!(
        sources_row.is_some(),
        "sources table must exist after init_schema"
    );

    let mut rows = conn
        .query(
            "SELECT id FROM sources WHERE id = ?1",
            libsql::params!["default"],
        )
        .await
        .expect("default source query");
    let row = rows
        .next()
        .await
        .expect("rows iter")
        .expect("default source row must be seeded");
    let id: String = row.get(0).expect("decode id");
    assert_eq!(id, "default");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn init_schema_is_idempotent() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("first init_schema");
    engine
        .init_schema()
        .await
        .expect("second init_schema must be a no-op");
    engine.disconnect().await.expect("disconnect");
}

// ── Slice 6a — 0002 migration shape verification ────────────────────────
//
// These tests assert the 19 columns + 4 indexes + 2 triggers added by
// `0002_pages_full_columns.sql` actually land in the live SQLite file.
// They are intentionally schema-shape only (no engine-method coverage —
// that lives in `libsql_engine_page_crud.rs` once the methods exist).

/// All columns 0002 promises to add. Kept in sync with the migration file.
const SLICE_6A_COLUMNS: &[&str] = &[
    "frontmatter",
    "content_hash",
    "emotional_weight",
    "deleted_at",
    "effective_date",
    "effective_date_source",
    "import_filename",
    "chunker_version",
    "source_path",
    "source_kind",
    "source_uri",
    "ingested_via",
    "ingested_at",
    "salience_touched_at",
    "last_retrieved_at",
    "contextual_retrieval_mode",
    "corpus_generation",
    "generation",
    "embedding",
];

/// All indexes 0002 promises to add. Order doesn't matter.
const SLICE_6A_INDEXES: &[&str] = &[
    "idx_pages_source_id",
    "pages_deleted_at_purge_idx",
    "pages_coalesce_date_idx",
    "pages_last_retrieved_at_idx",
];

/// All triggers 0002 promises to add.
const SLICE_6A_TRIGGERS: &[&str] = &["bump_page_generation_insert", "bump_page_generation_update"];

#[tokio::test]
async fn slice_6a_migration_adds_all_pages_columns() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");

    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    // PRAGMA table_info returns one row per column: (cid, name, type, ...).
    let mut rows = conn
        .query("PRAGMA table_info(pages)", ())
        .await
        .expect("table_info query");
    let mut actual: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await.expect("rows iter") {
        let name: String = row.get(1).expect("decode column name");
        actual.push(name);
    }

    for expected in SLICE_6A_COLUMNS {
        assert!(
            actual.iter().any(|c| c == expected),
            "0002 must add column {expected:?} — table_info reports {actual:?}"
        );
    }
}

#[tokio::test]
async fn slice_6a_migration_creates_all_indexes() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");

    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='pages'",
            (),
        )
        .await
        .expect("index list query");
    let mut actual: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await.expect("rows iter") {
        let name: String = row.get(0).expect("decode index name");
        actual.push(name);
    }

    for expected in SLICE_6A_INDEXES {
        assert!(
            actual.iter().any(|i| i == expected),
            "0002 must create index {expected:?} — pages indexes are {actual:?}"
        );
    }
}

#[tokio::test]
async fn slice_6a_migration_creates_generation_triggers() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");

    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='pages'",
            (),
        )
        .await
        .expect("trigger list query");
    let mut actual: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await.expect("rows iter") {
        let name: String = row.get(0).expect("decode trigger name");
        actual.push(name);
    }

    for expected in SLICE_6A_TRIGGERS {
        assert!(
            actual.iter().any(|t| t == expected),
            "0002 must create trigger {expected:?} — pages triggers are {actual:?}"
        );
    }
}

#[tokio::test]
async fn slice_6a_generation_trigger_bumps_on_insert_and_update() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");

    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    // First INSERT — generation should become 1.
    conn.execute(
        "INSERT INTO pages (slug, type, title, compiled_truth) \
         VALUES ('a', 'doc', 'A', 'body a')",
        (),
    )
    .await
    .expect("insert page a");

    let mut rows = conn
        .query("SELECT generation FROM pages WHERE slug = 'a'", ())
        .await
        .expect("select gen for a");
    let first_insert_gen: i64 = rows
        .next()
        .await
        .expect("rows")
        .expect("row a exists")
        .get(0)
        .expect("decode gen");
    assert_eq!(first_insert_gen, 1, "first row should land at generation=1");

    // Second INSERT — generation should be 2 (MAX(generation) + 1).
    conn.execute(
        "INSERT INTO pages (slug, type, title, compiled_truth) \
         VALUES ('b', 'doc', 'B', 'body b')",
        (),
    )
    .await
    .expect("insert page b");

    let mut rows = conn
        .query("SELECT generation FROM pages WHERE slug = 'b'", ())
        .await
        .expect("select gen for b");
    let second_insert_gen: i64 = rows
        .next()
        .await
        .expect("rows")
        .expect("row b exists")
        .get(0)
        .expect("decode gen");
    assert_eq!(
        second_insert_gen, 2,
        "second row should land at generation=2"
    );

    // UPDATE allow-listed column on row a — generation should bump.
    conn.execute(
        "UPDATE pages SET compiled_truth = 'body a v2' WHERE slug = 'a'",
        (),
    )
    .await
    .expect("update a body");

    let mut rows = conn
        .query("SELECT generation FROM pages WHERE slug = 'a'", ())
        .await
        .expect("select gen for a after update");
    let after_allow_listed_update: i64 = rows
        .next()
        .await
        .expect("rows")
        .expect("row a exists")
        .get(0)
        .expect("decode gen");
    assert!(
        after_allow_listed_update > first_insert_gen,
        "allow-listed column UPDATE must bump generation: was {first_insert_gen}, now {after_allow_listed_update}"
    );

    // UPDATE non-allow-listed column (updated_at). Generation should NOT bump.
    // We touch `updated_at` directly to prove the allow-list works.
    conn.execute(
        "UPDATE pages SET updated_at = '2030-01-01T00:00:00' WHERE slug = 'a'",
        (),
    )
    .await
    .expect("update a updated_at");

    let mut rows = conn
        .query("SELECT generation FROM pages WHERE slug = 'a'", ())
        .await
        .expect("select gen for a after timestamp touch");
    let after_non_allow_listed_update: i64 = rows
        .next()
        .await
        .expect("rows")
        .expect("row a exists")
        .get(0)
        .expect("decode gen");
    assert_eq!(
        after_non_allow_listed_update, after_allow_listed_update,
        "non-allow-listed column UPDATE must NOT bump generation"
    );
}

// ── Slice 6a S3 — 0003 migration shape verification ────────────────────
//
// 0003_pages_salience_and_trigger.sql lands two things:
//   (1) a `salience_score REAL` column on `pages`, mirroring the PG baseline
//       that backs getSalienceScores / refreshPageSalience.
//   (2) a rebuilt `bump_page_generation_update` trigger whose UPDATE OF list
//       AND WHEN clause both cover the FULL 10-column allow-list from
//       `pglite-schema.ts` bump_page_generation_fn — adding timeline, type,
//       and page_kind on top of the 7 already in 0002.
//
// salience_score is deliberately OMITTED from the trigger: the PG baseline's
// allow-list excludes it (salience writes are read-time signal mutations
// that must not invalidate the query cache).

#[tokio::test]
async fn slice_6a_s3_migration_adds_salience_score_column() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");

    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    let mut rows = conn
        .query("PRAGMA table_info(pages)", ())
        .await
        .expect("table_info query");
    let mut found: Option<String> = None;
    while let Some(row) = rows.next().await.expect("rows iter") {
        let name: String = row.get(1).expect("decode column name");
        if name == "salience_score" {
            let ty: String = row.get(2).expect("decode column type");
            found = Some(ty);
            break;
        }
    }
    let ty = found.expect("0003 must add salience_score column");
    assert!(
        ty.eq_ignore_ascii_case("REAL"),
        "salience_score must be REAL, got {ty:?}"
    );
}

/// Full 10-column allow-list from PG `bump_page_generation_fn`.
/// 0003 must widen the `SQLite` trigger to cover ALL of these.
const SLICE_6A_S3_TRIGGER_COLUMNS: &[&str] = &[
    "compiled_truth",
    "timeline",
    "frontmatter",
    "deleted_at",
    "contextual_retrieval_mode",
    "title",
    "type",
    "page_kind",
    "corpus_generation",
    "content_hash",
];

#[tokio::test]
async fn slice_6a_s3_generation_trigger_covers_full_pg_allow_list() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");

    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    // sqlite_master.sql preserves the DDL verbatim — we grep it for both the
    // UPDATE OF column list and the WHEN clause references.
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master \
             WHERE type='trigger' AND name='bump_page_generation_update'",
            (),
        )
        .await
        .expect("trigger sql query");
    let row = rows
        .next()
        .await
        .expect("rows")
        .expect("bump_page_generation_update trigger must exist");
    let sql: String = row.get(0).expect("decode trigger sql");

    for col in SLICE_6A_S3_TRIGGER_COLUMNS {
        assert!(
            sql.contains(col),
            "trigger DDL must reference column {col:?} \
             (in both UPDATE OF list and WHEN clause), got:\n{sql}"
        );
    }
}

#[tokio::test]
async fn slice_6a_s3_trigger_bumps_on_type_and_skips_salience() {
    let _guard = libsql_test_guard();
    let path = temp_db();
    let path_str = path.path().to_string_lossy().into_owned();

    let engine = LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(path_str.clone()),
        })
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");

    let db = Builder::new_local(&path_str)
        .build()
        .await
        .expect("verification db");
    let conn = db.connect().expect("verification conn");

    conn.execute(
        "INSERT INTO pages (slug, type, title, compiled_truth) \
         VALUES ('s3', 'doc', 'S3', 'body s3')",
        (),
    )
    .await
    .expect("insert page s3");

    let mut rows = conn
        .query("SELECT generation FROM pages WHERE slug = 's3'", ())
        .await
        .expect("select gen for s3");
    let gen_after_insert: i64 = rows
        .next()
        .await
        .expect("rows")
        .expect("row s3 exists")
        .get(0)
        .expect("decode gen");

    // UPDATE `type` (newly added to allow-list in 0003) must bump generation.
    conn.execute("UPDATE pages SET type = 'note' WHERE slug = 's3'", ())
        .await
        .expect("update type");

    let mut rows = conn
        .query("SELECT generation FROM pages WHERE slug = 's3'", ())
        .await
        .expect("select gen after type update");
    let gen_after_type: i64 = rows
        .next()
        .await
        .expect("rows")
        .expect("row s3 exists")
        .get(0)
        .expect("decode gen");
    assert!(
        gen_after_type > gen_after_insert,
        "UPDATE type must bump generation (0003 widened allow-list): \
         was {gen_after_insert}, now {gen_after_type}"
    );

    // UPDATE `salience_score` must NOT bump generation — it is deliberately
    // excluded from the PG allow-list (read-time signal, no cache impact).
    conn.execute(
        "UPDATE pages SET salience_score = 0.5 WHERE slug = 's3'",
        (),
    )
    .await
    .expect("update salience_score");

    let mut rows = conn
        .query("SELECT generation FROM pages WHERE slug = 's3'", ())
        .await
        .expect("select gen after salience update");
    let gen_after_salience: i64 = rows
        .next()
        .await
        .expect("rows")
        .expect("row s3 exists")
        .get(0)
        .expect("decode gen");
    assert_eq!(
        gen_after_salience, gen_after_type,
        "UPDATE salience_score must NOT bump generation \
         (excluded from PG allow-list)"
    );
}
