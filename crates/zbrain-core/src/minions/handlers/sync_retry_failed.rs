//! Sync-retry-failed handler — UNSUPPORTED / wontfix.
//!
//! The TS `sync-retry-failed` command was deleted under option C and has no
//! Rust verb. Failed syncs can be retried via the wired `sync` handler. Tracked
//! as G83 in `docs/plans/KNOWN-GAPS.md`.

use async_trait::async_trait;
use serde_json::Value;

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct SyncRetryFailedHandler;

#[async_trait]
impl MinionHandler for SyncRetryFailedHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Err(crate::minions::handlers::util::unsupported_job(
            "sync-retry-failed",
            "G83",
        ))
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
    async fn sync_retry_failed_is_unsupported() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "sync-retry-failed".into(),
            serde_json::json!({}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        assert!(SyncRetryFailedHandler.handle(&ctx).await.is_err());
    }
}
