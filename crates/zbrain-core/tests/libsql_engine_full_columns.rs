//! Slice #110-d - libsql full-column TS contract tests.
//!
//! Mirrors the PG #110-c contract suite against the libsql backend, with
//! SQLite-specific schema probes where PG uses `information_schema`. TS `PGLite` is
//! the source of truth: `put_page` does not persist `embedding` /
//! `last_retrieved_at`, `ingested_at` is server-stamped from provenance fields,
//! `frontmatter` defaults to `{}`, and `corpus_generation` is TEXT.

use libsql::Builder;
use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageInput};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::{EffectiveDateSource, PageKind};

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


async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile, String) {
    let path = NamedTempFile::new().expect("alloc temp db file");
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
    (engine, path, path_str)
}

fn base_input() -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: "Libsql Full Columns".to_string(),
        compiled_truth: "body".to_string(),
        timeline: Some("T1 -> T2".to_string()),
        frontmatter: Some(json!({"key": "value"})),
        content_hash: Some("sha256:abcdef".to_string()),
        page_kind: Some(PageKind::Markdown),
        effective_date: Some("2026-05-30".to_string()),
        effective_date_source: Some(EffectiveDateSource::Filename),
        import_filename: Some("contract.md".to_string()),
        chunker_version: Some(2),
        source_path: Some("/tmp/contract.md".to_string()),
        source_kind: Some("file".to_string()),
        source_uri: Some("file:///tmp/contract.md".to_string()),
        ingested_via: Some("cli".to_string()),
        ingested_at: None,
        last_retrieved_at: None,
        embedding: None,
    }
}

async fn raw_conn(path_str: &str) -> libsql::Connection {
    let db = Builder::new_local(path_str)
        .build()
        .await
        .expect("verification db");
    db.connect().expect("verification conn")
}

async fn page_column_info(path_str: &str, column: &str) -> (String, i64, Option<String>) {
    let conn = raw_conn(path_str).await;
    let mut rows = conn
        .query("PRAGMA table_info(pages)", ())
        .await
        .expect("table_info query");

    while let Some(row) = rows.next().await.expect("rows iter") {
        let name: String = row.get(1).expect("decode column name");
        if name == column {
            let column_type: String = row.get(2).expect("decode column type");
            let not_null: i64 = row.get(3).expect("decode notnull flag");
            let default_value: Option<String> = row.get(4).expect("decode default value");
            return (column_type, not_null, default_value);
        }
    }

    panic!("pages.{column} must exist in PRAGMA table_info(pages)");
}

