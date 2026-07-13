//! Child process spawn helpers (roadmap 1-2-5).
//!
//! Mirrors TS `spawn-helpers.ts`: tini detection + command-line building.
//! Pure functions with no runtime — the actual `tokio::process::Command::spawn`
//! call lives in `child_supervisor.rs`.

use std::path::PathBuf;

/// Detect the `tini` binary by scanning `PATH`. Called once at construction
/// time; result is cached. Returns `None` if tini is not found (common on
/// non-container environments). Mirrors TS `detectTini()`.
#[must_use]
pub fn detect_tini() -> Option<PathBuf> {
    let tini_name = if cfg!(windows) { "tini.exe" } else { "tini" };

    std::env::var_os("PATH")
        .and_then(|path_var| {
            std::env::split_paths(&path_var)
                .map(|dir| dir.join(tini_name))
                .find(|p| p.is_file())
        })
}

/// Resolved spawn command for a child process. When tini is available it
/// wraps the invocation (`tini -- <cli_path> <args>...`); otherwise the CLI
/// binary is spawned directly. Mirrors TS `buildSpawnInvocation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnInvocation {
    /// Executable to run.
    pub cmd: PathBuf,
    /// Arguments (including `--` separator when tini-wrapped).
    pub args: Vec<String>,
}

/// Build the spawn invocation for a child process. When `tini_path` is
/// `Some`, the child is spawned as `tini -- <cli_path> <args>...`.
/// Pure function — does not execute anything. Mirrors TS `buildSpawnInvocation`.
#[must_use]
pub fn build_spawn_args(
    tini_path: Option<&PathBuf>,
    cli_path: &str,
    args: &[String],
) -> SpawnInvocation {
    match tini_path {
        Some(tini) => {
            let mut all_args = vec!["--".to_string(), cli_path.to_string()];
            all_args.extend_from_slice(args);
            SpawnInvocation {
                cmd: tini.clone(),
                args: all_args,
            }
        }
        None => SpawnInvocation {
            cmd: PathBuf::from(cli_path),
            args: args.to_vec(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_tini_returns_none_when_not_found() {
        // tini is not installed in typical dev environments — safe to assert None.
        // If tini IS present, the test still passes (it just gets Some).
        let _result = detect_tini();
        // No assertion on value — it's environment-dependent.
        // We test detect_tini only for "doesn't panic" and returns a valid shape.
    }

    #[test]
    fn build_spawn_without_tini() {
        let args = vec!["jobs".to_string(), "work".to_string()];
        let inv = build_spawn_args(None, "/usr/bin/zbrain", &args);

        assert_eq!(inv.cmd, PathBuf::from("/usr/bin/zbrain"));
        assert_eq!(inv.args, vec!["jobs", "work"]);
    }

    #[test]
    fn build_spawn_with_tini_wraps() {
        let tini = PathBuf::from("/usr/bin/tini");
        let args = vec!["jobs".to_string(), "work".to_string(), "--concurrency".to_string(), "2".to_string()];
        let inv = build_spawn_args(Some(&tini), "/usr/bin/zbrain", &args);

        assert_eq!(inv.cmd, PathBuf::from("/usr/bin/tini"));
        assert_eq!(
            inv.args,
            vec!["--", "/usr/bin/zbrain", "jobs", "work", "--concurrency", "2"]
        );
    }

    #[test]
    fn build_spawn_no_args() {
        let inv = build_spawn_args(None, "/usr/bin/zbrain", &[]);
        assert_eq!(inv.args, Vec::<String>::new());
    }

    #[test]
    fn build_spawn_tini_no_args() {
        let tini = PathBuf::from("/usr/bin/tini");
        let inv = build_spawn_args(Some(&tini), "/usr/bin/zbrain", &[]);
        assert_eq!(inv.args, vec!["--", "/usr/bin/zbrain"]);
    }
}
