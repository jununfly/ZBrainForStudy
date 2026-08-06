//! Per-page in-process lock for atomic markdown read-modify-write.
//!
//! Minimal Rust port of `src/core/page-lock.ts`. Serializes fence writes to
//! the same `<slug>.md` within a single process so two concurrent forget (or
//! future fence-write) operations on the same page can't clobber each other's
//! `.tmp` + rename.
//!
//! NOTE: the TS version also does cross-process PID-liveness recovery via a
//! lock file under `~/.zbrain/page-locks`. That crash-safety is intentionally
//! omitted here — the atomic `.tmp` + rename performed by `facts_fence` already
//! guarantees the canonical file is never left half-written, and the real
//! `zbrain rebuild` reconciles DB state from the fence.

use std::collections::HashMap;
use std::sync::Arc;

use lazy_static::lazy_static;
use tokio::sync::Mutex;

lazy_static! {
    static ref PAGE_LOCKS: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> =
        std::sync::Mutex::new(HashMap::new());
}

/// Return the per-slug lock handle, creating it on first use.
///
/// Callers acquire the lock inline with `lock.lock().await` and hold the
/// returned `MutexGuard` across their read-modify-write. Holding the guard
/// across `.await` points is safe (`MutexGuard` is `Send`), and serializes
/// fence writes to the same `<slug>.md` within one process. The guard is
/// released when dropped (end of scope / early return), including on panic or
/// cancellation.
///
/// NOTE: this returns the `Arc` rather than taking a closure so it can be used
/// from inside another `async fn` without forcing the caller's future to carry
/// a local lifetime (which the borrow checker rejects as an escape).
pub fn page_lock_for(slug: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = PAGE_LOCKS
        .lock()
        .expect("page-lock registry poisoned");
    map.entry(slug.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

