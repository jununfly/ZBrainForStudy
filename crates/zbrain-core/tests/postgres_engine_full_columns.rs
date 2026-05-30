//! Slice #110-a (DDL only) - tests for PG `pages` full-column migration
//! + `bump_page_generation` trigger.
//!
//! Three RED tests:
//!
//! 1. `roundtrip_all_full_columns` - `put_page` + `get_page` should preserve
//!    every non-identity field. Expected to FAIL until #110-b lands the
//!    projection / decoder expansion. Today only the 10 init + `deleted_at`
//!    columns survive the roundtrip; the 19 new columns vanish.
//!
//! 2. `generation_bumps_on_watched_column_change` - SQL-side observation:
//!    after a `put_page` that touches `compiled_truth`, `generation` should
//!    increment by exactly 1. Verifies the trigger fires on UPDATE for an
//!    allow-listed column. Expected GREEN once 0003 is applied (the trigger
//!    runs at the DB layer regardless of decoder state).
//!
//! 3. `generation_does_not_bump_on_unwatched_column_change` - SQL-side
//!    observation: after a `put_page` whose payload only changes a non-watched
//!    column (e.g. `last_retrieved_at`), `generation` should stay constant.
//!    Verifies the IS DISTINCT FROM allow-list correctly excludes salience-
//!    style columns. Expected GREEN once 0003 is applied AND #110-b lands
//!    projection for `last_retrieved_at` via `put_page`; for now this asserts
//!    against the SQL-level state after a manual UPDATE that touches only
//!    the unwatched column.
//!
//! Gated on `ZBRAIN_TEST_PG_URL` (same pattern as the lifecycle suite).
//! Local runs without the env var skip silently; CI exercises the DB path.

use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, PageInput};
use zbrain_core::postgres::PostgresEngine;

fn pg_url() -> Option<String> {
    std::env::var("ZBRAIN_TEST_PG_URL").ok()
}

async fn init_clean_engine() -> Option<PostgresEngine> {
    let url = pg_url()?;
    let engine = PostgresEngine::new();
    let cfg = EngineConfig {
        database_url: Some(url.clone()),
        database_path: None,
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("verification pool");
    sqlx::query("TRUNCATE TABLE pages RESTART IDENTITY CASCADE")
        .execute(&pool)
        .await
        .expect("truncate pages");
    pool.close().await;

    Some(engine)
}

/// Side-channel pool for SQL-level inspection / manipulation that bypasses
/// the engine projection (we want to observe trigger behaviour even when
/// the decoder cannot read the new columns yet).
async fn side_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&pg_url().expect("ZBRAIN_TEST_PG_URL"))
        .await
        .expect("side pool")
}

/// Unique slug per test invocation. Uses monotonic nanoseconds plus the
/// test thread id so parallel runs (`cargo test`) and repeated runs (without
/// TRUNCATE between tests) cannot collide. Avoids pulling `uuid` into the
/// crate just for tests.
fn unique_slug() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // `u128 → u64` is intentional: nanos since epoch fits comfortably in
    // u64 (overflows year 2554). Clamp on the slim chance of overflow.
    let nanos: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u64, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("p-{nanos:x}-{n}")
}

