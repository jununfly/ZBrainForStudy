//! v0.37.x — brainstorm checkpoint (P7) with full idea bodies.
//!
//! Faithful port of `src/core/checkpoint.ts`. Contracts (locked by
//! /plan-eng-review):
//!   - `compute_run_id` = sha256(question + profile_label + sort(close) +
//!     sort(far)).slice(0,16). NO embedding bits — stable across
//!     embedding-model swaps.
//!
//! Q3 MVP scope: only `compute_run_id` is implemented and tested. The
//! save/load/list/gc/clear machinery (filesystem JSON, 7-day mtime GC,
//! resume playback that merges `completed_crosses` into the new run) is
//! TODO — the orchestrator in this slice does NOT call resume, so stubbing
//! these as safe no-ops keeps the surface honest without blocking the build.

use sha2::{Digest, Sha256};

/// Schema version for the (future) on-disk checkpoint payload.
pub const CURRENT_SCHEMA: u16 = 2;
/// 7-day staleness window (A5).
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
    let hash = Sha256::digest(payload.as_bytes());
    hex::encode(hash)[..16].to_string()
}

// ── Q3 MVP: resume machinery is TODO ────────────────────────────────────────
//
// The orchestrator in this slice does not call resume, so these stubs are
// safe no-ops that preserve the module surface. They will be filled in when
// resume playback is wired (full idea bodies per TX3, one --resume flag per
// TX4, atomic .tmp+rename save, mtime-based GC).

/// Q3 MVP stub: resume playback not wired. Returns `None` (fresh start).
#[must_use]
pub fn load_checkpoint(_run_id: &str) -> Option<()> {
    None
}

/// Q3 MVP stub: best-effort persistence not wired.
pub fn save_checkpoint(_run_id: &str) {}

/// Q3 MVP stub: no runs enumerated.
#[must_use]
pub fn list_runs() -> Vec<()> {
    vec![]
}

/// Q3 MVP stub: no checkpoints reclaimed.
#[must_use]
pub fn gc_stale_checkpoints(_max_age_days: u64) -> u64 {
    0
}

/// Q3 MVP stub: no-op clear.
pub fn clear_checkpoint(_run_id: &str) {}

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
}
