//! takes-quality-eval/replay — load a prior receipt without running models.
//!
//! Faithful port of TS `replay.ts`. Disk-first (no DB connection required),
//! and explicitly does NOT silently fall through to the DB. If the user
//! passed an explicit receipt path, they expect that file to exist; silent
//! DB fallback hides a missing-file error. For the disk-missing-but-in-DB
//! case, use [`load_receipt_from_db`] (engine arg) — separate code path,
//! separate user intent.

use std::path::Path;

use anyhow::anyhow;
use crate::engine::BrainEngine;
use crate::eval::takes_quality::receipt::{ReceiptIdentity, TakesQualityReceipt, RECEIPT_SCHEMA_VERSION};

/// Read a receipt from disk. The path can be absolute or relative; if just a
/// filename is given, the caller is expected to have already resolved it to an
/// absolute path (via the receipt-name builder).
pub fn load_receipt_from_disk(receipt_path: &Path) -> anyhow::Result<TakesQualityReceipt> {
    if !receipt_path.exists() {
        anyhow::bail!(
            "Receipt file not found: {}. If the disk artifact was lost but the run was \
             recorded in DB, re-export with `zbrain eval takes-quality replay --from-db <id>`.",
            receipt_path.display()
        );
    }
    let raw = std::fs::read_to_string(receipt_path).map_err(|e| anyhow!("read receipt: {e}"))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| anyhow!("receipt is not valid JSON: {e}"))?;
    if parsed.get("schema_version").and_then(|v| v.as_u64()) != Some(RECEIPT_SCHEMA_VERSION as u64) {
        anyhow::bail!(
            "Unsupported receipt schema_version (expected {}). Receipt was likely produced by a \
             newer zbrain; upgrade to read it.",
            RECEIPT_SCHEMA_VERSION
        );
    }
    let receipt: TakesQualityReceipt = serde_json::from_value(parsed)
        .map_err(|e| anyhow!("receipt JSON does not match schema: {e}"))?;
    Ok(receipt)
}

/// Reconstruct a receipt from the DB row's `receipt_json` column. Used as the
/// explicit fallback path when the disk artifact is gone.
pub async fn load_receipt_from_db(
    engine: &dyn BrainEngine,
    identity: &ReceiptIdentity,
) -> anyhow::Result<TakesQualityReceipt> {
    let json = engine.load_takes_quality_run(identity).await?;
    match json {
        Some(v) => {
            let receipt: TakesQualityReceipt = serde_json::from_value(v)
                .map_err(|e| anyhow!("DB receipt_json does not match schema: {e}"))?;
            Ok(receipt)
        }
        None => anyhow::bail!(
            "No DB row matching the requested 4-sha receipt identity. Either the run never \
             persisted or it was pruned."
        ),
    }
}
