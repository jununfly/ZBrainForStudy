//! resolver-filenames — shared filename policy for the resolver file.
//!
//! zbrain-native convention: `RESOLVER.md`. OpenClaw convention: `AGENTS.md`.
//! Both are valid at the same path (skills dir or workspace root). When both
//! exist at a location, `RESOLVER.md` wins by policy.
//!
//! One source of truth. Imported by `repo_root` (auto-detect) and
//! `trigger_index` (parser + error messages). Never hardcode `RESOLVER.md`
//! in new code — import from here.

use std::path::{Path, PathBuf};

/// Ordered: first-match wins. Do not reorder without updating tests.
pub const RESOLVER_FILENAMES: [&str; 2] = ["RESOLVER.md", "AGENTS.md"];

/// Human-readable list for error messages, e.g. "RESOLVER.md or AGENTS.md".
pub const RESOLVER_FILENAMES_LABEL: &str = "RESOLVER.md or AGENTS.md";

/// Return the first existing resolver file in `dir`, or None.
/// Pass the directory — this function joins for you.
pub fn find_resolver_file(dir: &Path) -> Option<PathBuf> {
    for name in RESOLVER_FILENAMES.iter() {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Return ALL existing resolver files in `dir` (preserves both when an
/// OpenClaw deployment ships skills/RESOLVER.md AND a parent AGENTS.md).
pub fn find_all_resolver_files(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    for name in RESOLVER_FILENAMES.iter() {
        let candidate = dir.join(name);
        if candidate.exists() {
            results.push(candidate);
        }
    }
    results
}

/// True iff `dir` contains at least one recognized resolver file.
pub fn has_resolver_file(dir: &Path) -> bool {
    find_resolver_file(dir).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_rf_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_resolver_md_first() {
        let dir = scratch("first");
        fs::write(dir.join("RESOLVER.md"), "# r").unwrap();
        fs::write(dir.join("AGENTS.md"), "# a").unwrap();
        let found = find_resolver_file(&dir).unwrap();
        assert_eq!(found.file_name().unwrap(), "RESOLVER.md");
    }

    #[test]
    fn falls_back_to_agents_md() {
        let dir = scratch("fallback");
        fs::write(dir.join("AGENTS.md"), "# a").unwrap();
        let found = find_resolver_file(&dir).unwrap();
        assert_eq!(found.file_name().unwrap(), "AGENTS.md");
    }

    #[test]
    fn none_when_absent() {
        let dir = scratch("none");
        assert!(find_resolver_file(&dir).is_none());
        assert!(!has_resolver_file(&dir));
    }

    #[test]
    fn find_all_returns_both() {
        let dir = scratch("all");
        fs::write(dir.join("RESOLVER.md"), "# r").unwrap();
        fs::write(dir.join("AGENTS.md"), "# a").unwrap();
        let all = find_all_resolver_files(&dir);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn label_is_correct() {
        assert_eq!(RESOLVER_FILENAMES_LABEL, "RESOLVER.md or AGENTS.md");
    }
}
