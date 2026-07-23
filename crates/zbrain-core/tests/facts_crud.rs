//! Phase 7B: Facts CRUD integration tests.
//!
//! Covers `insert_fact`, `list_facts_by_entity`, `get_facts_health`,
//! `expire_fact` across InMemory and Libsql backends.
//!
//! Unit tests for InMemoryEngine live in `src/engine.rs` (end of the impl
//! block). This file adds:
//!   - Cross-backend contract tests (InMemory + Libsql).
//!   - Libsql persistence tests (disconnect/reconnect).
//!   - Filter coverage (kinds, visibility, limit, offset) not covered by
//!     unit tests.
//!   - Postgres contract mirror (reuses the shared `test_*` contract fns
//!     against an ephemeral pg-embed instance).

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::types::{
    FactInsertStatus, FactKind, FactListOpts, FactVisibility, NewFact,
};
use zbrain_core::InMemoryEngine;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn nf(text: &str) -> NewFact {
    NewFact {
        fact: text.to_string(),
        kind: None,
        entity_slug: None,
        visibility: None,
        context: None,
        valid_from: None,
        valid_until: None,
        source: "test".to_string(),
        source_session: None,
        confidence: None,
        notability: None,
        claim_metric: None,
        claim_value: None,
        claim_unit: None,
        claim_period: None,
        event_type: None,
    }
}

fn nf_kind(text: &str, kind: FactKind) -> NewFact {
    NewFact {
        kind: Some(kind),
        ..nf(text)
    }
}

fn nf_conf_kind(text: &str, confidence: f64, kind: FactKind) -> NewFact {
    NewFact {
        kind: Some(kind),
        confidence: Some(confidence),
        ..nf(text)
    }
}

fn nf_kind_visibility(text: &str, kind: FactKind, visibility: FactVisibility) -> NewFact {
    NewFact {
        kind: Some(kind),
        visibility: Some(visibility),
        ..nf(text)
    }
}

fn nf_i(text: &str, kind: FactKind) -> NewFact {
    NewFact {
        kind: Some(kind),
        ..nf(text)
    }
}

// ---------------------------------------------------------------------------
// Shared contract — runs against any &dyn BrainEngine
// ---------------------------------------------------------------------------

macro_rules! assert_inserted {
    ($engine:expr, $sid:expr, $slug:expr, $input:expr) => {
        assert_eq!(
            $engine.insert_fact($sid, $slug, &$input).await.unwrap(),
            FactInsertStatus::Inserted
        );
    };
}

/// Insert facts and run assertions against both backends.
async fn test_insert_roundtrip(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "src-1", "alice", nf("likes coffee"));

    let rows = engine
        .list_facts_by_entity("src-1", "alice", &FactListOpts::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let f = &rows[0];
    assert_eq!(f.fact, "likes coffee");
    assert_eq!(f.kind, FactKind::Fact);
    assert_eq!(f.source_id, "src-1");
    assert_eq!(f.entity_slug.as_deref(), Some("alice"));
    assert_eq!(f.visibility, FactVisibility::Private);
    assert_eq!(f.notability, "medium");
    assert!(f.confidence > 0.99);
    assert!(f.valid_from.is_some()); // defaulted by engine
    assert!(f.expired_at.is_none());
    assert!(f.superseded_by.is_none());
    assert!(f.created_at.is_some());
}

