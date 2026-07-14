//! Sync-retry-failed handler — retries pages that failed during a previous
//! sync run.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts:1266` — `runSync(engine, ['--retry-failed'])`.
//! v1 skeleton: pending full sync pipeline migration.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct SyncRetryFailedHandler;

#[async_trait]
impl MinionHandler for SyncRetryFailedHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "sync-retry-failed pending CLI migration"}))
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

    #[tokio::test]
    async fn sync_retry_failed_smoke() {
        let eng = Arc::new(InMemoryEngine::new());
        let ctx = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "sync-retry-failed".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let result = SyncRetryFailedHandler.handle(&ctx).await.unwrap();
        assert_eq!(result["status"], "not_implemented");
    }
}
