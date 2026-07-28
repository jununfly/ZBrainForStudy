//! 1-3-3-2 — `undo_wave` reversal against a real libsql temp database.
//!
//! Canonical TS: `src/core/calibration/undo-wave.ts` (four-step reversal).
//! These tests exercise the *behavior* through the production path:
//! `undo_wave(&dyn BrainEngine)` → `BrainEngine` wave bridges →
//! `CalibrationWaveQueries for LibsqlEngine` real SQL, on a schema created
//! by `init_schema()` (migration 0023 registers the calibration tables).
//!
//! Each test allocates its own `NamedTempFile`, so the suite runs
//! unconditionally in CI with no daemon. Seeding writes rows through a raw
//! `libsql` connection to the same file (mirrors `code_symbol_query.rs`).

use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::calibration::undo_wave;
use zbrain_core::calibration_queries::UndoWaveOpts;
use zbrain_core::engine::{BrainEngine, EngineConfig};
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


/// Fresh `LibsqlEngine` backed by a temp file, schema fully migrated.
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

/// Raw connection to the same temp file for seeding / assertions.
async fn raw_conn(path: &std::path::Path) -> libsql::Connection {
    Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

async fn seed_source(conn: &libsql::Connection, id: &str) {
    conn.execute(
        "INSERT INTO sources (id, name) VALUES (?1, ?1)",
        libsql::params![id],
    )
    .await
    .unwrap();
}

/// Minimal `calibration_profiles` row for a wave (NOT NULL columns filled).
async fn seed_profile(conn: &libsql::Connection, source_id: &str, wave: &str) {
    conn.execute(
        "INSERT INTO calibration_profiles \
         (source_id, holder, wave_version, total_resolved, domain_scorecards, \
          pattern_statements, voice_gate_passed, voice_gate_attempts, \
          active_bias_tags, model_id) \
         VALUES (?1, 'garry', ?2, 10, '{}', '[]', 1, 1, '[]', 'test-model')",
        libsql::params![source_id, wave],
    )
    .await
    .unwrap();
}

async fn count_rows(conn: &libsql::Connection, sql: &str, wave: &str) -> i64 {
    let mut rows = conn.query(sql, libsql::params![wave]).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get::<i64>(0).unwrap()
}

/// Minimal `take_nudge_log` row for a wave. Uses `take_id` (the XOR
/// constraint requires exactly one of take_id / proposal_id).
async fn seed_nudge(conn: &libsql::Connection, source_id: &str, take_id: i64, wave: &str) {
    conn.execute(
        "INSERT INTO take_nudge_log (source_id, take_id, nudge_pattern, wave_version) \
         VALUES (?1, ?2, 'overconfident', ?3)",
        libsql::params![source_id, take_id, wave],
    )
    .await
    .unwrap();
}

/// Minimal `take_grade_cache` row (composite PK on take/prompt/judge/sig).
async fn seed_grade_cache(
    conn: &libsql::Connection,
    take_id: i64,
    wave: &str,
    applied: bool,
) {
    conn.execute(
        "INSERT INTO take_grade_cache \
         (take_id, prompt_version, judge_model_id, evidence_signature, \
          wave_version, verdict, confidence, applied) \
         VALUES (?1, 'p1', 'judge-1', 'sig-' || ?1, ?2, 'correct', 0.9, ?3)",
        libsql::params![take_id, wave, applied],
    )
    .await
    .unwrap();
}

/// Page row to satisfy `takes.page_id` FK; returns the page id.
async fn seed_page(engine: &LibsqlEngine, slug: &str) -> i64 {
    use zbrain_core::engine::PageInput;
    let page = engine
        .put_page(
            slug,
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: slug.to_string(),
                compiled_truth: String::new(),
                frontmatter: Some(serde_json::json!({})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    page.id as i64
}

/// Resolved take with an explicit id (so grade_cache linking is exact).
async fn seed_resolved_take(
    conn: &libsql::Connection,
    id: i64,
    page_id: i64,
    resolved_by: &str,
) {
    conn.execute(
        "INSERT INTO takes \
         (id, page_id, claim, resolved_at, resolved_quality, resolved_outcome, resolved_by) \
         VALUES (?1, ?2, 'claim-' || ?1, '2026-01-01T00:00:00Z', 'correct', 1, ?3)",
        libsql::params![id, page_id, resolved_by],
    )
    .await
    .unwrap();
}

fn opts(wave: &str) -> UndoWaveOpts {
    UndoWaveOpts {
        wave_version: wave.to_string(),
        dry_run: false,
        scrub_gstack: false,
        resolved_by_label: None,
    }
}

// ── Step 2: calibration_profiles deletion ────────────────────────────────

#[tokio::test]
async fn undo_wave_deletes_profiles_for_that_wave_only() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_source(&conn, "wiki").await;
    seed_profile(&conn, "wiki", "v1.0.0").await;
    seed_profile(&conn, "wiki", "v2.0.0").await;

    let result = undo_wave(&engine, &opts("v1.0.0")).await.unwrap();

    assert_eq!(result.profiles_deleted, 1, "exactly the v1 profile");
    let v1 = count_rows(
        &conn,
        "SELECT COUNT(*) FROM calibration_profiles WHERE wave_version = ?1",
        "v1.0.0",
    )
    .await;
    let v2 = count_rows(
        &conn,
        "SELECT COUNT(*) FROM calibration_profiles WHERE wave_version = ?1",
        "v2.0.0",
    )
    .await;
    assert_eq!(v1, 0, "v1 profile row physically gone");
    assert_eq!(v2, 1, "other wave untouched");
}

// ── Step 3: take_nudge_log purge ─────────────────────────────────────────

#[tokio::test]
async fn undo_wave_purges_nudge_log_for_that_wave_only() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_source(&conn, "wiki").await;
    seed_nudge(&conn, "wiki", 1, "v1.0.0").await;
    seed_nudge(&conn, "wiki", 2, "v1.0.0").await;
    seed_nudge(&conn, "wiki", 3, "v2.0.0").await;

    let result = undo_wave(&engine, &opts("v1.0.0")).await.unwrap();

    assert_eq!(result.nudges_purged, 2, "both v1 nudges");
    let v1 = count_rows(
        &conn,
        "SELECT COUNT(*) FROM take_nudge_log WHERE wave_version = ?1",
        "v1.0.0",
    )
    .await;
    let v2 = count_rows(
        &conn,
        "SELECT COUNT(*) FROM take_nudge_log WHERE wave_version = ?1",
        "v2.0.0",
    )
    .await;
    assert_eq!(v1, 0, "v1 nudge rows physically gone");
    assert_eq!(v2, 1, "other wave untouched");
}

// ── Step 1b: take_grade_cache applied=false (audit rows kept) ────────────

#[tokio::test]
async fn undo_wave_unapplies_grade_cache_keeping_audit_rows() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_grade_cache(&conn, 1, "v1.0.0", true).await;
    seed_grade_cache(&conn, 2, "v1.0.0", false).await; // already unapplied — not counted
    seed_grade_cache(&conn, 3, "v2.0.0", true).await; // other wave — untouched

    let result = undo_wave(&engine, &opts("v1.0.0")).await.unwrap();

    assert_eq!(result.grade_cache_unapplied, 1, "only the applied=true v1 row");
    // Audit trail kept: all 3 rows still exist.
    let total = count_rows(
        &conn,
        "SELECT COUNT(*) FROM take_grade_cache WHERE wave_version IN (?1, 'v2.0.0')",
        "v1.0.0",
    )
    .await;
    assert_eq!(total, 3, "no grade_cache rows deleted");
    // v1 rows now all applied=false; v2 row still applied=true.
    let v1_applied = count_rows(
        &conn,
        "SELECT COUNT(*) FROM take_grade_cache WHERE wave_version = ?1 AND applied = true",
        "v1.0.0",
    )
    .await;
    let v2_applied = count_rows(
        &conn,
        "SELECT COUNT(*) FROM take_grade_cache WHERE wave_version = ?1 AND applied = true",
        "v2.0.0",
    )
    .await;
    assert_eq!(v1_applied, 0, "v1 rows flipped to applied=false");
    assert_eq!(v2_applied, 1, "other wave keeps applied=true");
}

