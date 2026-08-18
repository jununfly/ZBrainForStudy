//! 1-6-7-10-4: code-graph symbol disambiguation — `disambiguate_symbol`.
//!
//! Mirrors TS `disambiguateSymbol` (`src/core/code-intel/recursive-walk.ts:77`),
//! the bare-name → `symbol_name_qualified` resolver that `runRecursiveWalk` uses
//! to locate its start symbol. The TS op routes through `engine.executeRaw`;
//! the Rust replacement is a typed `BrainEngine` method (no `execute_raw`),
//! implemented for InMemory / Libsql / Postgres. Op wiring is deferred to the
//! endgame cutover (consistent with 1-6-7-10-2 / -3).

use libsql::Builder;
use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, CreateSourceInput, EngineConfig, InMemoryEngine, PageInput};
use zbrain_core::import::{ChunkInput, ChunkSource, SymbolDisambiguation};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::PageKind;

mod support;
use support::pg_fixture::PgFixture;

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


/// Fresh LibsqlEngine backed by a temp file.
async fn temp_engine() -> (NamedTempFile, LibsqlEngine) {
    let temp = NamedTempFile::new().expect("alloc temp db file");
    let path = temp.path().to_string_lossy().to_string();
    let config = EngineConfig {
        database_path: Some(path),
        database_url: None,
    };
    let engine = LibsqlEngine::new();
    engine.connect(&config).await.unwrap();
    engine.init_schema().await.unwrap();
    (temp, engine)
}

/// Seed a `page_kind = 'code'` page owned by `source` with a `file` frontmatter
/// key; returns the created page id so callers can attach `content_chunks`.
async fn seed_code_page(engine: &LibsqlEngine, slug: &str, file: &str, source: &str) -> i64 {
    let page = engine
        .put_page(
            slug,
            Some(source),
            &PageInput {
                frontmatter: Some(json!({ "file": file })),
                page_kind: Some(PageKind::Code),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    page.id as i64
}

/// Insert one `content_chunks` row via a raw connection to the same file,
/// including `symbol_name_qualified` (the column the disambiguator keys on).
async fn seed_chunk(
    path: &std::path::Path,
    page_id: i64,
    symbol_name: &str,
    symbol_name_qualified: Option<&str>,
) {
    let conn = Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    conn.execute(
        "INSERT INTO content_chunks \
         (page_id, chunk_index, chunk_text, chunk_source, language, symbol_name, symbol_type, symbol_name_qualified, start_line, end_line) \
         VALUES (?1, 0, ?2, 'text', 'Rust', ?3, 'function', ?4, 1, 2)",
        libsql::params![
            page_id,
            format!("def {symbol_name}"),
            symbol_name,
            symbol_name_qualified
        ],
    )
    .await
    .unwrap();
}

/// TRACER BULLET (RED→GREEN): an exact `symbol_name` match yields its
/// `symbol_name_qualified` in `matches`, with empty `suggestions`.
#[tokio::test]
async fn libsql_disambiguate_exact_returns_qualified() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs", "default").await;
    seed_chunk(temp.path(), page_id, "render", Some("App::render")).await;

    let res = engine
        .disambiguate_symbol("render", "default")
        .await
        .expect("disambiguate_symbol should succeed");

    assert_eq!(res.matches, vec!["App::render".to_string()]);
    assert!(res.suggestions.is_empty(), "suggestions must be empty on exact hit");
}

/// Multiple exact matches are all returned (recursive walk treats this as
/// `ambiguous`).
#[tokio::test]
async fn libsql_disambiguate_multiple_exact_matches() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs", "default").await;
    seed_chunk(temp.path(), page_id, "render", Some("App::render")).await;
    seed_chunk(temp.path(), page_id, "render", Some("Lib::render")).await;

    let res = engine
        .disambiguate_symbol("render", "default")
        .await
        .unwrap();

    assert_eq!(res.matches.len(), 2, "both qualified names should surface");
    assert!(res.matches.contains(&"App::render".to_string()));
    assert!(res.matches.contains(&"Lib::render".to_string()));
    assert!(res.suggestions.is_empty());
}

/// No exact hit → fuzzy `did_you_mean` candidates via `ILIKE '%bare%'`
/// (case-insensitive). `matches` stays empty.
#[tokio::test]
async fn libsql_disambiguate_fuzzy_suggestions_when_no_exact() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    // symbol_name is "renderWidget", qualified "App::RenderWidget" — neither
    // equals the bare "render", but the qualified name contains it (case-insensitively).
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs", "default").await;
    seed_chunk(temp.path(), page_id, "renderWidget", Some("App::RenderWidget")).await;

    let res = engine
        .disambiguate_symbol("render", "default")
        .await
        .unwrap();

    assert!(res.matches.is_empty(), "no exact match expected");
    assert_eq!(res.suggestions, vec!["App::RenderWidget".to_string()]);
}

/// `symbol_name_qualified = bare` also counts as an exact hit (not just
/// `symbol_name`).
#[tokio::test]
async fn libsql_disambiguate_matches_on_qualified_exact() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs", "default").await;
    // symbol_name differs, but symbol_name_qualified equals the bare input.
    seed_chunk(temp.path(), page_id, "somethingElse", Some("render")).await;

    let res = engine
        .disambiguate_symbol("render", "default")
        .await
        .unwrap();

    assert_eq!(res.matches, vec!["render".to_string()]);
}

