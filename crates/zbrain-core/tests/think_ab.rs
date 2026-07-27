//! 1-3-3-6 — `runAbTrial` / `build_ab_report` D-style contract against a real
//! libsql temp DB.
//!
//! Mirrors `tests/libsql_cross_brain.rs`: a real `LibsqlEngine` over a temp
//! file (migrations auto-create `think_ab_results` + `sources`). `source_id`
//! uses the auto-seeded `'default'` source so the FK holds without fabrication
//! (G52). A `StubThinkRunner` + `StubPreferenceResolver` drive the orchestration
//! hermetically; assertions cover the INSERT, row read-back, and the
//! `build_ab_report` aggregation incl. the `calibration_net_negative` threshold
//! and the time-window cutoff.

use async_trait::async_trait;
use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::calibration::{
    build_ab_report, run_ab_trial, AbPreference, AbReportOpts, AbRunInput, PreferenceResolver,
    ThinkAbError, ThinkRunner, ThinkRunAnswer,
};
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;

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

async fn raw_conn(path: &std::path::Path) -> libsql::Connection {
    Builder::new_local(path).build().await.unwrap().connect().unwrap()
}

/// Stub think runner: returns distinct answers for baseline vs with-calibration.
struct StubThinkRunner {
    baseline: String,
    with_calibration: String,
    model: Option<String>,
}

#[async_trait]
impl ThinkRunner for StubThinkRunner {
    async fn run(&self, _question: &str, with_calibration: bool) -> Result<ThinkRunAnswer, ThinkAbError> {
        Ok(ThinkRunAnswer {
            answer: if with_calibration {
                self.with_calibration.clone()
            } else {
                self.baseline.clone()
            },
            model_used: self.model.clone(),
        })
    }
}

/// Stub preference resolver: always returns the configured preference.
struct StubPreferenceResolver {
    preference: AbPreference,
}

#[async_trait]
impl PreferenceResolver for StubPreferenceResolver {
    async fn resolve(
        &self,
        _baseline: &str,
        _with_calibration: &str,
    ) -> Result<AbPreference, ThinkAbError> {
        Ok(self.preference)
    }
}

#[tokio::test]
async fn run_ab_trial_inserts_and_reads_back() {
    let (temp, engine) = temp_engine().await;

    let input = AbRunInput {
        question: "should I ship Friday?".to_string(),
        source_id: "default".to_string(),
        queries: &engine,
        think_runner: std::sync::Arc::new(StubThinkRunner {
            baseline: "Ship it.".to_string(),
            with_calibration: "Ship it, but watch the rollback.".to_string(),
            model: Some("haiku".to_string()),
        }),
        preference_resolver: std::sync::Arc::new(StubPreferenceResolver {
            preference: AbPreference::Baseline,
        }),
        notes: Some("manual test".to_string()),
    };

    let result = run_ab_trial(&input).await.expect("run_ab_trial");
    assert_eq!(result.baseline_answer, "Ship it.");
    assert_eq!(result.with_calibration_answer, "Ship it, but watch the rollback.");
    assert_eq!(result.preferred, AbPreference::Baseline);
    assert_eq!(result.model_used.as_deref(), Some("haiku"));
    assert!(result.row_id.is_some(), "row_id should come back from RETURNING id");

    // Read the row back via a raw libsql connection (BrainEngine has no
    // execute_raw escape hatch by design; direct conn is fine in tests).
    let conn = raw_conn(temp.path()).await;
    let mut rows = conn
        .query(
            "SELECT source_id, question, baseline_answer, with_calibration_answer, preferred, model_id, notes \
             FROM think_ab_results WHERE id = ?1",
            libsql::params![result.row_id.unwrap()],
        )
        .await
        .expect("read back");
    let row = rows.next().await.expect("row cursor").expect("row exists");
    assert_eq!(row.get::<String>(0).unwrap(), "default");
    assert_eq!(row.get::<String>(1).unwrap(), "should I ship Friday?");
    assert_eq!(row.get::<String>(2).unwrap(), "Ship it.");
    assert_eq!(row.get::<String>(3).unwrap(), "Ship it, but watch the rollback.");
    assert_eq!(row.get::<String>(4).unwrap(), "baseline");
    assert_eq!(row.get::<Option<String>>(5).unwrap().as_deref(), Some("haiku"));
    assert_eq!(row.get::<Option<String>>(6).unwrap().as_deref(), Some("manual test"));
}