/// RED-1: full-column roundtrip via `put_page` + `get_page`.
///
/// Constructs a `PageInput` populating every write-side optional column,
/// then asserts `get_page` returns matching values. Will FAIL until #110-b
/// lands projection/decoder for the 19 new columns (today they roundtrip
/// as None / default).
#[tokio::test]
async fn roundtrip_all_full_columns() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return;
    };

    let slug = unique_slug();
    let input = PageInput {
        page_type: "note".to_string(),
        title: "Full column page".to_string(),
        compiled_truth: "body".to_string(),
        timeline: Some("[]".to_string()),
        frontmatter: Some(serde_json::json!({ "k": "v" })),
        content_hash: Some("sha256:abc".to_string()),
        page_kind: None,
        effective_date: Some("2026-05-30".to_string()),
        effective_date_source: None,
        import_filename: Some("note.md".to_string()),
        chunker_version: Some(7),
        source_path: Some("/tmp/note.md".to_string()),
        source_kind: Some("file".to_string()),
        source_uri: Some("file:///tmp/note.md".to_string()),
        ingested_via: Some("manual".to_string()),
        ingested_at: Some("2026-05-30T00:00:00+00:00".to_string()),
        last_retrieved_at: Some("2026-05-30T01:00:00+00:00".to_string()),
        embedding: Some(vec![1u8, 2, 3, 4]),
    };

    engine
        .put_page(&slug, None, &input)
        .await
        .expect("put_page");

    let page = engine
        .get_page(&slug, &GetPageOpts::default())
        .await
        .expect("get_page")
        .expect("page exists");

    // Roundtrip checks: every write-side optional should survive.
    assert_eq!(page.timeline, "[]", "timeline");
    assert_eq!(
        page.frontmatter,
        serde_json::json!({ "k": "v" }),
        "frontmatter"
    );
    assert_eq!(
        page.content_hash.as_deref(),
        Some("sha256:abc"),
        "content_hash"
    );
    assert_eq!(
        page.effective_date.as_deref(),
        Some("2026-05-30"),
        "effective_date"
    );
    assert_eq!(
        page.import_filename.as_deref(),
        Some("note.md"),
        "import_filename"
    );
    assert_eq!(page.chunker_version, 7, "chunker_version");
    assert_eq!(
        page.source_path.as_deref(),
        Some("/tmp/note.md"),
        "source_path"
    );
    assert_eq!(page.source_kind.as_deref(), Some("file"), "source_kind");
    assert_eq!(
        page.source_uri.as_deref(),
        Some("file:///tmp/note.md"),
        "source_uri"
    );
    assert_eq!(page.ingested_via.as_deref(), Some("manual"), "ingested_via");
    assert_eq!(
        page.ingested_at.as_deref(),
        Some("2026-05-30T00:00:00+00:00"),
        "ingested_at"
    );
    assert_eq!(
        page.last_retrieved_at.as_deref(),
        Some("2026-05-30T01:00:00+00:00"),
        "last_retrieved_at"
    );
    assert_eq!(page.embedding.as_deref(), Some(&[1u8, 2, 3, 4][..]), "embedding");
}

/// RED-2: trigger should bump `generation` when an allow-listed column
/// (`compiled_truth`) changes.
///
/// Bypasses the engine for inspection so we see the trigger result even if
/// the decoder still drops `generation`. Should PASS once 0003 lands.
#[tokio::test]
async fn generation_bumps_on_watched_column_change() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return;
    };

    let slug = unique_slug();
    let input = PageInput {
        page_type: "note".to_string(),
        title: "Bump test".to_string(),
        compiled_truth: "v1".to_string(),
        ..Default::default()
    };
    engine
        .put_page(&slug, None, &input)
        .await
        .expect("put_page v1");

    let pool = side_pool().await;
    let gen_v1: i64 = sqlx::query_scalar("SELECT generation FROM pages WHERE slug = $1")
        .bind(&slug)
        .fetch_one(&pool)
        .await
        .expect("read gen v1");

    // Touch an allow-listed column directly via SQL — observable regardless
    // of whether put_page yet covers compiled_truth in the projection path.
    sqlx::query("UPDATE pages SET compiled_truth = 'v2' WHERE slug = $1")
        .bind(&slug)
        .execute(&pool)
        .await
        .expect("sql update compiled_truth");

    let gen_v2: i64 = sqlx::query_scalar("SELECT generation FROM pages WHERE slug = $1")
        .bind(&slug)
        .fetch_one(&pool)
        .await
        .expect("read gen v2");

    assert_eq!(
        gen_v2,
        gen_v1 + 1,
        "generation must bump by 1 on watched column change"
    );

    pool.close().await;
}

/// RED-3: trigger must NOT bump `generation` when only an unwatched
/// column changes (e.g. `salience_score`, which is intentionally excluded
/// from the 10-column allow-list to keep the cache warm during background
/// salience recompute).
#[tokio::test]
async fn generation_does_not_bump_on_unwatched_column_change() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return;
    };

    let slug = unique_slug();
    let input = PageInput {
        page_type: "note".to_string(),
        title: "No-bump test".to_string(),
        compiled_truth: "stable".to_string(),
        ..Default::default()
    };
    engine
        .put_page(&slug, None, &input)
        .await
        .expect("put_page");

    let pool = side_pool().await;
    let gen_before: i64 = sqlx::query_scalar("SELECT generation FROM pages WHERE slug = $1")
        .bind(&slug)
        .fetch_one(&pool)
        .await
        .expect("read gen before");

    // `salience_score` is explicitly excluded from the allow-list.
    sqlx::query("UPDATE pages SET salience_score = 0.42 WHERE slug = $1")
        .bind(&slug)
        .execute(&pool)
        .await
        .expect("sql update salience_score");

    let gen_after: i64 = sqlx::query_scalar("SELECT generation FROM pages WHERE slug = $1")
        .bind(&slug)
        .fetch_one(&pool)
        .await
        .expect("read gen after");

    assert_eq!(
        gen_after, gen_before,
        "generation must NOT bump when an unwatched column changes"
    );

    pool.close().await;
}
