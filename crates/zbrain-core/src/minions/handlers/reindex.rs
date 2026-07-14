//! Reindex handler — re-parses markdown pages and rebuilds chunks.
//! v1 skeleton: reindex pipeline pending CLI migration.

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ReindexHandler;
#[async_trait]
impl MinionHandler for ReindexHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "reindex pending CLI migration"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*; use crate::engine::BrainEngine; use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine; use std::sync::Arc; use tokio_util::sync::CancellationToken;
    #[tokio::test] async fn reindex_smoke() {
        let e=Arc::new(InMemoryEngine::new());
        let r=ReindexHandler.handle(&MinionJobContext::new(Arc::clone(&e) as Arc<dyn BrainEngine>,1,"reindex".into(),json!({}),0,"t".into(),CancellationToken::new(),CancellationToken::new())).await.unwrap();
        assert_eq!(r["status"],"not_implemented");
    }
}
