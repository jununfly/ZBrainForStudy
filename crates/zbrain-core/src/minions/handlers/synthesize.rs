//! Synthesize handler — synthesize phase of the autopilot cycle.
//!
//! ## TS reference
//!
//! Registered as `"synthesize"` in `src/commands/jobs.ts`. Core logic in
//! `src/core/autopilot/cycle.ts` synthesize phase. v1 skeleton: runCycle
//! not yet ported.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct SynthesizeHandler;

#[async_trait]
impl MinionHandler for SynthesizeHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "synthesize phase pending runCycle port"}))
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
    async fn synthesize_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "synthesize".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = SynthesizeHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
