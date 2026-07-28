//! 1-3-3-7 — `run_calibration_profile` D-style contract against a real libsql
//! temp DB.
//!
//! Mirrors `tests/think_ab.rs`: a real `LibsqlEngine` over a temp file
//! (migrations auto-create `calibration_profiles` + `sources`). `source_id`
//! uses the auto-seeded `'default'` source so the FK holds without fabrication
//! (G52). Every LLM dependency is replaced by a hermetic stub
//! (`StubPatternsGen` / `StubBiasGen` / `StubJudge` / `DenyBudgetGate`) so the
//! suite runs fully offline. Coverage:
//!   * cold-brain short-circuit (`resolved < 5` → skip, no write)
//!   * full write + read-back (pattern statements, bias tags, scorecard)
//!   * budget-exhausted short-circuit (no write)
//!   * FK-contract error on an unknown `source_id` (never fabricated/silent)

use async_trait::async_trait;
use libsql::{params, Builder, Connection};
use std::sync::Arc;
use tempfile::NamedTempFile;
use zbrain_core::calibration::{
    run_calibration_profile, BudgetDecision, BudgetGate, CalibrationProfileError,
    CalibrationProfileOpts, CalibrationProfileStatus, PatternStatementsGenInput,
    PatternStatementsGenerator, BiasTagsGenInput, BiasTagsGenerator,
};
use zbrain_core::calibration::voice_gate::{VoiceGateError, VoiceGateMode, VoiceGateVerdict, VoiceJudge, VoiceVerdict};
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

async fn raw_conn(path: &std::path::Path) -> Connection {
    Builder::new_local(path).build().await.unwrap().connect().unwrap()
}

/// Seed >=5 resolved takes for `holder`. Migration 0012 already widened
/// `takes` with `holder`/`kind`/`weight`/`resolved_quality`, so no ALTER is
/// needed. A raw libsql connection has FK off, so `page_id = 0` is fine.
async fn seed_resolved_takes(conn: &Connection, holder: &str) {
    // The temp engine enforces FK; our raw seed connection relaxes it so we can
    // insert resolved takes without manufacturing real pages.
    conn.execute("PRAGMA foreign_keys = OFF", params![])
        .await
        .unwrap();
    for i in 0..6u32 {
        let quality = if i % 2 == 0 { "correct" } else { "incorrect" };
        let weight = (i as f64) * 0.1;
        conn.execute(
            "INSERT INTO takes (page_id, holder, kind, weight, resolved_quality) VALUES (0, ?1, 'bet', ?2, ?3)",
            params![holder, weight, quality],
        )
        .await
        .unwrap();
    }
}

/// Stub pattern-statements generator: fixed lines, no LLM.
struct StubPatternsGen;

#[async_trait]
impl PatternStatementsGenerator for StubPatternsGen {
    async fn generate(
        &self,
        _input: PatternStatementsGenInput,
    ) -> Result<Vec<String>, CalibrationProfileError> {
        Ok(vec![
            "You read the room well.".to_string(),
            "Macro calls are your blind spot.".to_string(),
        ])
    }
}

/// Stub bias-tags generator: fixed kebab tag, no LLM.
struct StubBiasGen;

#[async_trait]
impl BiasTagsGenerator for StubBiasGen {
    async fn generate(&self, _input: BiasTagsGenInput) -> Result<Vec<String>, CalibrationProfileError> {
        Ok(vec!["macro-blind-spot".to_string()])
    }
}

/// Stub judge: always conversational (passes the gate), no LLM.
struct StubJudge;

#[async_trait]
impl VoiceJudge for StubJudge {
    async fn judge(
        &self,
        _candidate: &str,
        _mode: VoiceGateMode,
        _rubric: &str,
    ) -> Result<VoiceGateVerdict, VoiceGateError> {
        Ok(VoiceGateVerdict {
            verdict: VoiceVerdict::Conversational,
            reason: "stub".to_string(),
        })
    }
}

/// Budget gate that always denies — exercises the budget-exhausted path.
struct DenyBudgetGate;

impl BudgetGate for DenyBudgetGate {
    fn allowed(&self, _est_input_tokens: u32, _max_output_tokens: u32, _model_id: &str) -> BudgetDecision {
        BudgetDecision {
            allowed: false,
            budget_usd: 0.0,
        }
    }
}