#[tokio::test]
async fn put_page_persists_embedding_but_not_last_retrieved_at() {
    let _guard = libsql_test_guard();
    // G24: put_page now DOES persist caller-provided `embedding` (f32-LE blob),
    // so the page-level vector path has a write route. `last_retrieved_at`
    // remains owned by the retrieval-tracker path and is still NOT written here.
    let (engine, _tmp, _path_str) = init_clean_engine().await;
    let mut input = base_input();
    let emb = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    input.embedding = Some(emb.clone());
    input.last_retrieved_at = Some("2026-05-30T01:02:03+00:00".to_string());

    let inserted = engine
        .put_page("write-through", None, &input)
        .await
        .expect("put_page");
    assert_eq!(
        inserted.embedding,
        Some(emb.clone()),
        "put_page must persist caller-provided embedding (G24)"
    );
    assert_eq!(
        inserted.last_retrieved_at, None,
        "put_page must not persist caller-provided last_retrieved_at"
    );

    let fetched = engine
        .get_page("write-through", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("page must exist");
    assert_eq!(
        fetched.embedding,
        Some(emb),
        "get_page must observe embedding written through put_page (G24)"
    );
    assert_eq!(
        fetched.last_retrieved_at, None,
        "get_page must not observe last_retrieved_at written through put_page"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn put_page_embedding_none_preserves_existing_on_update() {
    let _guard = libsql_test_guard();
    // G24: an upsert with embedding = None must NOT clobber a previously stored
    // embedding (COALESCE-preserve semantics, matching PageInput.embedding doc).
    let (engine, _tmp, _path_str) = init_clean_engine().await;
    let mut input = base_input();
    let emb = vec![9u8, 8, 7, 6];
    input.embedding = Some(emb.clone());
    engine
        .put_page("preserve-emb", None, &input)
        .await
        .expect("put_page insert");

    let mut update = base_input();
    update.title = "updated title".to_string();
    update.embedding = None; // must preserve
    let updated = engine
        .put_page("preserve-emb", None, &update)
        .await
        .expect("put_page update");
    assert_eq!(updated.title, "updated title");
    assert_eq!(
        updated.embedding,
        Some(emb),
        "embedding=None on upsert must preserve the previously stored embedding"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn ingested_at_server_stamped_when_any_ingestion_metadata_present() {
    let _guard = libsql_test_guard();
    let (engine, _tmp, _path_str) = init_clean_engine().await;
    let cases = [
        ("source-kind-only", Some("file".to_string()), None, None),
        (
            "source-uri-only",
            None,
            Some("file:///tmp/contract.md".to_string()),
            None,
        ),
        ("ingested-via-only", None, None, Some("cli".to_string())),
    ];

    for (slug, source_kind, source_uri, ingested_via) in cases {
        let mut input = base_input();
        input.source_kind = source_kind;
        input.source_uri = source_uri;
        input.ingested_via = ingested_via;
        input.ingested_at = Some("1999-01-01T00:00:00+00:00".to_string());

        let page = engine.put_page(slug, None, &input).await.expect("put_page");
        let ingested_at = page
            .ingested_at
            .as_deref()
            .expect("any single provenance field must trigger server-stamped ingested_at");
        assert_ne!(
            ingested_at, "1999-01-01T00:00:00+00:00",
            "server stamp must ignore caller-provided input.ingested_at for {slug}"
        );
        assert!(
            ingested_at.contains('T'),
            "server stamp should be an ISO-8601-ish timestamp for {slug}, got {ingested_at:?}"
        );
    }

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn ingested_at_remains_none_without_ingestion_metadata() {
    let _guard = libsql_test_guard();
    let (engine, _tmp, _path_str) = init_clean_engine().await;
    let mut input = base_input();
    input.source_kind = None;
    input.source_uri = None;
    input.ingested_via = None;
    input.ingested_at = Some("1999-01-01T00:00:00+00:00".to_string());

    let page = engine
        .put_page("no-provenance", None, &input)
        .await
        .expect("put_page");
    assert_eq!(
        page.ingested_at, None,
        "without ingestion metadata, put_page must not persist input.ingested_at"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn frontmatter_defaults_to_empty_object_when_omitted() {
    let _guard = libsql_test_guard();
    let (engine, _tmp, _path_str) = init_clean_engine().await;
    let mut input = base_input();
    input.frontmatter = None;

    let page = engine
        .put_page("default-frontmatter", None, &input)
        .await
        .expect("put_page");
    assert_eq!(
        page.frontmatter,
        json!({}),
        "omitted frontmatter must round-trip as an empty JSON object"
    );

    let fetched = engine
        .get_page("default-frontmatter", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("page must exist");
    assert_eq!(
        fetched.frontmatter,
        json!({}),
        "stored omitted frontmatter must read back as an empty JSON object"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn frontmatter_column_is_text_not_null_default_empty_object() {
    let _guard = libsql_test_guard();
    let (engine, _tmp, path_str) = init_clean_engine().await;

    let (column_type, not_null, default_value) = page_column_info(&path_str, "frontmatter").await;
    assert_eq!(column_type.to_uppercase(), "TEXT");
    assert_eq!(not_null, 1, "frontmatter must be NOT NULL in SQLite");
    assert_eq!(
        default_value.as_deref(),
        Some("'{}'"),
        "frontmatter default must be the empty JSON object literal"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn corpus_generation_column_is_text_and_decodes_as_string() {
    let _guard = libsql_test_guard();
    let (engine, _tmp, path_str) = init_clean_engine().await;

    let (column_type, _not_null, _default_value) =
        page_column_info(&path_str, "corpus_generation").await;
    assert_eq!(
        column_type.to_uppercase(),
        "TEXT",
        "corpus_generation must remain TEXT, not INTEGER"
    );

    engine
        .put_page("corpus-text", None, &base_input())
        .await
        .expect("put_page");
    let conn = raw_conn(&path_str).await;
    conn.execute(
        "UPDATE pages SET corpus_generation = ?1 WHERE slug = ?2",
        libsql::params!["gen-alpha", "corpus-text"],
    )
    .await
    .expect("set corpus_generation");

    let fetched = engine
        .get_page("corpus-text", &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("page must exist");
    assert_eq!(
        fetched.corpus_generation.as_deref(),
        Some("gen-alpha"),
        "corpus_generation must decode as Option<String>"
    );

    engine.disconnect().await.expect("disconnect");
}
