//! Retrieval-drift watch for the `eval_drift` doctor check.
//!
//! Mirrors TS `src/core/eval/drift-watch.ts`: a curated allowlist of
//! repo-relative paths whose change MEANINGFULLY affects retrieval quality.
//! The doctor check warns when any watched file has drifted in the working
//! tree since the given commit (or HEAD). Best-effort: if git is unavailable
//! or any step fails, we report no drift (fail-open) rather than poison the
//! doctor run.

use std::path::Path;
use std::process::Command;

/// Curated paths watched for retrieval drift. Each entry is matched against
/// repo-relative paths via prefix (trailing `/`) or exact (bare file) semantics.
/// Adding to this list REQUIRES a changelog line (see TS history [CDX-6]).
pub const RETRIEVAL_WATCH_PATTERNS: &[&str] = &[
    // Search pipeline core
    "src/core/search/",
    // Embedding shape (changing dim or chunker shape moves every result)
    "src/core/embedding.ts",
    // Chunkers (recursive + semantic + LLM-guided) — chunk granularity is retrieval
    "src/core/chunkers/",
    // AI recipes that drive expansion / embedding choices
    "src/core/ai/recipes/anthropic.ts",
    "src/core/ai/recipes/openai.ts",
    // The query op itself
    "src/core/operations.ts",
];

/// Path equality / prefix matcher for the curated list.
pub fn matches_watch_pattern(path: &str, patterns: &[&str]) -> bool {
    for p in patterns {
        if p.ends_with('/') {
            if path.starts_with(p) {
                return true;
            }
        } else if path == *p {
            return true;
        }
    }
    false
}

/// Return repo-relative paths changed in the working tree since `commit_sha`
/// (or HEAD if `None`). Best-effort: returns empty on any failure (missing
/// repo, git unavailable, timeout, non-zero exit).
pub fn files_drifted_since(repo_root: &Path, commit_sha: Option<&str>) -> Vec<String> {
    if !repo_root.exists() {
        return Vec::new();
    }
    let range = commit_sha
        .map(|c| format!("{c}..HEAD"))
        .unwrap_or_else(|| "HEAD".to_string());
    let output = Command::new("git")
        .arg("diff")
        .arg("--name-only")
        .arg(&range)
        .current_dir(repo_root)
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Only the changed files that match the retrieval watch list.
pub fn watched_files_drifted(
    repo_root: &Path,
    commit_sha: Option<&str>,
    patterns: &[&str],
) -> Vec<String> {
    files_drifted_since(repo_root, commit_sha)
        .into_iter()
        .filter(|p| matches_watch_pattern(p, patterns))
        .collect()
}

/// Status of the `eval_drift` doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalDriftStatus {
    Ok,
    Warn,
}

/// Compute the `eval_drift` check result against `repo_root`.
/// `commit_sha` is the last-eval commit; `None` compares HEAD against the
/// working tree (uncommitted changes). Returns `(status, human message)`.
pub fn eval_drift_status(repo_root: &Path, commit_sha: Option<&str>) -> (EvalDriftStatus, String) {
    let drifted = watched_files_drifted(repo_root, commit_sha, RETRIEVAL_WATCH_PATTERNS);
    if drifted.is_empty() {
        (
            EvalDriftStatus::Ok,
            "No retrieval-path drift detected since last eval".to_string(),
        )
    } else {
        let shown: Vec<String> = drifted.iter().take(10).cloned().collect();
        let more = if drifted.len() > 10 {
            format!(" (and {} more)", drifted.len() - 10)
        } else {
            String::new()
        };
        (
            EvalDriftStatus::Warn,
            format!(
                "Retrieval-path drift since last eval: {}{}",
                shown.join(", "),
                more
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_watch_pattern_prefix_and_exact() {
        assert!(matches_watch_pattern(
            "src/core/search/foo.ts",
            RETRIEVAL_WATCH_PATTERNS
        ));
        assert!(matches_watch_pattern(
            "src/core/embedding.ts",
            RETRIEVAL_WATCH_PATTERNS
        ));
        // directory prefix must not match a sibling file
        assert!(!matches_watch_pattern(
            "src/core/embedding.rs",
            RETRIEVAL_WATCH_PATTERNS
        ));
        // non-watched path
        assert!(!matches_watch_pattern(
            "src/core/foo.ts",
            RETRIEVAL_WATCH_PATTERNS
        ));
        // bare-file exact match only (no suffix leak)
        assert!(!matches_watch_pattern(
            "src/core/embedding.ts.bak",
            RETRIEVAL_WATCH_PATTERNS
        ));
    }

    #[test]
    fn files_drifted_since_missing_root_is_empty() {
        let root = Path::new("/nonexistent/path/that/does/not/exist/zbrain");
        assert!(files_drifted_since(root, None).is_empty());
    }

    #[test]
    fn eval_drift_status_clean_when_no_drift() {
        let root = Path::new("/nonexistent/path/that/does/not/exist/zbrain");
        let (status, _msg) = eval_drift_status(root, None);
        assert_eq!(status, EvalDriftStatus::Ok);
    }
}
