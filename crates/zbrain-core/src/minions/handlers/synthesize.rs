//! Synthesize handler — synthesize phase of the autopilot cycle.
//!
//! ## TS reference
//!
//! Registered as `"synthesize"` in `src/commands/jobs.ts`. Core logic in
//! `src/core/autopilot/cycle.ts` synthesize phase. Wired to the Rust
//! `run_cycle` orchestrator (Part12 port) via `CyclePhase::Synthesize`.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::autopilot::cycle::{run_cycle, CycleOpts, CyclePhase};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct SynthesizeHandler;

#[async_trait]
impl MinionHandler for SynthesizeHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let data = &ctx.data;
        let brain_dir = data
            .get("dir")
            .or_else(|| data.get("brain_dir"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let opts = CycleOpts {
            dry_run: data.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false),
            phases: Some(vec![CyclePhase::Synthesize]),
            brain_dir,
            pull: data.get("pull").and_then(|v| v.as_bool()).unwrap_or(false),
            source_id: data.get("source_id").and_then(|v| v.as_str()).map(String::from),
            ..Default::default()
        };
        let engine = ctx.engine();
        let report = run_cycle(engine.as_ref(), &opts).await;
        let value = serde_json::to_value(&report)
            .map_err(|e| crate::Error::new("SerializationError", "cycle_report", &e.to_string()))?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    #[tokio::test]
    async fn synthesize_runs_phase() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "synthesize".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = SynthesizeHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert!(
            result.get("status").is_some(),
            "expected a CycleReport JSON with a status field, got: {result}"
        );
    }
}
