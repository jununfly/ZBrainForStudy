//! Cross-platform `~/.zbrain` resolution — single source of truth.
//!
//! Honors the `ZBRAIN_HOME` environment variable as an explicit,
//! cross-platform override. Its value is treated as the home *root*, so the
//! zbrain home (`~/.zbrain`) lives at `<ZBRAIN_HOME>/.zbrain`. Falls back to
//! the OS home directory (`$HOME` on Unix, `%USERPROFILE%` on Windows).
//!
//! ## Why `ZBRAIN_HOME`?
//!
//! `dirs::home_dir()` (used historically) **ignores `$HOME`** on Windows — it
//! reads `%USERPROFILE%` instead. That means `export HOME=/tmp/x` does *not*
//! redirect `~/.zbrain` state for the Rust binary on Windows, so "isolated
//! HOME" test setups silently wrote into the real brain database. `ZBRAIN_HOME`
//! is honored on every platform and is therefore the recommended isolation
//! mechanism for tests and isolated runs.
//!
//! All `~/.zbrain` resolution in the codebase must go through this module
//! (via [`home_root`] / [`zbrain_home`]) so the entire binary resolves state
//! identically.

use std::path::PathBuf;

/// Resolve the home *root*, honoring `ZBRAIN_HOME`.
///
/// Resolution order:
/// 0. Thread-local test override (test builds only — see [`ScopedTestHome`])
/// 1. `ZBRAIN_HOME` (explicit cross-platform override)
/// 2. `$HOME` (Unix)
/// 3. `%USERPROFILE%` (Windows)
///
/// The thread-local override is how tests inject an isolated `~/.zbrain` root
/// **without mutating process-global environment variables**. Because cargo
/// runs each test on its own thread, a per-thread override gives every test a
/// private home with zero cross-test interference — no serialization required.
#[must_use]
pub fn home_root() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(p) = test_home::current() {
            return Some(p);
        }
    }
    if let Ok(v) = std::env::var("ZBRAIN_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

/// Get the default zbrain home directory (`~/.zbrain`), honoring `ZBRAIN_HOME`.
#[must_use]
pub fn zbrain_home() -> Option<PathBuf> {
    home_root().map(|home| home.join(".zbrain"))
}

// ---------------------------------------------------------------------------
// Thread-local test home injection (parallel-safe, no global env mutation)
// ---------------------------------------------------------------------------

/// Per-thread `~/.zbrain` root override for tests.
///
/// Historically, schema-pack tests isolated `~/.zbrain` by mutating the
/// process-global `HOME`/`USERPROFILE` env vars, which forced the whole suite
/// to run single-threaded (or behind a process-wide mutex) to avoid races.
/// A thread-local override removes that coupling entirely: each test injects
/// its own home path on its own thread via [`ScopedTestHome`], so parallel
/// tests never see each other's state.
#[cfg(test)]
pub(crate) mod test_home {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// Currently-active override for this thread, if any.
    pub(crate) fn current() -> Option<PathBuf> {
        OVERRIDE.with(|c| c.borrow().clone())
    }

    pub(super) fn set(path: PathBuf) {
        OVERRIDE.with(|c| *c.borrow_mut() = Some(path));
    }

    pub(super) fn clear() {
        OVERRIDE.with(|c| *c.borrow_mut() = None);
    }
}

/// RAII guard that routes all `~/.zbrain` resolution on the current thread to a
/// unique temp directory, then tears it down on drop.
///
/// Use this in place of the old `HOME`-mutating test helpers: it injects the
/// home path (per-thread) rather than clobbering shared process-global env, so
/// tests run fully in parallel with no serialization.
///
/// ```ignore
/// let home = crate::paths::ScopedTestHome::new();
/// // ...code under test resolves crate::paths::zbrain_home() == home.zbrain_dir()
/// // dir is removed automatically when `home` drops.
/// ```
#[cfg(test)]
pub(crate) struct ScopedTestHome {
    root: PathBuf,
}

#[cfg(test)]
impl ScopedTestHome {
    /// Create a unique temp home root and activate it for this thread.
    pub(crate) fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "zbrain-test-home-{}-{:?}-{}",
            std::process::id(),
            std::thread::current().id(),
            n
        ));
        // Best-effort clean slate in case a previous run left the dir behind.
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scoped test home root");
        test_home::set(root.clone());
        Self { root }
    }

    /// The home *root* (the directory that contains `.zbrain`).
    #[allow(dead_code)]
    pub(crate) fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// The `~/.zbrain` directory under this scoped home.
    #[allow(dead_code)]
    pub(crate) fn zbrain_dir(&self) -> PathBuf {
        self.root.join(".zbrain")
    }
}

#[cfg(test)]
impl Drop for ScopedTestHome {
    fn drop(&mut self) {
        test_home::clear();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_test_home_overrides_resolution() {
        let home = ScopedTestHome::new();
        assert_eq!(home_root(), Some(home.root().to_path_buf()));
        assert_eq!(zbrain_home(), Some(home.zbrain_dir()));
    }

    #[test]
    fn scoped_test_home_clears_on_drop() {
        let root;
        {
            let home = ScopedTestHome::new();
            root = home.root().to_path_buf();
            assert_eq!(home_root(), Some(root.clone()));
        }
        // After drop, the override is gone (falls back to env) and the dir is removed.
        assert_ne!(home_root(), Some(root.clone()));
        assert!(!root.exists());
    }

    // NOTE: we deliberately do NOT mutate global env vars in these tests.
    // `home_root`/`zbrain_home` read process-global environment, and mutating
    // it races with other tests in this binary (schema-pack tests assume a
    // HOME-based `~/.zbrain`). The schema-pack suite is run single-threaded,
    // but to keep this module race-free under any test configuration we only
    // assert behavior when `ZBRAIN_HOME` is *already* present in the
    // environment (as CI isolation does), never set/remove it ourselves.

    #[test]
    fn zbrain_home_honors_zbrain_home_when_set() {
        if let Ok(v) = std::env::var("ZBRAIN_HOME") {
            if !v.is_empty() {
                assert_eq!(home_root(), Some(PathBuf::from(&v)));
                assert_eq!(zbrain_home(), Some(PathBuf::from(&v).join(".zbrain")));
            }
        }
    }
}