/// FK contract (G52): an unknown source_id must surface as an error, never a
/// fabricated source or a silent drop.
#[tokio::test]
async fn run_ab_trial_unknown_source_errors() {
    let (_temp, engine) = temp_engine().await;

    let input = AbRunInput {
        question: "q".to_string(),
        source_id: "no-such-source".to_string(),
        queries: &engine,
        think_runner: std::sync::Arc::new(StubThinkRunner {
            baseline: "b".to_string(),
            with_calibration: "w".to_string(),
            model: None,
        }),
        preference_resolver: std::sync::Arc::new(StubPreferenceResolver {
            preference: AbPreference::Tie,
        }),
        notes: None,
    };

    let err = run_ab_trial(&input).await.expect_err("FK violation must error");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("foreign key") || msg.contains("insert_think_ab_result"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn build_ab_report_aggregates_and_flags_net_negative() {
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;

    // 20 baseline, 5 with_calibration, 2 neither, 1 tie — all within the 30d
    // window. Plus 3 baseline rows that are OLD (outside the window) and must
    // be excluded by the cutoff.
    for i in 0..20 {
        conn.execute(
            "INSERT INTO think_ab_results (source_id, question, baseline_answer, with_calibration_answer, preferred) VALUES ('default', 'q', 'b', 'w', 'baseline')",
            libsql::params![],
        ).await.unwrap();
        let _ = i;
    }
    for _ in 0..5 {
        conn.execute(
            "INSERT INTO think_ab_results (source_id, question, baseline_answer, with_calibration_answer, preferred) VALUES ('default', 'q', 'b', 'w', 'with_calibration')",
            libsql::params![],
        ).await.unwrap();
    }
    for _ in 0..2 {
        conn.execute(
            "INSERT INTO think_ab_results (source_id, question, baseline_answer, with_calibration_answer, preferred) VALUES ('default', 'q', 'b', 'w', 'neither')",
            libsql::params![],
        ).await.unwrap();
    }
    for _ in 0..1 {
        conn.execute(
            "INSERT INTO think_ab_results (source_id, question, baseline_answer, with_calibration_answer, preferred) VALUES ('default', 'q', 'b', 'w', 'tie')",
            libsql::params![],
        ).await.unwrap();
    }
    for _ in 0..3 {
        conn.execute(
            "INSERT INTO think_ab_results (source_id, question, baseline_answer, with_calibration_answer, preferred, ran_at) VALUES ('default', 'q', 'b', 'w', 'baseline', '2000-01-01T00:00:00Z')",
            libsql::params![],
        ).await.unwrap();
    }

    let report = build_ab_report(&engine, &AbReportOpts { days: 30 })
        .await
        .expect("build_ab_report");

    assert_eq!(report.total_trials, 28); // 20+5+2+1, old 3 excluded
    assert_eq!(report.baseline_wins, 20);
    assert_eq!(report.with_calibration_wins, 5);
    assert_eq!(report.neither, 2);
    assert_eq!(report.ties, 1);
    assert_eq!(report.decisive_trials, 25);
    assert!((report.with_calibration_win_rate.unwrap() - 0.2).abs() < 1e-9);
    assert!(report.net_negative, "with-calibration losing 80% of decisive trials should flag net_negative");
}

#[tokio::test]
async fn build_ab_report_no_decisive_no_net_negative() {
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(temp.path()).await;

    // Only ties/neither -> no decisive trials -> win_rate None, net_negative false.
    for _ in 0..4 {
        conn.execute(
            "INSERT INTO think_ab_results (source_id, question, baseline_answer, with_calibration_answer, preferred) VALUES ('default', 'q', 'b', 'w', 'tie')",
            libsql::params![],
        ).await.unwrap();
    }

    let report = build_ab_report(&engine, &AbReportOpts { days: 30 })
        .await
        .expect("build_ab_report");
    assert_eq!(report.total_trials, 4);
    assert_eq!(report.decisive_trials, 0);
    assert!(report.with_calibration_win_rate.is_none());
    assert!(!report.net_negative);
}
