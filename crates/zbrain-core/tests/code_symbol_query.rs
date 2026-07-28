//! 1-6-7-10-3: code-graph symbol queries — `find_code_def` / `find_code_refs`.
//!
//! Mirrors TS `src/commands/code-def.ts` (`findCodeDef`) and
//! `src/commands/code-refs.ts` (`findCodeRefs`). The TS ops route through
//! `engine.executeRaw`; the Rust replacements are typed `BrainEngine` methods
//! (no `execute_raw`), implemented for InMemory / Libsql / Postgres.

use libsql::Builder;
use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};
use zbrain_core::import::{ChunkInput, ChunkSource, CodeSymbolQueryOpts};
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

/// Seed a `page_kind = 'code'` page with a `file` frontmatter key; returns the
/// created page id so callers can attach `content_chunks` rows.
async fn seed_code_page(engine: &LibsqlEngine, slug: &str, file: &str) -> i64 {
    let page = engine
        .put_page(
            slug,
            Some("default"),
            &PageInput {
                page_type: "code".to_string(),
                title: slug.to_string(),
                compiled_truth: String::new(),
                frontmatter: Some(json!({ "file": file })),
                page_kind: Some(PageKind::Code),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    page.id as i64
}

/// Insert one `content_chunks` row via a raw connection to the same file.
async fn seed_chunk(
    path: &std::path::Path,
    page_id: i64,
    chunk_text: &str,
    language: &str,
    symbol_name: &str,
    symbol_type: &str,
    start_line: i64,
    end_line: i64,
) {
    let conn = Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    conn.execute(
        "INSERT INTO content_chunks \
         (page_id, chunk_index, chunk_text, chunk_source, language, symbol_name, symbol_type, start_line, end_line) \
         VALUES (?1, 0, ?2, 'text', ?3, ?4, ?5, ?6, ?7)",
        libsql::params![
            page_id,
            chunk_text,
            language,
            symbol_name,
            symbol_type,
            start_line,
            end_line
        ],
    )
    .await
    .unwrap();
}

/// TRACER BULLET (RED→GREEN): a `function` symbol on a code page is returned
/// by `find_code_def`, joined to its page slug + `frontmatter->>'file'`.
#[tokio::test]
async fn libsql_find_code_def_returns_definition() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs").await;
    seed_chunk(
        temp.path(),
        page_id,
        "fn render() { draw(); }",
        "Rust",
        "render",
        "function",
        10,
        20,
    )
    .await;

    let defs = engine
        .find_code_def("render", &CodeSymbolQueryOpts::default())
        .await
        .expect("find_code_def should succeed");

    assert_eq!(defs.len(), 1, "expected exactly one definition");
    let d = &defs[0];
    assert_eq!(d.slug, "foo");
    assert_eq!(d.file.as_deref(), Some("src/foo.rs"));
    assert_eq!(d.language.as_deref(), Some("Rust"));
    assert_eq!(d.symbol_type.as_deref(), Some("function"));
    assert_eq!(d.start_line, Some(10));
    assert_eq!(d.end_line, Some(20));
    assert!(d.snippet.starts_with("fn render()"), "snippet: {}", d.snippet);
}

/// InMemory mirror of the libsql tracer: `find_code_def` scans the in-memory
/// chunk store joined to code-kind pages and returns the definition.
#[tokio::test]
async fn inmemory_find_code_def_returns_definition() {
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
                chunk_text: "fn render() { draw(); }".to_string(),
                chunk_source: ChunkSource::FencedCode,
                embedding: None,
                token_count: None,
                language: Some("Rust".to_string()),
                symbol_name: Some("render".to_string()),
                symbol_type: Some("function".to_string()),
                start_line: Some(10),
                end_line: Some(20),
                parent_symbol_path: Vec::new(),
                symbol_name_qualified: None,
            }],
        )
        .await
        .unwrap();

    let defs = engine
        .find_code_def("render", &CodeSymbolQueryOpts::default())
        .await
        .expect("find_code_def should succeed");

    assert_eq!(defs.len(), 1);
    let d = &defs[0];
    assert_eq!(d.slug, "foo");
    assert_eq!(d.file.as_deref(), Some("src/foo.rs"));
    assert_eq!(d.symbol_type.as_deref(), Some("function"));
    assert_eq!(d.start_line, Some(10));
}

