//! Autopilot-cycle handler — the main cycle orchestrator.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts` — delegates to `runCycle` from
//! `src/core/autopilot/cycle.ts` (~2057 lines). v1 skeleton: runCycle
//! is a 5000+ line subsystem pending its own dedicated port node.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct AutopilotCycleHandler;

#[async_trait]
impl MinionHandler for AutopilotCycleHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        // v1: runCycle not yet ported — see grill Q1 (1-4-3).
        Ok(json!({"status": "not_implemented", "detail": "runCycle pending dedicated port node"}))
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
    async fn autopilot_cycle_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "autopilot-cycle".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = AutopilotCycleHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
