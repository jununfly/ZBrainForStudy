//! Slice #110-c - PG ↔ TS source-of-truth contract alignment.
//!
//! Reworks #110-b's roundtrip test to encode three contracts that TS
//! `postgres-engine.ts` + `pglite-engine.ts` + `schema.sql` actually enforce,
//! which #110-b's "intentional divergence" doc-comment got wrong:
//!
//!   - `embedding` / `last_retrieved_at`: NOT persisted via `put_page`.
//!     PUT accepts them in `PageInput` (write-through schema convenience) but
//!     GET must return `None`. Source of truth: TS engines' putPage SQL has
//!     19 columns, no `embedding`, no `last_retrieved_at`. Those columns are
//!     written by separate code paths (embedder / retrieval-tracker).
//!
//!   - `ingested_at`: server-stamped, not client-provided. When any of
//!     `source_kind` / `source_uri` / `ingested_via` is present and the
//!     caller does not supply `ingested_at`, the engine stamps `NOW()`.
//!     When all three are None, `ingested_at` stays None.
//!     Source of truth: TS pglite-engine.ts:849 — `(sourceKind || sourceUri
//!     || ingestedVia) ? new Date().toISOString() : null;`
//!
//!   - `corpus_generation`: TEXT, not INTEGER. Source of truth: TS
//!     schema.sql:131 + both engines' `ALTER TABLE pages ADD COLUMN IF NOT
//!     EXISTS corpus_generation TEXT;`. #110-a 0003 migration wrote INTEGER
//!     by mistake; #110-c 0004 fixes the column type and 0004 also enforces
//!     `frontmatter NOT NULL DEFAULT '{}'::jsonb` (TS schema.sql:93).
//!
//! Six tests total. Three roundtrip-shape tests (one per contract above)
//! plus the two trigger tests carried forward unchanged from #110-b, plus a
//! new `frontmatter_defaults_to_empty_object_when_omitted` and a new
//! `corpus_generation_column_is_text` (`pg_typeof` probe).
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
    // Reseed the `default` sources row. The 0001 migration seeds it on first
    // apply, but sqlx records that migration as applied; later `init_schema`
    // calls do NOT re-run it. If another test (or the shared test DB) ever
    // TRUNCATEs the `sources` table, the row vanishes permanently. Since
    // `put_page` defaults `source_id` to `"default"` (postgres.rs put_page),
    // its absence triggers `pages_source_id_fkey` on every shape test below.
    // ON CONFLICT keeps the operation idempotent.
    sqlx::query(
        "INSERT INTO sources (id, name) VALUES ('default', 'default') ON CONFLICT (id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .expect("seed default source");
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

/// RED-1: 17-column roundtrip via `put_page` + `get_page`.
///
/// Aligned to TS source of truth (#110-c contract refresh):
///   - 16 plain optionals roundtrip normally.
///   - `ingested_at` client-provided value is honoured (server-stamp path
///     covered by the next two tests).
///   - `embedding` + `last_retrieved_at` are NOT persisted by `put_page`;
///     PUT accepts them as a write-through schema convenience but GET
///     must return None. (TS engines write these via separate code paths.)
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
        "ingested_at (client-provided)"
    );
    // Contract (#110-c): embedding + last_retrieved_at are NOT persisted by
    // put_page. TS engines write these via separate code paths (embedder /
    // retrieval-tracker). PUT accepts them as a write-through schema
    // convenience but GET must return None.
    assert_eq!(
        page.last_retrieved_at, None,
        "last_retrieved_at must not be persisted by put_page"
    );
    assert_eq!(
        page.embedding, None,
        "embedding must not be persisted by put_page"
    );
}

/// RED-2 (new in #110-c): `ingested_at` is server-stamped when any ingestion
/// metadata is present and the caller does not supply a value.
///
/// TS source of truth (pglite-engine.ts:849):
///   `(sourceKind || sourceUri || ingestedVia) ? new Date().toISOString() : null`
#[tokio::test]
async fn ingested_at_server_stamped_when_any_ingestion_metadata_present() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return;
    };

    let slug = unique_slug();
    let input = PageInput {
        page_type: "note".to_string(),
        title: "Server-stamp test".to_string(),
        compiled_truth: "body".to_string(),
        source_kind: Some("file".to_string()),
        // ingested_at intentionally omitted; source_kind alone must trigger
        // server-stamp.
        ..Default::default()
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

    assert!(
        page.ingested_at.is_some(),
        "ingested_at must be server-stamped when source_kind is present"
    );
}

/// RED-3 (new in #110-c): `ingested_at` stays None when no ingestion
/// metadata is present and the caller does not supply a value.
#[tokio::test]
async fn ingested_at_remains_none_without_ingestion_metadata() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return;
    };

    let slug = unique_slug();
    let input = PageInput {
        page_type: "note".to_string(),
        title: "No-stamp test".to_string(),
        compiled_truth: "body".to_string(),
        ..Default::default()
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

    assert_eq!(
        page.ingested_at, None,
        "ingested_at must remain None when no ingestion metadata is provided"
    );
}

/// RED-4 (new in #110-c): `frontmatter` defaults to `{}` when omitted, and
/// the column itself is NOT NULL.
///
/// TS source of truth: schema.sql:93 — `frontmatter JSONB NOT NULL DEFAULT '{}'`.
#[tokio::test]
async fn frontmatter_defaults_to_empty_object_when_omitted() {
    let Some(engine) = init_clean_engine().await else {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return;
    };

    let slug = unique_slug();
    let input = PageInput {
        page_type: "note".to_string(),
        title: "Frontmatter default test".to_string(),
        compiled_truth: "body".to_string(),
        // frontmatter intentionally omitted
        ..Default::default()
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

    assert_eq!(
        page.frontmatter,
        serde_json::json!({}),
        "frontmatter must default to empty object when omitted"
    );

    let pool = side_pool().await;
    let is_nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns \
         WHERE table_name = 'pages' AND column_name = 'frontmatter'",
    )
    .fetch_one(&pool)
    .await
    .expect("read is_nullable");
    assert_eq!(
        is_nullable, "NO",
        "frontmatter column must be NOT NULL (TS schema.sql:93)"
    );
    pool.close().await;
}

/// RED-5 (new in #110-c): `corpus_generation` is a TEXT column.
///
/// TS source of truth: schema.sql:131 + both engines'
/// `ALTER TABLE pages ADD COLUMN IF NOT EXISTS corpus_generation TEXT;`.
/// #110-a 0003 wrote INTEGER by mistake; 0004 fixes it.
#[tokio::test]
async fn corpus_generation_column_is_text() {
    let Some(_engine) = init_clean_engine().await else {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return;
    };

    let pool = side_pool().await;
    let data_type: String = sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_name = 'pages' AND column_name = 'corpus_generation'",
    )
    .fetch_one(&pool)
    .await
    .expect("read data_type");
    assert_eq!(
        data_type, "text",
        "corpus_generation must be TEXT (TS schema.sql:131)"
    );
    pool.close().await;
}

/// Trigger test (carried from #110-b): `generation` bumps when an
/// allow-listed column (`compiled_truth`) changes via direct SQL UPDATE.
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

/// Trigger test (carried from #110-b): `generation` must NOT bump when only
/// an unwatched column changes (e.g. `salience_score`, intentionally excluded
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
