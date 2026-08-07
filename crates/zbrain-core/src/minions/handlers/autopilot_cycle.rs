//! Autopilot-cycle handler — the main cycle orchestrator.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts` — delegates to `runCycle` from
//! `src/core/autopilot/cycle.ts` (~2057 lines). Wired to the Rust
//! `run_cycle` orchestrator (the Part12 port of the cycle). This handler
//! runs a full maintenance cycle (all phases) by calling `run_cycle` with
//! `phases: None`, mirroring the TS `runCycle` default and the CLI
//! `zbrain dream` command.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::autopilot::cycle::{run_cycle, CycleOpts, CyclePhase};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct AutopilotCycleHandler;

#[async_trait]
impl MinionHandler for AutopilotCycleHandler {
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
            phases: None,
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
    async fn autopilot_cycle_runs_full_cycle() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "autopilot-cycle".into(), json!({"dry_run": true}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = AutopilotCycleHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert!(
            result.get("status").is_some(),
            "expected a CycleReport JSON with a status field, got: {result}"
        );
    }
}