// ── Step 1: revert wave-applied take resolutions ─────────────────────────

#[tokio::test]
async fn undo_wave_reverts_only_wave_applied_auto_resolutions() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    let page_id = seed_page(&engine, "note/a").await;

    // Take 1: auto-resolved by this wave (grade_cache applied=true, v1).
    seed_resolved_take(&conn, 101, page_id, "zbrain:grade_takes").await;
    seed_grade_cache(&conn, 101, "v1.0.0", true).await;
    // Take 2: in this wave's cache but manually re-resolved afterwards —
    // resolved_by cross-check must protect it.
    seed_resolved_take(&conn, 102, page_id, "garry").await;
    seed_grade_cache(&conn, 102, "v1.0.0", true).await;
    // Take 3: auto-resolved by a DIFFERENT wave — untouched.
    seed_resolved_take(&conn, 103, page_id, "zbrain:grade_takes").await;
    seed_grade_cache(&conn, 103, "v2.0.0", true).await;

    let result = undo_wave(&engine, &opts("v1.0.0")).await.unwrap();

    assert_eq!(result.resolutions_reverted, 1, "only take 101");
    // Take 101: all resolved_* columns back to NULL.
    let t101_unresolved = count_rows(
        &conn,
        "SELECT COUNT(*) FROM takes WHERE id = 101 AND resolved_at IS NULL \
         AND resolved_quality IS NULL AND resolved_outcome IS NULL \
         AND resolved_value IS NULL AND resolved_unit IS NULL \
         AND resolved_by IS NULL AND ?1 = ?1",
        "x",
    )
    .await;
    assert_eq!(t101_unresolved, 1, "take 101 fully unresolved");
    // Takes 102/103 keep their resolution.
    let still_resolved = count_rows(
        &conn,
        "SELECT COUNT(*) FROM takes WHERE id IN (102, 103) \
         AND resolved_at IS NOT NULL AND resolved_by IS NOT NULL AND ?1 = ?1",
        "x",
    )
    .await;
    assert_eq!(still_resolved, 2, "manual + other-wave resolutions persist");
}

