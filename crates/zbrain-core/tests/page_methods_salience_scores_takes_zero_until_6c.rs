//! Slice 6a S6-T1 placeholder-lock test: salience score `takes` count is zero until 6c.
//!
//! This is the **strong-semantics** sibling of
//! `page_methods_get_salience_scores.rs`. The generic placeholder test
//! only checks that 6a still returns `Unsupported`; this test pins down
//! a behavioural invariant that must remain true **even after** S6-T2
//! green phase lands the real impl, until slice 6c introduces the
//! `takes` table.
//!
//! Formula (mirrors TS `getSalienceScores`):
//!
//! ```text
//! score = COALESCE(emotional_weight, 0) * 5
//!       + ln(1 + distinct_active_take_count)
//! ```
//!
//! In 6a `distinct_active_take_count` must be hard-coded to `0`, so
//! `ln(1 + 0) = 0` and the score collapses to `emotional_weight * 5`.
//!
//! **Lock phase (now)**: locks the Unsupported placeholder.
//!
//! **Green phase (S6-T2)**: when the impl lands, this test must be
//! rewritten in the SAME commit to insert a Page with
//! `emotional_weight = 0.4`, call `get_salience_scores`, and assert the
//! result is `(0.4 * 5).abs_diff_eq(score) < 1e-9` — proving the takes
//! count contribution is exactly zero.
//!
//! **Slice 6c**: when the `takes` table lands, rewrite again to insert
//! N takes and assert `score = 0.4*5 + ln(1+N)`.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::PageRef;
use zbrain_core::libsql::LibsqlEngine;

async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile) {
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
async fn slice_6a_page_methods_salience_scores_takes_zero_until_6c() {
    let (engine, _tmp) = init_clean_engine().await;
    let refs = vec![PageRef {
        slug: "a".to_string(),
        source_id: "src-1".to_string(),
    }];
    // 6a placeholder-lock: even the strong-semantics test starts as a placeholder lock;
    // S6-T2 rewrites this test (see file-level doc) before flipping green.
    let err = engine
        .get_salience_scores(&refs)
        .await
        .expect_err("6a placeholder-lock: salience scores must be Unsupported (takes-zero contract applies in 6a-pg+T2)");
    let msg = err.to_string();
    assert!(
        msg.contains("pending slice 6a"),
        "expected placeholder marker, got: {msg}"
    );
    engine.disconnect().await.expect("disconnect");
}
