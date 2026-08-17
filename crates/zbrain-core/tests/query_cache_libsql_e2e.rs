//! 1-5-17 — G69-B integration: semantic query cache end-to-end via the
//! libsql persistence layer.
//!
//! Unit coverage for the cache orchestrator lives in `search/cache.rs`; this
//! binary proves the libsql wiring of the five `BrainEngine` cache methods
//! (`cache_lookup` / `cache_store` / `cache_clear` / `cache_prune` /
//! `cache_stats`) and the D11 two-layer invalidation gate against a real
//! libsql file. This is the second leg of the v6 dual-verify rule for G69-B.

use std::collections::HashMap;
use std::sync::OnceLock;

use tempfile::NamedTempFile;
use zbrain_core::engine::{
    BrainEngine, CacheLookupOpts, CacheStoreOpts, CreateSourceInput, EngineConfig, GetPageOpts,
    PageInput,
};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::search::cache::stable_hash;

/// Serialize all libsql FFI access in this binary. The libsql native library
/// is not safe to drive from multiple OS threads concurrently on Windows
/// (parallel `cargo test` threads crash with 0xc0000005). Each test grabs
/// this guard for its whole body so the suite stays green under default
/// parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile) {
    let _g = libsql_test_guard();
    let path = NamedTempFile::new().expect("alloc temp db file");
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path)
}

async fn seed_source(engine: &LibsqlEngine, id: &str) {
    engine
        .create_source(&CreateSourceInput {
            id: id.to_string(),
            name: id.to_string(),
            config: None,
        })
        .await
        .expect("create source");
}

fn f32_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn snapshot_for(source_id: &str, slug: &str, generation: i64) -> HashMap<i64, i64> {
    let mut m = HashMap::new();
    m.insert(stable_hash(&format!("{source_id}::{slug}")), generation);
    m
}