/// `find_code_refs` returns every chunk whose text contains the symbol
/// (substring / case-insensitive), restricted to code pages.
#[tokio::test]
async fn libsql_find_code_refs_returns_references() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "bar", "src/bar.rs").await;
    seed_chunk(
        temp.path(),
        page_id,
        "let x = render(); render();", // "render" appears twice → one row
        "Rust",
        "x",
        "local",
        5,
        5,
    )
    .await;

    let refs = engine
        .find_code_refs("render", &CodeSymbolQueryOpts::default())
        .await
        .expect("find_code_refs should succeed");

    assert_eq!(refs.len(), 1, "expected one referencing chunk");
    let r = &refs[0];
    assert_eq!(r.slug, "bar");
    assert_eq!(r.file.as_deref(), Some("src/bar.rs"));
    assert_eq!(r.symbol_name.as_deref(), Some("x"));
    assert!(r.snippet.contains("render"));
}

/// InMemory mirror of the refs query.
#[tokio::test]
async fn inmemory_find_code_refs_returns_references() {
    let _guard = libsql_test_guard();
    let engine = InMemoryEngine::new();
    engine
        .put_page(
            "bar",
            Some("default"),
            &PageInput {
                page_kind: Some(PageKind::Code),
                frontmatter: Some(json!({ "file": "src/bar.rs" })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    engine
        .upsert_chunks(
            "bar",
            &[ChunkInput {
                chunk_index: 0,
                chunk_text: "let x = render();".to_string(),
                chunk_source: ChunkSource::FencedCode,
                embedding: None,
                token_count: None,
                language: Some("Rust".to_string()),
                symbol_name: Some("x".to_string()),
                symbol_type: Some("local".to_string()),
                start_line: Some(5),
                end_line: Some(5),
                parent_symbol_path: Vec::new(),
                symbol_name_qualified: None,
            }],
        )
        .await
        .unwrap();

    let refs = engine
        .find_code_refs("render", &CodeSymbolQueryOpts::default())
        .await
        .expect("find_code_refs should succeed");

    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].slug, "bar");
    assert!(refs[0].snippet.contains("render"));
}

/// `find_code_def` only returns rows whose `symbol_type` is a definition kind;
/// a usage-site chunk (e.g. `local`) with the same `symbol_name` is excluded.
#[tokio::test]
async fn libsql_find_code_def_excludes_non_def_symbol_type() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs").await;
    seed_chunk(temp.path(), page_id, "fn render() {}", "Rust", "render", "function", 10, 20).await;
    seed_chunk(temp.path(), page_id, "let render = 1;", "Rust", "render", "local", 30, 30).await;

    let defs = engine
        .find_code_def("render", &CodeSymbolQueryOpts::default())
        .await
        .unwrap();
    assert_eq!(defs.len(), 1, "only the function def should match");
    assert_eq!(defs[0].symbol_type.as_deref(), Some("function"));
}

/// `find_code_def` ignores symbols on non-code pages (`page_kind != 'code'`).
#[tokio::test]
async fn libsql_find_code_def_excludes_non_code_pages() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    // Markdown page with the same symbol_name + def type → must be skipped.
    let md = engine
        .put_page(
            "notes",
            Some("default"),
            &PageInput {
                page_kind: Some(PageKind::Markdown),
                frontmatter: Some(json!({ "file": "notes.md" })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    seed_chunk(temp.path(), md.id as i64, "fn render() {}", "Rust", "render", "function", 1, 2).await;

    let defs = engine
        .find_code_def("render", &CodeSymbolQueryOpts::default())
        .await
        .unwrap();
    assert!(defs.is_empty(), "markdown page must not yield a code def");
}

/// `--lang` restricts both def and refs to a single language.
#[tokio::test]
async fn libsql_find_code_def_language_filter() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs").await;
    seed_chunk(temp.path(), page_id, "fn render() {}", "Rust", "render", "function", 10, 20).await;
    seed_chunk(temp.path(), page_id, "def render(): pass", "Python", "render", "function", 1, 1).await;

    let rust_only = engine
        .find_code_def(
            "render",
            &CodeSymbolQueryOpts {
                limit: None,
                language: Some("Rust".to_string()),
            },
        )
        .await
        .unwrap();
    assert_eq!(rust_only.len(), 1);
    assert_eq!(rust_only[0].language.as_deref(), Some("Rust"));
}

/// `find_code_def` orders functions before classes even when the class has a
/// smaller start_line (deterministic type-rank ordering, mirroring TS).
#[tokio::test]
async fn libsql_find_code_def_orders_by_type_rank() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let page_id = seed_code_page(&engine, "foo", "src/foo.rs").await;
    // Same symbol_name "render", two def kinds. Class has the smaller line
    // number, but function must still rank first.
    seed_chunk(temp.path(), page_id, "class render {}", "Rust", "render", "class", 5, 6).await;
    seed_chunk(temp.path(), page_id, "fn render() {}", "Rust", "render", "function", 50, 60).await;

    let defs = engine
        .find_code_def("render", &CodeSymbolQueryOpts::default())
        .await
        .unwrap();
    assert_eq!(defs.len(), 2, "both def kinds of 'render' match");
    assert_eq!(defs[0].symbol_type.as_deref(), Some("function"));
    assert_eq!(defs[0].start_line, Some(50));
    assert_eq!(defs[1].symbol_type.as_deref(), Some("class"));
    assert_eq!(defs[1].start_line, Some(5));
}