async fn test_duplicate_detection(engine: &dyn BrainEngine) {
    let input = nf("likes pizza");

    let s1 = engine.insert_fact("src-2", "alice", &input).await.unwrap();
    assert_eq!(s1, FactInsertStatus::Inserted);

    let s2 = engine.insert_fact("src-2", "alice", &input).await.unwrap();
    assert_eq!(s2, FactInsertStatus::Duplicate);

    // Only one row persisted
    let rows = engine
        .list_facts_by_entity("src-2", "alice", &FactListOpts::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

async fn test_duplicate_different_entity_inserted(engine: &dyn BrainEngine) {
    let input = nf("likes tea");
    assert_inserted!(engine, "src-3", "alice", input.clone());
    assert_inserted!(engine, "src-3", "bob", input);
}

async fn test_supersede_high_confidence(engine: &dyn BrainEngine) {
    engine
        .insert_fact("src-4", "alice", &nf_conf_kind("old belief", 0.8, FactKind::Belief))
        .await
        .unwrap();

    let s = engine
        .insert_fact(
            "src-4",
            "alice",
            &nf_conf_kind("new belief", 0.95, FactKind::Belief),
        )
        .await
        .unwrap();
    assert_eq!(s, FactInsertStatus::Superseded);

    let rows = engine
        .list_facts_by_entity(
            "src-4",
            "alice",
            &FactListOpts {
                active_only: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    let old = rows.iter().find(|r| r.fact == "old belief").unwrap();
    assert!(old.superseded_by.is_some());

    let active = engine
        .list_facts_by_entity("src-4", "alice", &FactListOpts::default())
        .await
        .unwrap();
    assert_eq!(active.len(), 2); // active_only defaults to false→shows all
}

async fn test_supersede_low_confidence_noop(engine: &dyn BrainEngine) {
    engine
        .insert_fact("src-5", "alice", &nf_conf_kind("low-a", 0.8, FactKind::Belief))
        .await
        .unwrap();

    // confidence 0.89 is below 0.9 threshold → inserted, no supersede
    let s = engine
        .insert_fact(
            "src-5",
            "alice",
            &nf_conf_kind("low-b", 0.89, FactKind::Belief),
        )
        .await
        .unwrap();
    assert_eq!(s, FactInsertStatus::Inserted);

    let rows = engine
        .list_facts_by_entity("src-5", "alice", &FactListOpts::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.superseded_by.is_none()));
}

async fn test_list_active_only(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "src-6", "eve", nf_kind("event-a", FactKind::Event));
    assert_inserted!(engine, "src-6", "eve", nf_kind("belief-a", FactKind::Belief));

    // Expire first
    let rows = engine
        .list_facts_by_entity("src-6", "eve", &FactListOpts::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let id1 = rows[0].id;

    engine.expire_fact("src-6", id1).await.unwrap();

    let active = engine
        .list_facts_by_entity(
            "src-6",
            "eve",
            &FactListOpts {
                active_only: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, rows[1].id);
}

async fn test_list_kinds_filter(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "src-7", "felix", nf_kind("e1", FactKind::Event));
    assert_inserted!(engine, "src-7", "felix", nf_kind("p1", FactKind::Preference));
    assert_inserted!(engine, "src-7", "felix", nf_kind("b1", FactKind::Belief));

    let events_only = engine
        .list_facts_by_entity(
            "src-7",
            "felix",
            &FactListOpts {
                kinds: Some(vec![FactKind::Event]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(events_only.len(), 1);
    assert_eq!(events_only[0].kind, FactKind::Event);

    let multi = engine
        .list_facts_by_entity(
            "src-7",
            "felix",
            &FactListOpts {
                kinds: Some(vec![FactKind::Event, FactKind::Preference]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(multi.len(), 2);
}

async fn test_list_visibility_filter(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "src-8", "gina", nf_kind_visibility("pub-fact", FactKind::Event, FactVisibility::World));
    assert_inserted!(engine, "src-8", "gina", nf_kind_visibility("priv-fact", FactKind::Belief, FactVisibility::Private));

    let world_only = engine
        .list_facts_by_entity(
            "src-8",
            "gina",
            &FactListOpts {
                visibility: Some(vec![FactVisibility::World]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(world_only.len(), 1);
    assert_eq!(world_only[0].fact, "pub-fact");

    let private_only = engine
        .list_facts_by_entity(
            "src-8",
            "gina",
            &FactListOpts {
                visibility: Some(vec![FactVisibility::Private]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(private_only.len(), 1);
    assert_eq!(private_only[0].fact, "priv-fact");
}

async fn test_list_limit_offset(engine: &dyn BrainEngine) {
    let kinds = [
        FactKind::Event,
        FactKind::Preference,
        FactKind::Belief,
        FactKind::Commitment,
        FactKind::Fact,
    ];
    for i in 0..5 {
        assert_inserted!(
            engine,
            "src-9",
            "hank",
            nf_i(&format!("fact-{i}"), kinds[i].clone())
        );
    }

    // Limit
    let limited = engine
        .list_facts_by_entity(
            "src-9",
            "hank",
            &FactListOpts {
                limit: Some(3),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(limited.len(), 3);

    // Offset
    let paged = engine
        .list_facts_by_entity(
            "src-9",
            "hank",
            &FactListOpts {
                limit: Some(2),
                offset: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(paged.len(), 2);
    // Should be different from first 2
    let first_two = &limited[..2];
    assert_ne!(first_two[0].id, paged[0].id);
}

async fn test_list_wrong_source_empty(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "src-a", "zoe", nf("only in A"));

    let rows = engine
        .list_facts_by_entity("src-b", "zoe", &FactListOpts::default())
        .await
        .unwrap();
    assert!(rows.is_empty());
}

async fn test_health_counts(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "h-src", "alice", nf_kind("f1", FactKind::Event));
    assert_inserted!(engine, "h-src", "alice", nf_kind("f2", FactKind::Preference));

    let health = engine.get_facts_health("h-src").await.unwrap();
    assert_eq!(health.source_id, "h-src");
    assert_eq!(health.total_active, 2);
    assert_eq!(health.total_expired, 0);
    assert_eq!(health.total_consolidated, 0);
    assert!(health.top_entities.len() >= 1);
}

async fn test_health_top_entities(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "h2", "alice", nf_kind("a1", FactKind::Event));
    assert_inserted!(engine, "h2", "alice", nf_kind("a2", FactKind::Preference));
    assert_inserted!(engine, "h2", "bob", nf("b1"));

    let health = engine.get_facts_health("h2").await.unwrap();
    // bob has 1, alice has 2 → alice should be first
    let top = &health.top_entities[0];
    assert_eq!(top.entity_slug, "alice");
    assert_eq!(top.count, 2);
}

async fn test_health_empty_source(engine: &dyn BrainEngine) {
    let health = engine.get_facts_health("no-such-source").await.unwrap();
    assert_eq!(health.total_active, 0);
    assert_eq!(health.total_today, 0);
    assert_eq!(health.total_week, 0);
    assert!(health.top_entities.is_empty());
}

async fn test_expire_basic(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "ex-1", "dave", nf("expirable"));

    let rows = engine
        .list_facts_by_entity("ex-1", "dave", &FactListOpts::default())
        .await
        .unwrap();
    let id = rows[0].id;

    let ok = engine.expire_fact("ex-1", id).await.unwrap();
    assert!(ok);

    // Active-only filter should exclude it now
    let active = engine
        .list_facts_by_entity(
            "ex-1",
            "dave",
            &FactListOpts {
                active_only: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(active.is_empty());
}

async fn test_expire_twice_returns_false(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "ex-2", "ed", nf("one-shot"));
    let rows = engine
        .list_facts_by_entity("ex-2", "ed", &FactListOpts::default())
        .await
        .unwrap();
    let id = rows[0].id;

    assert!(engine.expire_fact("ex-2", id).await.unwrap());
    assert!(!engine.expire_fact("ex-2", id).await.unwrap());
}

async fn test_expire_wrong_source_returns_false(engine: &dyn BrainEngine) {
    assert_inserted!(engine, "ex-a", "fred", nf("only-in-A"));
    let rows = engine
        .list_facts_by_entity("ex-a", "fred", &FactListOpts::default())
        .await
        .unwrap();
    let id = rows[0].id;

    assert!(!engine.expire_fact("ex-b", id).await.unwrap());
}

async fn test_expire_nonexistent_returns_false(engine: &dyn BrainEngine) {
    assert!(!engine.expire_fact("ex-z", 99999).await.unwrap());
}

// ---------------------------------------------------------------------------
// InMemoryEngine tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmem_insert_roundtrip() {
    test_insert_roundtrip(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_duplicate_detection() {
    test_duplicate_detection(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_duplicate_different_entity_inserted() {
    test_duplicate_different_entity_inserted(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_supersede_high_confidence() {
    test_supersede_high_confidence(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_supersede_low_confidence_noop() {
    test_supersede_low_confidence_noop(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_list_active_only() {
    test_list_active_only(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_list_kinds_filter() {
    test_list_kinds_filter(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_list_visibility_filter() {
    test_list_visibility_filter(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_list_limit_offset() {
    test_list_limit_offset(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_list_wrong_source_empty() {
    test_list_wrong_source_empty(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_health_counts() {
    test_health_counts(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_health_top_entities() {
    test_health_top_entities(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_health_empty_source() {
    test_health_empty_source(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_expire_basic() {
    test_expire_basic(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_expire_twice_returns_false() {
    test_expire_twice_returns_false(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_expire_wrong_source_returns_false() {
    test_expire_wrong_source_returns_false(&InMemoryEngine::new()).await;
}

#[tokio::test]
async fn inmem_expire_nonexistent_returns_false() {
    test_expire_nonexistent_returns_false(&InMemoryEngine::new()).await;
}

// ---------------------------------------------------------------------------
// Libsql integration tests
// ---------------------------------------------------------------------------

async fn init_clean_libsql() -> (LibsqlEngine, NamedTempFile) {
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

#[tokio::test]
async fn libsql_insert_roundtrip() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_insert_roundtrip(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_duplicate_detection() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_duplicate_detection(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_duplicate_different_entity_inserted() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_duplicate_different_entity_inserted(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_supersede_high_confidence() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_supersede_high_confidence(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_supersede_low_confidence_noop() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_supersede_low_confidence_noop(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_list_active_only() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_list_active_only(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_list_kinds_filter() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_list_kinds_filter(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_list_visibility_filter() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_list_visibility_filter(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_list_limit_offset() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_list_limit_offset(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_health_counts() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_health_counts(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_health_top_entities() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_health_top_entities(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_health_empty_source() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_health_empty_source(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_expire_basic() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_expire_basic(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_expire_twice_returns_false() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_expire_twice_returns_false(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_expire_wrong_source_returns_false() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_expire_wrong_source_returns_false(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_expire_nonexistent_returns_false() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_expire_nonexistent_returns_false(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

// ── Libsql persistence: facts survive disconnect/reconnect ──────────

#[tokio::test]
async fn libsql_facts_survive_reconnect() {
    let (engine, tmp) = init_clean_libsql().await;

    assert_inserted!(engine, "persist", "alice", nf("survives reboot"));

    engine.disconnect().await.expect("disconnect");
    drop(engine);

    // Reconnect to the same temp file
    let engine2 = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(tmp.path().to_string_lossy().into_owned()),
    };
    engine2.connect(&cfg).await.expect("reconnect");
    engine2.init_schema().await.expect("reinit schema");

    let rows = engine2
        .list_facts_by_entity("persist", "alice", &FactListOpts::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fact, "survives reboot");
    assert_eq!(rows[0].kind, FactKind::Fact);

    engine2.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_supersede_survives_reconnect() {
    let (engine, tmp) = init_clean_libsql().await;

    engine
        .insert_fact("persist", "bob", &nf_conf_kind("v1", 0.8, FactKind::Belief))
        .await
        .unwrap();
    engine
        .insert_fact("persist", "bob", &nf_conf_kind("v2", 0.95, FactKind::Belief))
        .await
        .unwrap();

    engine.disconnect().await.expect("disconnect");
    drop(engine);

    let engine2 = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(tmp.path().to_string_lossy().into_owned()),
    };
    engine2.connect(&cfg).await.expect("reconnect");
    engine2.init_schema().await.expect("reinit schema");

    let rows = engine2
        .list_facts_by_entity(
            "persist",
            "bob",
            &FactListOpts {
                active_only: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let old = rows.iter().find(|r| r.fact == "v1").unwrap();
    assert!(old.superseded_by.is_some());

    engine2.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// Postgres integration tests
// ---------------------------------------------------------------------------
//
// These reuse the backend-agnostic `test_*` contract functions against an
// ephemeral pg-embed instance. Unlike SQLite (which does not enforce foreign
// keys by default), Postgres enforces the `facts.source_id REFERENCES
// sources(id)` constraint, so every source id touched by a contract must be
// seeded first via side-channel SQL.

/// Seed one or more source rows so the `facts.source_id` FK is satisfied.
async fn pg_seed_sources(url: &str, ids: &[&str]) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("source seed pool");
    for id in ids {
        sqlx::query("INSERT INTO sources (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
            .bind(id)
            .bind(id)
            .execute(&pool)
            .await
            .expect("seed source");
    }
    pool.close().await;
}

#[tokio::test]
async fn postgres_insert_roundtrip() {
    let fix = support::pg_fixture::PgFixture::start().await;
    pg_seed_sources(&fix.url, &["src-1"]).await;
    test_insert_roundtrip(&fix.engine).await;
}

#[tokio::test]
async fn postgres_supersede_high_confidence() {
    let fix = support::pg_fixture::PgFixture::start().await;
    pg_seed_sources(&fix.url, &["src-4"]).await;
    test_supersede_high_confidence(&fix.engine).await;
}

#[tokio::test]
async fn postgres_list_active_only() {
    let fix = support::pg_fixture::PgFixture::start().await;
    pg_seed_sources(&fix.url, &["src-6"]).await;
    test_list_active_only(&fix.engine).await;
}

#[tokio::test]
async fn postgres_health_counts() {
    let fix = support::pg_fixture::PgFixture::start().await;
    pg_seed_sources(&fix.url, &["h-src"]).await;
    test_health_counts(&fix.engine).await;
}

#[tokio::test]
async fn postgres_expire_basic() {
    let fix = support::pg_fixture::PgFixture::start().await;
    pg_seed_sources(&fix.url, &["ex-1"]).await;
    test_expire_basic(&fix.engine).await;
}

// ---------------------------------------------------------------------------
// find_trajectory integration (cutover batch 4)
// ---------------------------------------------------------------------------

async fn test_find_trajectory(engine: &dyn BrainEngine) {
    use zbrain_core::types::{TrajectoryKind, TrajectoryOpts};

    let claim = |metric: &str, value: f64, vf: &str, slug: &str| NewFact {
        fact: format!("{metric}={value} @ {vf}"),
        kind: Some(FactKind::Fact),
        entity_slug: Some(slug.to_string()),
        claim_metric: Some(metric.to_string()),
        claim_value: Some(value),
        claim_unit: Some("usd".to_string()),
        claim_period: Some("monthly".to_string()),
        valid_from: Some(vf.to_string()),
        confidence: Some(0.5),
        source: "test".to_string(),
        ..nf("")
    };
    let event = |etype: &str, vf: &str, slug: &str| NewFact {
        fact: format!("event {etype} @ {vf}"),
        kind: Some(FactKind::Event),
        entity_slug: Some(slug.to_string()),
        event_type: Some(etype.to_string()),
        valid_from: Some(vf.to_string()),
        confidence: Some(0.5),
        source: "test".to_string(),
        ..nf("")
    };

    assert_inserted!(engine, "src-ft", "acme", claim("mrr", 100.0, "2024-01-01", "acme"));
    assert_inserted!(engine, "src-ft", "acme", claim("mrr", 80.0, "2024-02-01", "acme"));
    assert_inserted!(engine, "src-ft", "acme", event("funding", "2024-03-01", "acme"));
    assert_inserted!(engine, "src-ft", "other", claim("mrr", 50.0, "2024-01-15", "other"));

    // All rows for acme, ordered by valid_from ASC.
    let all = engine
        .find_trajectory(&TrajectoryOpts {
            entity_slug: "acme".to_string(),
            source_id: Some("src-ft".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 3, "acme has 3 active rows (2 metric + 1 event)");
    assert_eq!(all[0].metric.as_deref(), Some("mrr"));
    assert_eq!(all[0].valid_from.as_deref(), Some("2024-01-01"));
    assert_eq!(all[1].metric.as_deref(), Some("mrr"));
    assert_eq!(all[2].event_type.as_deref(), Some("funding"));
    assert_eq!(all[2].valid_from.as_deref(), Some("2024-03-01"));

    // kind=Metric restricts to claim_metric IS NOT NULL.
    let metrics = engine
        .find_trajectory(&TrajectoryOpts {
            entity_slug: "acme".to_string(),
            source_id: Some("src-ft".to_string()),
            kind: TrajectoryKind::Metric,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(metrics.len(), 2);

    // kind=Event restricts to event_type IS NOT NULL.
    let events = engine
        .find_trajectory(&TrajectoryOpts {
            entity_slug: "acme".to_string(),
            source_id: Some("src-ft".to_string()),
            kind: TrajectoryKind::Event,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type.as_deref(), Some("funding"));

    // metric filter + kind=Metric.
    let mrr = engine
        .find_trajectory(&TrajectoryOpts {
            entity_slug: "acme".to_string(),
            source_id: Some("src-ft".to_string()),
            kind: TrajectoryKind::Metric,
            metric: Some("mrr".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(mrr.len(), 2);
    assert!(mrr.iter().all(|p| p.metric.as_deref() == Some("mrr")));

    // 'other' entity is isolated from 'acme'.
    let other = engine
        .find_trajectory(&TrajectoryOpts {
            entity_slug: "other".to_string(),
            source_id: Some("src-ft".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(other.len(), 1);

    // Stats: mrr 100 -> 80 is a -20% regression.
    let stats = zbrain_core::trajectory_stats::compute_trajectory_stats(&all, 0.10);
    assert_eq!(stats.regressions.len(), 1);
    assert_eq!(stats.regressions[0].metric, "mrr");
    assert!((stats.regressions[0].delta_pct + 0.20).abs() < 1e-9);
}

#[tokio::test]
async fn libsql_find_trajectory() {
    let (engine, _tmp) = init_clean_libsql().await;
    test_find_trajectory(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