async fn store_row(
    engine: &LibsqlEngine,
    q: &[f32],
    source: &str,
    text: &str,
    created: i64,
    ttl: i64,
) {
    let opts = CacheStoreOpts {
        source_id: source.to_string(),
        knobs_hash: "".to_string(),
        ttl_seconds: ttl,
        page_generations: HashMap::new(),
        max_generation_at_store: 0,
        now_epoch_secs: created,
    };
    engine
        .cache_store(text, q, r#"{"x":1}"#, "null", &opts)
        .await
        .expect("cache_store");
}

#[tokio::test]
async fn store_then_lookup_hits_with_matching_embedding() {
    let (engine, _path) = init_clean_engine().await;
    seed_source(&engine, "src-a").await;
    engine
        .put_page(
            "p1",
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "alpha".into(),
                compiled_truth: "alpha content".into(),
                embedding: Some(f32_to_le_bytes(&[1.0, 0.0, 0.0])),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("put p1");

    let q = vec![1.0_f32, 0.0, 0.0];
    let results_json = r#"{"cached":true}"#;
    let meta_json = "null";

    // Empty snapshot → D11 gate accepts (legacy carve-out), so the lookup
    // should hit purely on the embedding similarity.
    engine
        .cache_store(
            "what is alpha",
            &q,
            results_json,
            meta_json,
            &CacheStoreOpts {
                source_id: "src-a".to_string(),
                knobs_hash: "".to_string(),
                ttl_seconds: 3600,
                page_generations: HashMap::new(),
                max_generation_at_store: 0,
                now_epoch_secs: 1000,
            },
        )
        .await
        .expect("cache_store");

    let hit = engine
        .cache_lookup(
            "what is alpha",
            &q,
            &CacheLookupOpts {
                source_id: "src-a".to_string(),
                knobs_hash: "".to_string(),
                similarity_threshold: 0.92,
                now_epoch_secs: 1000,
            },
        )
        .await
        .expect("cache_lookup");

    let hit = hit.expect("expected a cache hit");
    assert_eq!(hit.results_json, results_json, "results_json must round-trip");
    assert_eq!(hit.meta_json, meta_json, "meta_json must round-trip");
    assert!(
        (hit.similarity - 1.0).abs() < 1e-6,
        "identical embedding → cosine similarity 1.0, got {}",
        hit.similarity
    );
    assert_eq!(hit.age_seconds, 0, "age = now - created_at = 0");
}

#[tokio::test]
async fn d11_gate_invalidates_when_page_generation_bumps() {
    let (engine, _path) = init_clean_engine().await;
    seed_source(&engine, "src-a").await;
    engine
        .put_page(
            "p1",
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "alpha".into(),
                compiled_truth: "v1".into(),
                embedding: Some(f32_to_le_bytes(&[1.0, 0.0, 0.0])),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("put p1");

    // Read the live generation so the snapshot is correct regardless of the
    // initial value the trigger assigns.
    let gen0 = engine
        .get_page("p1", &GetPageOpts {
            source_id: Some("src-a".to_string()),
            ..Default::default()
        })
        .await
        .expect("get_page")
        .expect("p1 present")
        .generation;

    let q = vec![1.0_f32, 0.0, 0.0];
    engine
        .cache_store(
            "what is alpha",
            &q,
            r#"{"cached":true}"#,
            "null",
            &CacheStoreOpts {
                source_id: "src-a".to_string(),
                knobs_hash: "".to_string(),
                ttl_seconds: 3600,
                page_generations: snapshot_for("src-a", "p1", gen0),
                max_generation_at_store: gen0,
                now_epoch_secs: 1000,
            },
        )
        .await
        .expect("cache_store");

    let opts = CacheLookupOpts {
        source_id: "src-a".to_string(),
        knobs_hash: "".to_string(),
        similarity_threshold: 0.92,
        now_epoch_secs: 1000,
    };
    let before = engine
        .cache_lookup("what is alpha", &q, &opts)
        .await
        .expect("cache_lookup")
        .expect("expected a hit before the bump");
    assert_eq!(before.results_json, r#"{"cached":true}"#);

    // Bump p1 by changing a watched column → generation trigger fires.
    engine
        .put_page(
            "p1",
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "alpha".into(),
                compiled_truth: "v2".into(), // distinct from "v1" → generation bumps
                embedding: Some(f32_to_le_bytes(&[1.0, 0.0, 0.0])),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("re-put p1");

    let gen1 = engine
        .get_page("p1", &GetPageOpts {
            source_id: Some("src-a".to_string()),
            ..Default::default()
        })
        .await
        .expect("get_page")
        .expect("p1 present")
        .generation;
    assert!(gen1 > gen0, "trigger should have bumped generation ({gen0} -> {gen1})");

    // Same query + same embedding → still similar, but D11 gate must reject.
    let after = engine
        .cache_lookup("what is alpha", &q, &opts)
        .await
        .expect("cache_lookup");
    assert!(
        after.is_none(),
        "D11 gate must invalidate the cached row after the page generation bumped"
    );
}

#[tokio::test]
async fn clear_prune_and_stats_count_rows() {
    let (engine, _path) = init_clean_engine().await;
    seed_source(&engine, "src-a").await;
    seed_source(&engine, "src-b").await;

    // Two rows for src-a (one fresh, one already expired) and one for src-b.
    let q = vec![1.0_f32, 0.0, 0.0];
    store_row(&engine, &q, "src-a", "a-fresh", 1000, 3600).await;
    store_row(&engine, &q, "src-a", "a-expired", 100, 3600).await; // expires at 3700
    store_row(&engine, &q, "src-b", "b-fresh", 1000, 3600).await;

    // Stats reflect 3 total rows.
    let stats = engine.cache_stats().await.expect("cache_stats");
    assert_eq!(stats.total_rows, 3, "expected 3 cached rows total");

    // a-expired was stored at epoch 100 (expires 100+3600=3700); the two
    // fresh rows were stored at 1000 (expire 4600). Pruning at now=4000
    // removes only the row already past its TTL (3700 <= 4000), leaving the
    // two fresh rows (4600 > 4000) intact.
    let pruned = engine.cache_prune(4000).await.expect("cache_prune");
    assert_eq!(pruned, 1, "exactly the one pre-TTL row should be pruned");
    let stats2 = engine.cache_stats().await.expect("cache_stats");
    assert_eq!(stats2.total_rows, 2, "two rows remain after prune");

    // Scoped clear removes only src-a's rows.
    let cleared = engine
        .cache_clear(Some("src-a"))
        .await
        .expect("cache_clear scoped");
    assert_eq!(cleared, 1, "one src-a row remains to clear");
    let stats3 = engine.cache_stats().await.expect("cache_stats");
    assert_eq!(stats3.total_rows, 1, "only src-b row left");

    // Unscoped clear removes everything.
    let cleared_all = engine.cache_clear(None).await.expect("cache_clear all");
    assert_eq!(cleared_all, 1);
    let stats4 = engine.cache_stats().await.expect("cache_stats");
    assert_eq!(stats4.total_rows, 0, "all rows cleared");
}

#[tokio::test]
async fn lookup_misses_when_embedding_below_threshold() {
    let (engine, _path) = init_clean_engine().await;
    seed_source(&engine, "src-a").await;
    engine
        .put_page(
            "p1",
            Some("src-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "alpha".into(),
                compiled_truth: "alpha content".into(),
                embedding: Some(f32_to_le_bytes(&[1.0, 0.0, 0.0])),
                source_kind: Some("src-a".into()),
                ..Default::default()
            },
        )
        .await
        .expect("put p1");

    let stored = vec![1.0_f32, 0.0, 0.0];
    engine
        .cache_store(
            "q",
            &stored,
            r#"{"cached":true}"#,
            "null",
            &CacheStoreOpts {
                source_id: "src-a".to_string(),
                knobs_hash: "".to_string(),
                ttl_seconds: 3600,
                page_generations: HashMap::new(),
                max_generation_at_store: 0,
                now_epoch_secs: 1000,
            },
        )
        .await
        .expect("cache_store");

    // A query embedding near-orthogonal to the stored one → cosine below
    // the 0.92 threshold → miss.
    let different = vec![0.0_f32, 1.0, 0.0];
    let hit = engine
        .cache_lookup(
            "q",
            &different,
            &CacheLookupOpts {
                source_id: "src-a".to_string(),
                knobs_hash: "".to_string(),
                similarity_threshold: 0.92,
                now_epoch_secs: 1000,
            },
        )
        .await
        .expect("cache_lookup");
    assert!(hit.is_none(), "dissimilar embedding must miss the cache");
}
