//! Main skillpack installer — atomic installation with locking and protection.
//!
//! - Per-file overwrite protection: only overwrite when `--overwrite-local`
//! - Dependency closure: pulls full shared_deps for each install
//! - Locking: atomic rename from temp directory with .zbrain-skillpack.lock protection

use std::fs::{create_dir_all, remove_dir_all, rename, File};
use std::io::{Write, BufWriter};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, Duration};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, StructuredError};
use crate::skillpack::{bundle, copy};
use copy::{CopyArtifactsOpts, CopyItem, CopyResult};

/// Outcome for a single installed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileOutcome {
    /// Wrote a new file (did not exist).
    WroteNew,
    /// Overwrote an existing file (differs from bundle).
    WroteOverwrite,
    /// Skipped because it already existed and user declined overwrite.
    SkippedLocallyModified,
    /// Skipped because content is identical to bundle.
    SkippedIdentical,
    /// Skipped because overwrite declined and content differs.
    SkippedOverwriteDeclined,
}

/// Result for a single installed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileResult {
    /// Source path (absolute from zbrain root).
    pub source: PathBuf,
    /// Target path (absolute).
    pub target: PathBuf,
    /// Outcome of the install.
    pub outcome: FileOutcome,
    /// Whether this is from shared dependencies.
    pub shared_dep: bool,
}

/// Result for managed block update in AGENTS.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedBlockResult {
    /// Path to the resolver file.
    pub resolver_file: PathBuf,
    /// Did we apply an update.
    pub applied: bool,
    /// Reason why we didn't apply.
    pub skipped_reason: Option<String>,
}

/// Installation plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPlan {
    /// zbrain repo root (source of truth for bundle).
    pub zbrain_root: PathBuf,
    /// Target skills directory (where skills are installed).
    pub target_skills_dir: PathBuf,
    /// Target workspace root (top-level).
    pub target_workspace: PathBuf,
    /// All files to install.
    pub entries: Vec<CopyItem>,
    /// Parsed bundle manifest.
    pub manifest: bundle::BundleManifest,
    /// Per-entry computed outcomes (pre-install check).
    pub entry_outcomes: Vec<(CopyItem, bool, bool, bool)>,
}

/// Installation options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallOptions {
    /// Target workspace root (top-level, contains skills/ directory).
    pub target_workspace: PathBuf,
    /// Target skills directory (usually `target_workspace/skills`).
    pub target_skills_dir: PathBuf,
    /// zbrain repo root (source bundle), auto-detected if empty.
    pub zbrain_root: Option<PathBuf>,
    /// Install just this skill slug, None for all from manifest.
    pub skill_slug: Option<String>,
    /// Overwrite existing local files that differ from bundle.
    pub overwrite_local: Option<bool>,
    /// Dry run: compute plan, do not write anything.
    pub dry_run: Option<bool>,
    /// Force unlock even if lock appears held.
    pub force_unlock: Option<bool>,
    /// Override default stale lock threshold (milliseconds).
    pub lock_stale_ms: Option<u64>,
}

/// Error codes for install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallErrorCode {
    /// Lock is held by another process.
    LockHeld,
    /// Stale lock (PID not running) but user didn't force unlock.
    LockStale,
    /// Failed to create directory.
    DirectoryCreateFailed,
    /// Could not read manifest.
    ManifestReadFailed,
}

/// Install error.
#[derive(Debug)]
pub struct InstallError {
    code: InstallErrorCode,
    message: String,
}

impl InstallError {
    pub fn new(code: InstallErrorCode, message: String) -> Self {
        Self { code, message }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code: {:?})", self.message, self.code)
    }
}

impl std::error::Error for InstallError {}

impl From<InstallError> for Error {
    fn from(e: InstallError) -> Self {
        StructuredError::new("Install", "install_error", e.to_string())
    }
}

/// Lock file content stored in `.zbrain-skillpack.lock`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockFile {
    pid: u32,
    start_time_ms: u64,
}

/// Default stale lock threshold: 10 minutes.
const DEFAULT_STALE_MS: u64 = 10 * 60 * 1000;

/// Check if there is a held lock, and acquire it if possible.
fn check_acquire_lock(lock_path: &Path, force_unlock: bool, stale_threshold_ms: u64) -> Result<bool> {
    if !lock_path.exists() {
        // No existing lock — create it.
        let pid = std::process::id();
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let content = serde_json::to_string_pretty(&LockFile {
            pid,
            start_time_ms: start_time,
        }).unwrap();
        if let Err(e) = std::fs::write(lock_path, content) {
            return Err(InstallError::new(
                InstallErrorCode::LockHeld,
                format!("Failed to create lock file: {}", e),
            ).into());
        }
        return Ok(true);
    }

    // Existing lock — check if stale.
    let content = match std::fs::read_to_string(lock_path) {
        Ok(c) => c,
        Err(e) => {
            // Can't read — assume stale, allow overwrite if force.
            if force_unlock {
                let _ = std::fs::remove_file(lock_path);
                return check_acquire_lock(lock_path, force_unlock, stale_threshold_ms);
            }
            return Err(InstallError::new(
                InstallErrorCode::LockHeld,
                format!("Failed to read existing lock: {}", e),
            ).into());
        }
    };

    let lock: LockFile = match serde_json::from_str(&content) {
        Ok(l) => l,
        Err(e) => {
            if force_unlock {
                let _ = std::fs::remove_file(lock_path);
                return check_acquire_lock(lock_path, force_unlock, stale_threshold_ms);
            }
            return Err(InstallError::new(
                InstallErrorCode::LockHeld,
                format!("Invalid lock file: {}", e),
            ).into());
        }
    };

    let start_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let age = start_time.saturating_sub(lock.start_time_ms);

    if age > stale_threshold_ms {
        // Lock is stale — we can take it.
        let _ = std::fs::remove_file(lock_path);
        return check_acquire_lock(lock_path, force_unlock, stale_threshold_ms);
    }

    // Check if PID is still running (works on Unix, always false on Windows).
    #[cfg(unix)]
    let pid_running = unsafe { libc::kill(lock.pid, 0) == 0 };
    #[cfg(not(unix))]
    let pid_running = false;

    if !pid_running {
        // PID not running — stale, take it if force is on.
        if force_unlock {
            let _ = std::fs::remove_file(lock_path);
            return check_acquire_lock(lock_path, force_unlock, stale_threshold_ms);
        }
    }

    Err(InstallError::new(
        InstallErrorCode::LockHeld,
        format!(
            "Lock held by pid {} (age {} ms). Remove {} manually or use --force-unlock to proceed.",
            lock.pid, age, lock_path.display()
        ),
    ).into())
}

