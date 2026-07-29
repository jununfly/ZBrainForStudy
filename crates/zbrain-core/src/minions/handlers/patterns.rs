//! Patterns handler — standalone `patterns` minion job.
//!
//! ## TS reference
//!
//! In TS, `patterns` is a cycle phase (`src/core/cycle/patterns.ts`), not a
//! standalone minion handler. This Rust handler reproduces the dispatchable
//! behavior: when a `patterns` job is run, it enqueues the single Sonnet
//! detection subagent (exactly what the cycle phase does) and returns. The
//! subagent itself is executed by the minion worker via the `subagent`
//! handler — so this handler does NOT run the LLM or wait for completion.
//!
//! Skipping (disabled config or insufficient recent reflections) is reported
//! as `{"status":"skipped"}`; otherwise `{"status":"enqueued", "job_id": …}`.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::autopilot::phases::patterns::enqueue_patterns_subagent;
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct PatternsHandler;

#[async_trait]
impl MinionHandler for PatternsHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        match enqueue_patterns_subagent(ctx.engine().as_ref()).await? {
            None => Ok(json!({
                "status": "skipped",
                "detail": "patterns not enqueued: disabled or insufficient recent reflections",
            })),
            Some(job) => Ok(json!({
                "status": "enqueued",
                "job_id": job.id,
                "job_name": job.name,
                "job_status": job.status.as_str(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BrainEngine, EngineConfig};
    use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    async fn engine() -> Arc<dyn BrainEngine> {
        let eng = InMemoryEngine::new();
        // enqueue_patterns_subagent reads reflections via list_pages, which
        // needs a connected engine.
        eng.connect(&EngineConfig::default()).await.unwrap();
        Arc::new(eng)
    }

    #[tokio::test]
    async fn patterns_smoke_does_not_panic() {
        // Empty brain → insufficient reflections → handler reports skipped
        // (does not panic / does not block on a worker).
        let eng = engine().await;
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "patterns".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = PatternsHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "skipped");
        assert!(result["detail"].as_str().unwrap_or("").contains("insufficient"));
    }
}
