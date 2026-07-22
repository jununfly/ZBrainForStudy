//! Thin git client for sync_brain (roadmap 1-6-7-13).
//!
//! Shells out to the system `git` CLI via `tokio::process::Command`, mirroring
//! the TS `sync.ts` `execFileSync('git', …)` approach. No libgit2 dependency
//! (per grill decision Q2). Happy-path commands only; divergence / missing
//! origin / detached-HEAD are surfaced as non-fatal so callers can fall back
//! to syncing from the local working tree.

use std::path::Path;
use tokio::process::Command;

/// Error from a git invocation.
#[derive(Debug)]
pub enum GitError {
    /// I/O failure spawning git (e.g. git not on PATH).
    Spawn(std::io::Error),
    /// git exited non-zero.
    NonZero {
        cmd: String,
        code: i32,
        stderr: String,
    },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Spawn(e) => write!(f, "failed to spawn git: {e}"),
            GitError::NonZero { cmd, code, stderr } => {
                write!(f, "git {cmd} failed (exit {code}): {stderr}")
            }
        }
    }
}

impl From<std::io::Error> for GitError {
    fn from(e: std::io::Error) -> Self {
        GitError::Spawn(e)
    }
}

impl GitError {
    /// True when the failure is a non-fast-forward / diverged pull that should
    /// fall back to syncing from the local working tree (non-fatal).
    pub fn is_divergence(&self) -> bool {
        match self {
            GitError::NonZero { stderr, .. } => {
                stderr.contains("non-fast-forward")
                    || stderr.contains("diverged")
                    || stderr.contains("rejected")
            }
            _ => false,
        }
    }
}

/// Stateless git client. All methods take the repo path explicitly.
pub struct GitClient;

impl GitClient {
    /// Run `git -C <repo> <args>`; returns trimmed stdout. Errors on non-zero.
    pub async fn run(repo: &Path, args: &[&str]) -> Result<String, GitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .await?;
        if !output.status.success() {
            return Err(GitError::NonZero {
                cmd: args.join(" "),
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Whether `repo` is a git repository (has a `.git` entry).
    pub fn is_repo(repo: &Path) -> bool {
        repo.join(".git").exists()
    }

    /// `git rev-parse HEAD` — current commit SHA.
    pub async fn head_commit(repo: &Path) -> Result<String, GitError> {
        Self::run(repo, &["rev-parse", "HEAD"]).await
    }

    /// `git rev-parse --abbrev-ref HEAD` — `"HEAD"` when detached.
    pub async fn current_branch(repo: &Path) -> Result<String, GitError> {
        Self::run(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await
    }

    /// Whether an `origin` remote is configured.
    pub async fn has_origin(repo: &Path) -> bool {
        Self::run(repo, &["remote", "get-url", "origin"]).await.is_ok()
    }

    /// Happy-path pull: `git pull --ff-only`. Divergence is non-fatal.
    pub async fn pull(repo: &Path) -> Result<(), GitError> {
        Self::run(repo, &["pull", "--ff-only"]).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergence_detection_keys_off_stderr() {
        let err = GitError::NonZero {
            cmd: "pull --ff-only".to_string(),
            code: 1,
            stderr: "fatal: rejecting non-fast-forward\n".to_string(),
        };
        assert!(err.is_divergence());

        let unrelated = GitError::NonZero {
            cmd: "rev-parse HEAD".to_string(),
            code: 128,
            stderr: "fatal: not a git repository".to_string(),
        };
        assert!(!unrelated.is_divergence());
    }

    #[test]
    fn is_repo_false_for_plain_dir() {
        let tmp = std::env::temp_dir().join("zbrain_git_is_repo_test");
        let _ = std::fs::create_dir_all(&tmp);
        assert!(!GitClient::is_repo(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