/// Build the standard stubbed opts for `holder` against the seeded `'default'`
/// source. All LLM deps injected, so no `chat` provider is required.
fn stub_opts(holder: &str, budget_gate: Option<Arc<dyn BudgetGate>>) -> CalibrationProfileOpts {
    CalibrationProfileOpts {
        holder: Some(holder.to_string()),
        source_id: Some("default".to_string()),
        patterns_generator: Some(Arc::new(StubPatternsGen)),
        bias_tags_generator: Some(Arc::new(StubBiasGen)),
        voice_gate_judge: Some(Arc::new(StubJudge)),
        budget_gate,
        ..Default::default()
    }
}

#[tokio::test]
async fn calibration_profile_cold_brain_skips() {
    let _guard = libsql_test_guard();
    // Empty DB: no resolved takes -> cold-brain short-circuit, no LLM, no write.
    let (_temp, engine) = temp_engine().await;
    let result = run_calibration_profile(&engine, &stub_opts("garry", None))
        .await
        .expect("run_calibration_profile");

    assert_eq!(result.status, CalibrationProfileStatus::Skipped);
    assert_eq!(result.skipped.as_deref(), Some("insufficient_data"));
    assert!(!result.profile_written, "cold brain must not write a profile");
    assert!(result.pattern_statements.is_empty());
}

#[tokio::test]
async fn calibration_profile_writes_and_reads_back() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_resolved_takes(&conn, "garry").await;

    let result = run_calibration_profile(&engine, &stub_opts("garry", None))
        .await
        .expect("run_calibration_profile");

    assert!(result.profile_written, "profile should be written");
    assert_eq!(result.status, CalibrationProfileStatus::Ok);
    assert!(result.voice_gate_passed);
    assert_eq!(result.voice_gate_attempts, 1);
    assert_eq!(
        result.pattern_statements,
        vec!["You read the room well.", "Macro calls are your blind spot."]
    );
    assert_eq!(result.active_bias_tags, vec!["macro-blind-spot"]);
    assert_eq!(result.total_resolved, 6);

    // Read the row back via a raw connection (no execute_raw by design).
    let mut rows = conn
        .query(
            "SELECT source_id, holder, total_resolved, voice_gate_passed, voice_gate_attempts, \
             model_id, pattern_statements, active_bias_tags, domain_scorecards \
             FROM calibration_profiles WHERE source_id = ?1 AND holder = ?2",
            params!["default", "garry"],
        )
        .await
        .expect("read back");
    let row = rows.next().await.expect("row cursor").expect("row exists");

    assert_eq!(row.get::<String>(0).unwrap(), "default");
    assert_eq!(row.get::<String>(1).unwrap(), "garry");
    assert_eq!(row.get::<i32>(2).unwrap(), 6);
    assert_eq!(row.get::<i32>(3).unwrap(), 1); // voice_gate_passed
    assert_eq!(row.get::<i32>(4).unwrap(), 1); // voice_gate_attempts
    assert_eq!(row.get::<String>(5).unwrap(), "claude-sonnet-4-6");

    let pats = row.get::<String>(6).unwrap();
    assert!(pats.contains("You read the room well."));
    assert!(pats.contains("Macro calls are your blind spot."));
    assert!(pats.starts_with('[') && pats.ends_with(']'));

    let tags = row.get::<String>(7).unwrap();
    assert!(tags.contains("macro-blind-spot"));
    assert!(tags.starts_with('[') && tags.ends_with(']'));

    // No active domains -> {} (R1 byte-identical regression).
    assert_eq!(row.get::<String>(8).unwrap(), "{}");
}

#[tokio::test]
async fn calibration_profile_budget_exhausted_skips() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_resolved_takes(&conn, "garry").await;

    let result = run_calibration_profile(&engine, &stub_opts("garry", Some(Arc::new(DenyBudgetGate))))
        .await
        .expect("run_calibration_profile");

    assert_eq!(result.status, CalibrationProfileStatus::Warn);
    assert_eq!(result.skipped.as_deref(), Some("budget_exhausted"));
    assert!(!result.profile_written, "budget gate must prevent write");

    // No profile row should exist.
    let mut rows = conn
        .query(
            "SELECT COUNT(*) AS n FROM calibration_profiles WHERE source_id = ?1 AND holder = ?2",
            params!["default", "garry"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i32>(0).unwrap(), 0);
}

#[tokio::test]
async fn calibration_profile_unknown_source_errors() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;
    seed_resolved_takes(&conn, "garry").await;

    let opts = CalibrationProfileOpts {
        source_id: Some("no-such-source".to_string()),
        ..stub_opts("garry", None)
    };
    let err = run_calibration_profile(&engine, &opts)
        .await
        .expect_err("FK violation must error");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("foreign key") || msg.contains("insert_calibration_profile"),
        "unexpected error: {msg}"
    );
}