// ─────────────────────────────────────────────────────────────────────────
// Postgres integration tests
// ─────────────────────────────────────────────────────────────────────────

/// Insert a `content_chunks` row referencing an already-created page. Postgres
/// enforces `page_id` FK, so the page must exist first (seeded via the engine's
/// `put_page`).
async fn pg_seed_chunk(
    url: &str,
    page_id: i64,
    chunk_text: &str,
    language: &str,
    symbol_name: &str,
    symbol_type: &str,
    start_line: i32,
    end_line: i32,
) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("seed pool");
    sqlx::query(
        "INSERT INTO content_chunks \
         (page_id, chunk_index, chunk_text, chunk_source, language, symbol_name, symbol_type, start_line, end_line) \
         VALUES ($1, 0, $2, 'text', $3, $4, $5, $6, $7)",
    )
    .bind(page_id)
    .bind(chunk_text)
    .bind(language)
    .bind(symbol_name)
    .bind(symbol_type)
    .bind(start_line)
    .bind(end_line)
    .execute(&pool)
    .await
    .expect("seed chunk");
    pool.close().await;
}

#[tokio::test]
async fn postgres_find_code_def_returns_definition() {
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
    pg_seed_chunk(
        &fix.url,
        page.id as i64,
        "fn render() { draw(); }",
        "Rust",
        "render",
        "function",
        10,
        20,
    )
    .await;

    let defs = fix
        .engine
        .find_code_def("render", &CodeSymbolQueryOpts::default())
        .await
        .expect("find_code_def should succeed");
    assert_eq!(defs.len(), 1);
    let d = &defs[0];
    assert_eq!(d.slug, "foo");
    assert_eq!(d.file.as_deref(), Some("src/foo.rs"));
    assert_eq!(d.language.as_deref(), Some("Rust"));
    assert_eq!(d.symbol_type.as_deref(), Some("function"));
    assert_eq!(d.start_line, Some(10));
    assert_eq!(d.end_line, Some(20));
}

#[tokio::test]
async fn postgres_find_code_refs_returns_references() {
    let _guard = libsql_test_guard();
    let fix = PgFixture::start().await;
    let page = fix
        .engine
        .put_page(
            "bar",
            Some("default"),
            &PageInput {
                page_kind: Some(PageKind::Code),
                frontmatter: Some(json!({ "file": "src/bar.rs" })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    pg_seed_chunk(
        &fix.url,
        page.id as i64,
        "let x = render(); render();",
        "Rust",
        "x",
        "local",
        5,
        5,
    )
    .await;

    let refs = fix
        .engine
        .find_code_refs("render", &CodeSymbolQueryOpts::default())
        .await
        .expect("find_code_refs should succeed");
    assert_eq!(refs.len(), 1);
    let r = &refs[0];
    assert_eq!(r.slug, "bar");
    assert_eq!(r.file.as_deref(), Some("src/bar.rs"));
    assert_eq!(r.symbol_name.as_deref(), Some("x"));
    assert!(r.snippet.contains("render"));
}