// ── Cross-cutting behavior ───────────────────────────────────────────────

/// `dry_run` reports the same counts as a real run but writes nothing.
#[tokio::test]
async fn undo_wave_dry_run_counts_without_writing() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_source(&conn, "wiki").await;
    let page_id = seed_page(&engine, "note/dry").await;
    seed_resolved_take(&conn, 201, page_id, "zbrain:grade_takes").await;
    seed_grade_cache(&conn, 201, "v1.0.0", true).await;
    seed_profile(&conn, "wiki", "v1.0.0").await;
    seed_nudge(&conn, "wiki", 201, "v1.0.0").await;

    let mut o = opts("v1.0.0");
    o.dry_run = true;
    let result = undo_wave(&engine, &o).await.unwrap();

    assert!(result.dry_run);
    assert_eq!(result.resolutions_reverted, 1);
    assert_eq!(result.grade_cache_unapplied, 1);
    assert_eq!(result.profiles_deleted, 1);
    assert_eq!(result.nudges_purged, 1);

    // Nothing actually changed.
    let take_resolved = count_rows(
        &conn,
        "SELECT COUNT(*) FROM takes WHERE id = 201 AND resolved_by IS NOT NULL AND ?1 = ?1",
        "x",
    )
    .await;
    let cache_applied = count_rows(
        &conn,
        "SELECT COUNT(*) FROM take_grade_cache WHERE wave_version = ?1 AND applied = true",
        "v1.0.0",
    )
    .await;
    let profiles = count_rows(
        &conn,
        "SELECT COUNT(*) FROM calibration_profiles WHERE wave_version = ?1",
        "v1.0.0",
    )
    .await;
    let nudges = count_rows(
        &conn,
        "SELECT COUNT(*) FROM take_nudge_log WHERE wave_version = ?1",
        "v1.0.0",
    )
    .await;
    assert_eq!(
        (take_resolved, cache_applied, profiles, nudges),
        (1, 1, 1, 1),
        "dry run wrote nothing"
    );
}

/// Re-running against an already-undone wave yields all-zero counts.
#[tokio::test]
async fn undo_wave_is_idempotent() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_source(&conn, "wiki").await;
    let page_id = seed_page(&engine, "note/idem").await;
    seed_resolved_take(&conn, 301, page_id, "zbrain:grade_takes").await;
    seed_grade_cache(&conn, 301, "v1.0.0", true).await;
    seed_profile(&conn, "wiki", "v1.0.0").await;
    seed_nudge(&conn, "wiki", 301, "v1.0.0").await;

    let first = undo_wave(&engine, &opts("v1.0.0")).await.unwrap();
    assert_eq!(
        (
            first.resolutions_reverted,
            first.grade_cache_unapplied,
            first.profiles_deleted,
            first.nudges_purged
        ),
        (1, 1, 1, 1)
    );

    let second = undo_wave(&engine, &opts("v1.0.0")).await.unwrap();
    assert_eq!(
        (
            second.resolutions_reverted,
            second.grade_cache_unapplied,
            second.profiles_deleted,
            second.nudges_purged
        ),
        (0, 0, 0, 0),
        "second run is a no-op"
    );
}

/// A custom `resolved_by_label` targets that label instead of the default.
#[tokio::test]
async fn undo_wave_honors_custom_resolved_by_label() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    let page_id = seed_page(&engine, "note/label").await;
    seed_resolved_take(&conn, 401, page_id, "custom:grader").await;
    seed_grade_cache(&conn, 401, "v1.0.0", true).await;

    // Default label misses the custom-resolved take.
    let miss = undo_wave(&engine, &opts("v1.0.0")).await.unwrap();
    assert_eq!(miss.resolutions_reverted, 0, "default label does not match");

    // Re-seed cache applied flag (first run unapplied it).
    conn.execute(
        "UPDATE take_grade_cache SET applied = true WHERE take_id = 401",
        (),
    )
    .await
    .unwrap();

    let mut o = opts("v1.0.0");
    o.resolved_by_label = Some("custom:grader".to_string());
    let hit = undo_wave(&engine, &o).await.unwrap();
    assert_eq!(hit.resolutions_reverted, 1, "custom label matches");
}

/// `scrub_gstack` is a recorded no-op in the Rust port: never attempted,
/// surfaced as a warning (registered KNOWN-GAP).
#[tokio::test]
async fn undo_wave_gstack_scrub_is_skipped_with_warning() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;

    let mut o = opts("v1.0.0");
    o.scrub_gstack = true;
    let result = undo_wave(&engine, &o).await.unwrap();

    assert!(!result.gstack_scrub_attempted);
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("gstack-learnings-prune"));

    // Dry run: flag is ignored entirely (mirrors TS `scrubGstack && !dryRun`).
    o.dry_run = true;
    let dry = undo_wave(&engine, &o).await.unwrap();
    assert!(!dry.gstack_scrub_attempted);
    assert!(dry.warnings.is_empty());
}