/// Scoping: a chunk in a different source is excluded even with a matching
/// `symbol_name`.
#[tokio::test]
async fn libsql_disambiguate_scoped_to_source() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    engine
        .create_source(&CreateSourceInput {
            id: "other".to_string(),
            name: "other".to_string(),
            config: None,
        })
        .await
        .unwrap();
    let default_page = seed_code_page(&engine, "foo", "src/foo.rs", "default").await;
    let other_page = seed_code_page(&engine, "bar", "src/bar.rs", "other").await;
    seed_chunk(temp.path(), default_page, "render", Some("App::render")).await;
    seed_chunk(temp.path(), other_page, "render", Some("Other::render")).await;

    let res_default = engine
        .disambiguate_symbol("render", "default")
        .await
        .unwrap();
    assert_eq!(res_default.matches, vec!["App::render".to_string()]);

    let res_other = engine.disambiguate_symbol("render", "other").await.unwrap();
    assert_eq!(res_other.matches, vec!["Other::render".to_string()]);
}

// ─────────────────────────────────────────────────────────────────────────
// InMemory mirrors
// ─────────────────────────────────────────────────────────────────────────

/// InMemory mirror of the libsql tracer.
#[tokio::test]
async fn inmemory_disambiguate_exact_returns_qualified() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    engine
        .put_page(
            "foo",
            Some("default"),
            &PageInput {
                page_kind: Some(PageKind::Code),
                frontmatter: Some(json!({ "file": "src/foo.rs" })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    engine
        .upsert_chunks(
            "foo",
            &[ChunkInput {
                chunk_index: 0,
                chunk_text: "fn render() {}".to_string(),
                chunk_source: ChunkSource::FencedCode,
                embedding: None,
                embedding_multimodal: None,
                token_count: None,
                language: Some("Rust".to_string()),
                symbol_name: Some("render".to_string()),
                symbol_type: Some("function".to_string()),
                start_line: Some(1),
                end_line: Some(2),
                parent_symbol_path: Vec::new(),
                symbol_name_qualified: Some("App::render".to_string()),
            }],
        )
        .await
        .unwrap();

    let res = engine
        .disambiguate_symbol("render", "default")
        .await
        .unwrap();
    assert_eq!(res.matches, vec!["App::render".to_string()]);
    assert!(res.suggestions.is_empty());
}

/// InMemory mirror of the fuzzy `did_you_mean` path.
#[tokio::test]
async fn inmemory_disambiguate_fuzzy_suggestions_when_no_exact() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    engine
        .put_page(
            "foo",
            Some("default"),
            &PageInput {
                page_kind: Some(PageKind::Code),
                frontmatter: Some(json!({ "file": "src/foo.rs" })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    engine
        .upsert_chunks(
            "foo",
            &[ChunkInput {
                chunk_index: 0,
                chunk_text: "fn renderWidget() {}".to_string(),
                chunk_source: ChunkSource::FencedCode,
                embedding: None,
                embedding_multimodal: None,
                token_count: None,
                language: Some("Rust".to_string()),
                symbol_name: Some("renderWidget".to_string()),
                symbol_type: Some("function".to_string()),
                start_line: Some(1),
                end_line: Some(2),
                parent_symbol_path: Vec::new(),
                symbol_name_qualified: Some("App::RenderWidget".to_string()),
            }],
        )
        .await
        .unwrap();

    let res = engine
        .disambiguate_symbol("render", "default")
        .await
        .unwrap();
    assert!(res.matches.is_empty());
    assert_eq!(res.suggestions, vec!["App::RenderWidget".to_string()]);
}

// ─────────────────────────────────────────────────────────────────────────
// Postgres integration tests
// ─────────────────────────────────────────────────────────────────────────

/// Insert a `content_chunks` row referencing an already-created page.
async fn pg_seed_chunk(url: &str, page_id: i64, symbol_name: &str, symbol_name_qualified: Option<&str>) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("seed pool");
    sqlx::query(
        "INSERT INTO content_chunks \
         (page_id, chunk_index, chunk_text, chunk_source, language, symbol_name, symbol_type, symbol_name_qualified, start_line, end_line) \
         VALUES ($1, 0, $2, 'text', 'Rust', $3, 'function', $4, 1, 2)",
    )
    .bind(page_id)
    .bind(format!("def {symbol_name}"))
    .bind(symbol_name)
    .bind(symbol_name_qualified)
    .execute(&pool)
    .await
    .expect("seed chunk");
    pool.close().await;
}

#[tokio::test]
async fn postgres_disambiguate_exact_returns_qualified() {
    let _guard = libsql_test_guard();
    let fix = PgFixture::start().await;
    let page = fix
        .engine
        .put_page(
            "foo",
            Some("default"),
            &PageInput {
                page_kind: Some(PageKind::Code),
                frontmatter: Some(json!({ "file": "src/foo.rs" })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    pg_seed_chunk(&fix.url, page.id as i64, "render", Some("App::render")).await;

    let res = fix
        .engine
        .disambiguate_symbol("render", "default")
        .await
        .expect("disambiguate_symbol should succeed");
    assert_eq!(res.matches, vec!["App::render".to_string()]);
    assert!(res.suggestions.is_empty());
}

#[tokio::test]
async fn postgres_disambiguate_fuzzy_suggestions_when_no_exact() {
    let _guard = libsql_test_guard();
    let fix = PgFixture::start().await;
    let page = fix
        .engine
        .put_page(
            "foo",
            Some("default"),
            &PageInput {
                page_kind: Some(PageKind::Code),
                frontmatter: Some(json!({ "file": "src/foo.rs" })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    pg_seed_chunk(&fix.url, page.id as i64, "renderWidget", Some("App::RenderWidget")).await;

    let res = fix
        .engine
        .disambiguate_symbol("render", "default")
        .await
        .unwrap();
    assert!(res.matches.is_empty());
    assert_eq!(res.suggestions, vec!["App::RenderWidget".to_string()]);
}
