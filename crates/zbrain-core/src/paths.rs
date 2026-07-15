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
/// 1. `ZBRAIN_HOME` (explicit cross-platform override)
/// 2. `$HOME` (Unix)
/// 3. `%USERPROFILE%` (Windows)
#[must_use]
pub fn home_root() -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
