//! Extract handler — UNSUPPORTED / wontfix.
//!
//! The TS `extract` command was deleted under option C and has no Rust verb.
//! (Distinct from `extract_facts` / `extract-conversation-facts`, which are wired
//! through `run_cycle`.) Tracked as G76 in `docs/plans/KNOWN-GAPS.md` (the
//! command-level gap; this minion job type shares that disposition).

use async_trait::async_trait;
use serde_json::Value;

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ExtractHandler;

#[async_trait]
impl MinionHandler for ExtractHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Err(crate::minions::handlers::util::unsupported_job("extract", "G76"))
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
    async fn extract_is_unsupported() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "extract".into(),
            serde_json::json!({}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        assert!(ExtractHandler.handle(&ctx).await.is_err());
    }
}
