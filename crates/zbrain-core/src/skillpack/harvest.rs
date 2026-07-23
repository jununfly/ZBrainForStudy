/**
 * skillpack/harvest.rs — `zbrain skillpack harvest <slug> --from <host-repo-root>`.
 *
 * Inverse of scaffold: lifts a skill from a host agent repo into
 * zbrain's tree so other clients can scaffold it via the normal path.
 *
 * Source contract (D11): `--from` points at the host repo root.
 * `<from>/skills/<slug>/` is the skill dir. Paired source files
 * declared in the host skill's frontmatter `sources:` array land at
 * the mirror path inside zbrain.
 *
 * Security (D13): every harvested file goes through canonical-path
 * validation and symlink rejection. `realpath(file).startsWith(realpath(host-skill-dir))`.
 * Mirrors `validateUploadPath` from `src/core/operations.ts`. Without this gate,
 * a malicious or careless symlink could leak secrets into zbrain's source tree.
 *
 * Privacy (D4, T7): after copying but before declaring success, the
 * harvested files are scanned against a regex allowlist of "private
 * patterns" (defaults + user-maintained `~/.zbrain/harvest-private-patterns.txt`).
 * Any match → rollback (delete harvested files) and exit non-zero.
 * `--no-lint` bypasses the linter (used by the editorial workflow
 * skill after a manual scrub).
 */

