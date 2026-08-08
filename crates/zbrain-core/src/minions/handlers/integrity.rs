//! Integrity handler — runs the read-only brain-integrity scan.
//!
//! Wires the minion `integrity` job to the Rust port of
//! `src/commands/integrity.ts` (`scanIntegrity`): `zbrain_core::integrity::scan_integrity`.
//! The read-only `check` path needs only the engine (no AI gateway, no
//! resolver SDK, no writes) — see `crates/zbrain-core/src/integrity.rs`.
//! The `auto`/`review`/`reset-progress` subcommands remain out of scope
//! (KNOWN-GAPS G51).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::engine::BrainEngine;
use crate::integrity::{scan_integrity, IntegrityScanOptions};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct IntegrityHandler;

#[async_trait]
impl MinionHandler for IntegrityHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let data = &ctx.data;
        let opts = IntegrityScanOptions {
            limit: data.get("limit").and_then(|v| v.as_u64()),
            type_filter: data
                .get("type_filter")
                .and_then(|v| v.as_str())
                .map(String::from),
        };

        let engine = ctx.engine();
        let result = scan_integrity(engine.as_ref(), &opts).await?;
        let value = serde_json::to_value(&result).map_err(|e| {
            crate::Error::new("SerializationError", "integrity_scan", &e.to_string())
        })?;
        Ok(value)
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
    async fn integrity_smoke() {
        let e = Arc::new(InMemoryEngine::new());
        let r = IntegrityHandler
            .handle(&MinionJobContext::new(
                Arc::clone(&e) as Arc<dyn BrainEngine>,
                1,
                "integrity".into(),
                json!({}),
                0,
                "t".into(),
                CancellationToken::new(),
                CancellationToken::new(),
            ))
            .await
            .unwrap();
        // Wired to scan_integrity: result carries the scan report shape
        // (camelCase serialized), not the old "not_implemented" stub.
        assert!(r.get("pagesScanned").is_some(), "expected integrity scan report, got: {r}");
        assert!(r.get("bareHits").is_some());
    }
}
