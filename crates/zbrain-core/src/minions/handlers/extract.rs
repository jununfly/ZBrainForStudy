//! Extract handler — UNSUPPORTED (minion surface only).
//!
//! The TS `extract` command was deleted under option C. As of 2026-08-09 a CLI
//! verb **does** exist — `zbrain extract {links,timeline,all}` (see
//! `crates/zbrain-cli/src/lib.rs`, backed by `auto_fix::extract_links` /
//! `auto_fix::extract_timeline`) — so the earlier "has no Rust verb" note is stale.
//!
//! This *minion job type* stays unsupported on purpose: enqueueing `extract`
//! would run a whole-brain extraction that writes link/timeline rows, and the
//! job-payload contract (scope by slug? whole brain? dry-run?) has not been
//! decided. Wiring it is registered as node 1-2-4 in
//! `docs/plans/zbrain-g74-g76-reimpl.json`.
//!
//! (Distinct from `extract_facts` / `extract-conversation-facts`, which are wired
//! through `run_cycle`.) Tracked as G76 in `docs/plans/KNOWN-GAPS.md`.

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