use std::fs;
use std::path::{Path, PathBuf};
use std::io::ErrorKind;
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use crate::skillpack::copy::{copy_artifacts, CopyArtifactsOpts, CopyItem};
use crate::skillpack::bundle::{load_skill_sources, LoadedSkillSources};
use crate::skillpack::harvest_lint::{run_privacy_lint, PrivacyLintError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestOptions {
    /// Slug of the skill to harvest (e.g. "my-fork-skill").
    pub slug: String,
    /// Absolute path to the host agent repo root.
    pub host_repo_root: PathBuf,
    /// Absolute path to zbrain repo root (destination).
    pub zbrain_root: PathBuf,
    /// Skip the privacy linter.
    #[serde(default)]
    pub no_lint: bool,
    /// Dry-run: preview, no writes.
    #[serde(default)]
    pub dry_run: bool,
    /// Custom private-patterns file (defaults to ~/.zbrain/harvest-private-patterns.txt).
    pub private_patterns_path: Option<PathBuf>,
    /// Allow overwriting an existing zbrain/skills/<slug>/ tree.
    #[serde(default)]
    pub overwrite_local: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarvestStatus {
    Harvested,
    HostSkillMissing,
    SlugCollision,
    LintFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestResult {
    pub status: HarvestStatus,
    pub slug: String,
    pub host_skill_dir: PathBuf,
    /// Files written under zbrain/.
    pub files_copied: Vec<PathBuf>,
    /// Paired source files (from frontmatter) included.
    pub paired_sources: Vec<String>,
    /// Privacy-lint hits, when status === 'lint_failed'.
    pub lint_hits: Vec<String>,
    /// True when the manifest was updated (only on success, non-dry-run).
    pub manifest_updated: bool,
    pub dry_run: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HarvestError {
    #[error("Host skill not found: {0}. Pass --from <host-repo-root> pointing at a repo whose skills/<slug>/ exists.")]
    HostSkillMissing(PathBuf),

    #[error("Host skill frontmatter is malformed: {0}")]
    HostSkillMalformed(String),

    #[error("Slug collision: zbrain already has skills/{0}/. Pass --overwrite-local to replace.")]
    SlugCollision(String),

    #[error("Path traversal attempt rejected: {0}")]
    PathTraversal(String),

    #[error("Symlink rejected: {0}")]
    SymlinkRejected(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

const PLUGIN_JSON: &str = "openclaw.plugin.json";

/// Map a copy error (StructuredError) to a HarvestError, preserving the
/// symlink/path-traversal distinctions via the error message text.
/// `copy_artifacts` now returns `StructuredError` whose `code` is the generic
/// `"copy_error"`, so we inspect `message` to recover the specific cause.
fn to_harvest_err(e: crate::error::StructuredError) -> HarvestError {
    if e.message.contains("Symlink") {
        HarvestError::SymlinkRejected(e.message)
    } else if e.message.contains("Target path escapes") {
        HarvestError::PathTraversal(e.message)
    } else {
        HarvestError::Io(std::io::Error::new(ErrorKind::Other, e.to_string()))
    }
}

fn default_private_patterns_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".zbrain").join("harvest-private-patterns.txt"))
}

/// Run harvest: lifts a skill from a host repo into zbrain's tree.
pub fn run_harvest(opts: HarvestOptions) -> Result<HarvestResult, HarvestError> {
    let dry_run = opts.dry_run;
    let host_skill_dir = opts.host_repo_root.join("skills").join(&opts.slug);
    let host_skill_md = host_skill_dir.join("SKILL.md");

    if !host_skill_md.exists() {
        return Err(HarvestError::HostSkillMissing(host_skill_md));
    }

    let zbrain_skill_dir = opts.zbrain_root.join("skills").join(&opts.slug);
    if zbrain_skill_dir.exists() && !opts.overwrite_local {
        return Err(HarvestError::SlugCollision(opts.slug));
    }

    // Read frontmatter sources from the host SKILL.md. Reuse the bundler's
    // validation — but skip its existence check on the destination since
    // we're reading from a different root.
    let paired_sources = read_host_skill_sources(&opts.host_repo_root, &opts.slug)?;

    // Build items list:
    //   - skill dir → zbrain/skills/<slug>/
    //   - paired sources → zbrain/<source-path>
    let mut items: Vec<CopyItem> = Vec::new();
    for item in crate::skillpack::copy::walk_source_dir(&host_skill_dir, &zbrain_skill_dir) {
        items.push(item);
    }
    for src in &paired_sources {
        let source = opts.host_repo_root.join(src);
        let target = opts.zbrain_root.join(src);
        items.push(CopyItem { source, target });
    }

    // Copy with D13 confinement + symlink reject. The confinement root is
    // the HOST skill dir (every source must canonicalize inside it). For
    // paired sources outside the skill dir, fall through to symlink-only
    // protection (the host repo is user-trusted at this granularity).
    let files_copied: Vec<PathBuf> = items.iter().map(|i| i.target.clone()).collect();
    let (skill_items, paired_items): (Vec<&CopyItem>, Vec<&CopyItem>) =
        items.iter().partition(|i| i.source.starts_with(&host_skill_dir));

    let skill_vec: Vec<CopyItem> = skill_items.into_iter().cloned().collect();
    let paired_vec: Vec<CopyItem> = paired_items.into_iter().cloned().collect();

    let copy_skill_options = CopyArtifactsOpts {
        reject_symlinks: Some(true),
        confine_realpath_to: Some(host_skill_dir.clone()),
        dry_run: Some(dry_run),
    };
    let copy_paired_options = CopyArtifactsOpts {
        reject_symlinks: Some(true),
        confine_realpath_to: None,
        dry_run: Some(dry_run),
    };

    if !dry_run {
        if let Err(e) = copy_artifacts(&skill_vec, &copy_skill_options) {
            return Err(to_harvest_err(e));
        }
        if let Err(e) = copy_artifacts(&paired_vec, &copy_paired_options) {
            return Err(to_harvest_err(e));
        }
    } else {
        // Dry-run still validates safety gates but doesn't copy.
        if let Err(e) = copy_artifacts(&skill_vec, &copy_skill_options) {
            return Err(to_harvest_err(e));
        }
        if let Err(e) = copy_artifacts(&paired_vec, &copy_paired_options) {
            return Err(to_harvest_err(e));
        }
    }

    // Privacy lint AFTER copy (lint scans the harvested files). On match,
    // rollback (delete) and report.
    let mut lint_hits = Vec::new();
    if !opts.no_lint && !dry_run {
        let patterns_path = opts.private_patterns_path
            .or_else(default_private_patterns_path);
        match run_privacy_lint(&files_copied, patterns_path.as_deref()) {
            Err(PrivacyLintError::LintErrors(hits)) => {
                // Rollback: remove every file we just wrote.
                rollback_harvest(&zbrain_skill_dir, &files_copied);
                return Ok(HarvestResult {
                    status: HarvestStatus::LintFailed,
                    slug: opts.slug,
                    host_skill_dir,
                    files_copied: Vec::new(),
                    paired_sources,
                    lint_hits: hits,
                    manifest_updated: false,
                    dry_run: false,
                });
            }
            Err(e) => return Err(HarvestError::Io(std::io::Error::new(ErrorKind::Other, e.to_string()))),
            Ok(_) => {}
        }
    }

    // Update openclaw.plugin.json — add slug to "skills" array if missing.
    let mut manifest_updated = false;
    if !dry_run {
        manifest_updated = add_to_bundle_manifest(&opts.zbrain_root, &opts.slug);
    }

    Ok(HarvestResult {
        status: HarvestStatus::Harvested,
        slug: opts.slug,
        host_skill_dir,
        files_copied,
        paired_sources,
        lint_hits,
        manifest_updated,
        dry_run,
    })
}

/// Read a host skill's frontmatter `sources:` without using the bundler
/// (the bundler resolves paths against zbrainRoot, not the host). Mirrors
/// `loadSkillSources`'s validation but resolves against the host root.
fn read_host_skill_sources(host_root: &Path, slug: &str) -> Result<Vec<String>, HarvestError> {
    // Lean on bundle's load_skill_sources but pass the host as the root.
    // Its validation (no abs paths, no `..`, must exist) applies to the
    // host's tree, which is what we want.
    let result = load_skill_sources(host_root, &format!("skills/{slug}"))
        .map_err(|e| HarvestError::HostSkillMalformed(e.to_string()))?;
    Ok(result.sources)
}

/// Delete everything we just wrote.
fn rollback_harvest(zbrain_skill_dir: &Path, paired_targets: &[PathBuf]) {
    if zbrain_skill_dir.exists() {
        let _ = fs::remove_dir_all(zbrain_skill_dir);
    }
    for target in paired_targets {
        if target.exists() {
            let _ = fs::remove_file(target);
        }
    }
}

/// Add `slugs/<slug>` to `openclaw.plugin.json#skills` if missing.
/// Preserves JSON formatting via 2-space indent. Idempotent.
///
/// Returns true if the manifest was modified.
pub fn add_to_bundle_manifest(zbrain_root: &Path, slug: &str) -> bool {
    let manifest_path = zbrain_root.join(PLUGIN_JSON);
    if !manifest_path.exists() {
        return false;
    }

    let raw = match fs::read_to_string(&manifest_path) {
        Ok(r) => r,
        Err(_) => return false,
    };

    let mut manifest: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(_) => return false,
    };

    let Some(skills) = manifest.get_mut("skills") else {
        return false;
    };
    let Some(skills_arr) = skills.as_array_mut() else {
        return false;
    };

    let skill_rel = format!("skills/{slug}");
    if skills_arr.iter().any(|v| v.as_str() == Some(&skill_rel)) {
        return false;
    }

    skills_arr.push(serde_json::Value::String(skill_rel));
    skills_arr.sort_by_key(|v| v.as_str().unwrap_or_default().to_string());

    let pretty = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    let _ = fs::write(manifest_path, format!("{pretty}\n"));
    true
}