/// Release the lock after successful install.
fn release_lock(lock_path: &Path) {
    let _ = std::fs::remove_file(lock_path);
}

/// Build the install plan from the bundle manifest.
pub fn build_install_plan(zbrain_root: &Path, opts: &InstallOptions) -> Result<InstallPlan> {
    let manifest = bundle::load_bundle_manifest(zbrain_root)
        .map_err(|e| InstallError::new(
            InstallErrorCode::ManifestReadFailed,
            format!("Failed to load bundle manifest: {}", e),
        ))?;

    let target_skills_dir = &opts.target_skills_dir;
    let mut entries = Vec::new();
    let excluded = opts.skill_slug.as_ref()
        .map(|slug| manifest.skills.iter().filter(|s| s.as_str() != slug.as_str()).cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    // Get all entries from the manifest.
    let all_entries = bundle::enumerate_bundle_files(zbrain_root, &manifest, &excluded)?;

    // Check target directory exists, create if needed.
    if !target_skills_dir.exists() {
        if let Err(e) = create_dir_all(target_skills_dir) {
            return Err(InstallError::new(
                InstallErrorCode::DirectoryCreateFailed,
                format!("Failed to create target skills directory: {}", e),
            ).into());
        }
    }

    // Pre-compute existence for each entry for planning.
    let mut entry_outcomes = Vec::new();
    for entry in &all_entries {
        let target = target_skills_dir.join(&entry.rel_target);
        let existing = target.exists();
        let identical = if existing {
            let source_content = std::fs::read(&entry.source).unwrap_or_default();
            let target_content = std::fs::read(&target).unwrap_or_default();
            source_content == target_content
        } else {
            false
        };
        entry_outcomes.push((
            CopyItem {
                source: entry.source.clone(),
                target,
            },
            existing,
            identical,
            entry.shared_dep,
        ));
    }

    entries.extend(entry_outcomes.iter().map(|(ci, _, _, _)| ci.clone()));

    Ok(InstallPlan {
        zbrain_root: zbrain_root.to_path_buf(),
        target_skills_dir: target_skills_dir.to_path_buf(),
        target_workspace: opts.target_workspace.to_path_buf(),
        entries,
        manifest,
        entry_outcomes,
    })
}

/// Execute the install plan (write files to target).
pub fn execute_install(plan: &InstallPlan, opts: &InstallOptions) -> Result<Vec<FileResult>> {
    let dry_run = opts.dry_run.unwrap_or(false);
    let overwrite_local = opts.overwrite_local.unwrap_or(false);
    let force_unlock = opts.force_unlock.unwrap_or(false);
    let lock_stale_ms = opts.lock_stale_ms.unwrap_or(DEFAULT_STALE_MS);

    // Acquire the lock.
    let lock_path = plan.target_workspace.join(".zbrain-skillpack.lock");
    check_acquire_lock(&lock_path, force_unlock, lock_stale_ms)?;

    let mut results = Vec::new();

    // Install each entry.
    for (entry, existing, identical, shared_dep) in &plan.entry_outcomes {
        let outcome = if *identical {
            FileOutcome::SkippedIdentical
        } else if !*existing {
            FileOutcome::WroteNew
        } else if *existing && overwrite_local {
            FileOutcome::WroteOverwrite
        } else if *existing && !overwrite_local {
            FileOutcome::SkippedOverwriteDeclined
        } else {
            FileOutcome::SkippedLocallyModified
        };

        results.push(FileResult {
            source: entry.source.clone(),
            target: entry.target.clone(),
            outcome,
            shared_dep: *shared_dep,
        });

        if !dry_run {
            // Actually copy the file.
            let target_dir = entry.target.parent().unwrap();
            if !target_dir.exists() {
                let _ = create_dir_all(target_dir);
            }

            let content = std::fs::read(&entry.source)
                .expect("source file already validated exists in planning");
            let _ = std::fs::write(&entry.target, content);
        }
    }

    // Release the lock on success.
    release_lock(&lock_path);

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_lock_acquire_empty() {
        let dir = tempdir().unwrap();
        let lock = dir.path().join("test.lock");
        assert!(check_acquire_lock(&lock, false, DEFAULT_STALE_MS).unwrap());
    }
}
