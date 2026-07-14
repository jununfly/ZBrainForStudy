//! Extract handler — extracts links and timeline metadata from pages.
//! v1 skeleton: extraction logic pending CLI migration.

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ExtractHandler;
#[async_trait]
impl MinionHandler for ExtractHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "extract pending CLI migration"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*; use crate::engine::BrainEngine; use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine; use std::sync::Arc; use tokio_util::sync::CancellationToken;
    #[tokio::test] async fn extract_smoke() {
        let e = Arc::new(InMemoryEngine::new());
        let r = ExtractHandler.handle(&MinionJobContext::new(Arc::clone(&e) as Arc<dyn BrainEngine>,1,"extract".into(),json!({}),0,"t".into(),CancellationToken::new(),CancellationToken::new())).await.unwrap();
        assert_eq!(r["status"],"not_implemented");
    }
}
