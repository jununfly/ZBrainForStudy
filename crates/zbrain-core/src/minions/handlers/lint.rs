//! Lint handler — UNSUPPORTED / wontfix.
//!
//! The TS `lint` command was deleted under option C and has no Rust verb. The
//! cycle's lint behavior is covered by `run_cycle` phases, not this job type.
//! Tracked as G81 in `docs/plans/KNOWN-GAPS.md`.

use async_trait::async_trait;
use serde_json::Value;

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct LintHandler;

#[async_trait]
impl MinionHandler for LintHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Err(crate::minions::handlers::util::unsupported_job("lint", "G81"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn lint_is_unsupported() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "lint".into(),
            serde_json::json!({}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        assert!(LintHandler.handle(&ctx).await.is_err());
    }
}
