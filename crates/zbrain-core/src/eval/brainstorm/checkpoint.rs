//! v0.37.x — brainstorm checkpoint (P7) with full idea bodies.
//!
//! Faithful port of `src/core/checkpoint.ts`. Contracts (locked by
//! /plan-eng-review):
//!   - `compute_run_id` = sha256(question + profile_label + sort(close) +
//!     sort(far)).slice(0,16). NO embedding bits — stable across
//!     embedding-model swaps.
//!
//! Run persistence (1-1-5-9): `save_checkpoint` / `list_runs` /
//! `gc_stale_checkpoints` / `clear_checkpoint` are now real and delegate to
//! the filesystem [`store`] (one `<run_id>.json` per run under the brainstorm
//! run-store dir). `load_checkpoint` loads a single run row for resume
//! playback (wired at 1-1-5-11). All take an explicit `store_dir` so the CLI's
//! `--store-dir` override is honored everywhere.

use std::path::Path;

use anyhow::Result;
use sha2::Digest;

use super::orchestrator::BrainstormResult;
use super::store::{self, BrainstormRunRow, BrainstormRunSummary};

/// Schema version for the on-disk checkpoint payload.
pub const CURRENT_SCHEMA: u16 = 2;
/// 7-day staleness window (A5) — the default GC horizon.
pub const STALE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// A5 amended identity: sha256(question + profile + sort(close) + sort(far))
/// truncated to 16 hex chars. No embedding bits — embedding-model swaps
/// don't break checkpoints. Mirrors TS `computeRunId` byte-for-byte.
#[must_use]
pub fn compute_run_id(
    question: &str,
    profile_label: &str,
    close_slugs: &[String],
    far_slugs: &[String],
) -> String {
    let mut close = close_slugs.to_vec();
    close.sort();
    let mut far = far_slugs.to_vec();
    far.sort();
    // TS concatenates question + profileLabel + JSON.stringify(sortedClose)
    // + JSON.stringify(sortedFar) with no separators. `serde_json::to_string`
    // emits `["a","b"]` (no spaces) — identical to `JSON.stringify` on a
    // string array — so the byte payload matches exactly.
    let payload = format!(
        "{}{}{}{}",
        question,
        profile_label,
        serde_json::to_string(&close).expect("string vec serializes"),
        serde_json::to_string(&far).expect("string vec serializes"),
    );
    let hash = sha2::Sha256::digest(payload.as_bytes());
    hex::encode(hash)[..16].to_string()
}

/// Persist a run to `store_dir` (atomic `.tmp`+rename). Returns the path.
pub fn save_checkpoint(result: &BrainstormResult, store_dir: &Path) -> Result<std::path::PathBuf> {
    store::save_run(result, store_dir)
}

/// List persisted runs in `store_dir`, newest first.
#[must_use]
pub fn list_runs(store_dir: &Path) -> Vec<BrainstormRunSummary> {
    store::list_runs(store_dir)
}

/// Reclaim runs in `store_dir` older than `max_age_days` (mtime-based).
#[must_use]
pub fn gc_stale_checkpoints(store_dir: &Path, max_age_days: u64) -> u64 {
    store::gc_stale_runs(store_dir, max_age_days)
}

/// Delete a single run by id from `store_dir`.
#[must_use]
pub fn clear_checkpoint(store_dir: &Path, run_id: &str) -> bool {
    store::clear_run(store_dir, run_id)
}

/// Load a single run row by id (resume playback, 1-1-5-11). Returns `None`
/// when the run is absent or corrupt.
#[must_use]
pub fn load_checkpoint(store_dir: &Path, run_id: &str) -> Option<BrainstormRunRow> {
    store::load_run(store_dir, run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_is_16_hex_chars() {
        let id = compute_run_id(
            "what if gravity is information?",
            "brainstorm",
            &["people/maria".to_string(), "wiki/vc".to_string()],
            &["concepts/drift".to_string()],
        );
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn run_id_is_stable_and_order_independent() {
        let close = vec!["b".to_string(), "a".to_string()];
        let far = vec!["y".to_string(), "x".to_string()];
        let id1 = compute_run_id("q", "p", &close, &far);
        // Same multiset in a different order must produce the identical id.
        let id2 = compute_run_id(
            "q",
            "p",
            &["a".to_string(), "b".to_string()],
            &["x".to_string(), "y".to_string()],
        );
        assert_eq!(id1, id2);
    }

    #[test]
    fn run_id_changes_with_question() {
        let a = compute_run_id("q1", "p", &[], &[]);
        let b = compute_run_id("q2", "p", &[], &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn checkpoint_facade_roundtrip() {
        let dir = std::env::temp_dir().join(format!("bs_cp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_file(e.path());
        }
        let result = crate::eval::brainstorm::orchestrator::BrainstormResult {
            profile_label: "brainstorm",
            question: "q".to_string(),
            embedding_model: None,
            ideas: vec![],
            close_set: vec![],
            far_set: vec![],
            active_bias_tags: None,
            short_of_target: false,
            judge_failed: false,
            cost: crate::eval::brainstorm::orchestrator::BrainstormCost::default(),
            run_id: "0123456789abcdef".to_string(),
        };
        let path = save_checkpoint(&result, &dir).unwrap();
        assert!(path.exists());
        assert_eq!(list_runs(&dir).len(), 1);
        assert!(clear_checkpoint(&dir, "0123456789abcdef"));
        assert!(list_runs(&dir).is_empty());
    }
}
