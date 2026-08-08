//! Lint-fix handler — UNSUPPORTED / wontfix.
//!
//! The TS `lint-fix` command was deleted under option C and has no Rust verb.
//! Tracked as G82 in `docs/plans/KNOWN-GAPS.md`.

use async_trait::async_trait;
use serde_json::Value;

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct LintFixHandler;

#[async_trait]
impl MinionHandler for LintFixHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Err(crate::minions::handlers::util::unsupported_job("lint-fix", "G82"))
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
    async fn lint_fix_is_unsupported() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "lint-fix".into(),
            serde_json::json!({}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        assert!(LintFixHandler.handle(&ctx).await.is_err());
    }
}
