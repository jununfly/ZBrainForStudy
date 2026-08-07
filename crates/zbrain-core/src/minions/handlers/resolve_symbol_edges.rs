//! Resolve-symbol-edges handler — resolve symbol edges phase of the autopilot cycle.
//!
//! ## TS reference
//!
//! Registered as `"resolve_symbol_edges"` in `src/commands/jobs.ts`. Core logic
//! in `src/core/autopilot/cycle.ts` resolve_symbol_edges phase. Wired to the
//! Rust `run_cycle` orchestrator (Part12 port) via
//! `CyclePhase::ResolveSymbolEdges`.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::autopilot::cycle::{run_cycle, CycleOpts, CyclePhase};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ResolveSymbolEdgesHandler;

#[async_trait]
impl MinionHandler for ResolveSymbolEdgesHandler {
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
            phases: Some(vec![CyclePhase::ResolveSymbolEdges]),
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
    async fn resolve_symbol_edges_runs_phase() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "resolve_symbol_edges".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ResolveSymbolEdgesHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert!(
            result.get("status").is_some(),
            "expected a CycleReport JSON with a status field, got: {result}"
        );
    }
}
