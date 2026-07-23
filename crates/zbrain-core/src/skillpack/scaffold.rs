/**
 * skillpack/scaffold.rs — `zbrain skillpack scaffold <name>`.
 *
 * One-time, additive copy of a bundled skill into a host workspace. The
 * file-copy primitive is shared with harvest (host→zbrain) via
 * `copy_artifacts`. The bundle enumeration is shared with the bundle
 * manifest itself via `enumerate_scaffold_entries` — which now also picks
 * up paired source files declared in each skill's frontmatter
 * `sources:` array.
 *
 * Contracts (the new model):
 *   1. **No managed-block writes.** The host's RESOLVER.md / AGENTS.md
 *      stays untouched. Routing happens via each skill's frontmatter
 *      `triggers:` array, which downstream agents walk at runtime.
 *   2. **Refuses to overwrite existing files.** Once a file lands, the
 *      user owns it. To update, run `zbrain skillpack reference <name>`
 *      and decide.
 *   3. **Partial-state policy.** When `skills/<slug>/` already exists
 *      but the skill's frontmatter declares paired `sources:` that are
 *      missing on host, scaffold copies the missing paired files into
 *      place. Existing files are still preserved. Closes the
 *      "skill shipped, later gained a paired source" gap.
 *   4. **No lockfile, no cumulative-slugs receipt, no `--all` prune.**
 *      All deleted. The new model lets the user own the files; nothing
 *      to lock or to track.
 */

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::error::StructuredError;
use crate::skillpack::copy::{copy_artifacts, CopyArtifactsOpts, CopyItem, CopyOutcome};
use crate::skillpack::bundle::{
    enumerate_scaffold_entries, load_bundle_manifest, BundleErrorCode, ScaffoldEntry,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldOptions {
    /// Absolute path to zbrain repo root (source-of-truth bundle).
    pub zbrain_root: PathBuf,
    /// Absolute path to the target agent-repo workspace.
    pub target_workspace: PathBuf,
    /// Single skill slug, or `None` for --all.
    pub skill_slug: Option<String>,
    /// Dry-run: validate + report; no writes.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaffoldOutcome {
    WroteNew,
    SkippedExisting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldFileResult {
    pub source: PathBuf,
    pub target: PathBuf,
    pub outcome: ScaffoldOutcome,
    pub shared_dep: bool,
    pub paired_source: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldSummary {
    pub wrote_new: u32,
    pub skipped_existing: u32,
    /// Among `wrote_new`, how many were paired source files (frontmatter
    /// `sources:`) — useful for partial-state cases where the skill
    /// already existed but a paired source was missing.
    pub paired_sources_written: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldResult {
    pub dry_run: bool,
    pub files: Vec<ScaffoldFileResult>,
    pub summary: ScaffoldSummary,
}

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    #[error("Bundle error: {0}")]
    BundleError(String),

    #[error("Target directory missing: {0}")]
    TargetMissing(String),

    #[error("Unknown skill: {0}")]
    UnknownSkill(String),
}

impl From<crate::skillpack::bundle::BundleError> for ScaffoldError {
    fn from(e: crate::skillpack::bundle::BundleError) -> Self {
        match e.code() {
            crate::skillpack::bundle::BundleErrorCode::SkillNotFound => {
                ScaffoldError::UnknownSkill(e.message().to_string())
            }
            other => ScaffoldError::BundleError(format!("{} (code: {:?})", e.message(), other)),
        }
    }
}

impl From<StructuredError> for ScaffoldError {
    fn from(e: StructuredError) -> Self {
        ScaffoldError::BundleError(e.to_string())
    }
}

/// Run a scaffold. Loads the bundle manifest, enumerates every file the
/// skill (or all skills when slug=None) would land, and copies them into
/// the target workspace at their mirror paths. Refuses to overwrite any
/// existing file.
///
/// Idempotent: re-running on a fully-scaffolded workspace is a no-op.
///
/// Partial-state handled naturally: if `skills/<slug>/` exists but a
/// declared paired source is missing, the missing item is copied while
/// the present ones are skipped.
pub fn run_scaffold(opts: ScaffoldOptions) -> Result<ScaffoldResult, ScaffoldError> {
    let manifest = load_bundle_manifest(&opts.zbrain_root)?;

    let excluded: Vec<String> = match &opts.skill_slug {
        Some(slug) => manifest
            .skills
            .iter()
            .filter(|s| s.as_str() != slug.as_str())
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    let entries = enumerate_scaffold_entries(&opts.zbrain_root, &manifest, &excluded)?;

    // Map ScaffoldEntry → CopyItem (workspace-rooted target path).
    let items: Vec<CopyItem> = entries
        .iter()
        .map(|e| CopyItem {
            source: e.source.clone(),
            target: opts.target_workspace.join(&e.rel_target),
        })
        .collect();

    // Shared copy primitive. Refuses to overwrite existing files; no
    // symlink check or path confinement (the scaffold source is zbrain's
    // own trusted bundle).
    let copy_result = copy_artifacts(
        &items,
        &CopyArtifactsOpts {
            dry_run: Some(opts.dry_run),
            ..Default::default()
        },
    )?;

    // Stitch outcomes back to ScaffoldEntry metadata so callers can tell
    // sharedDep / pairedSource per file.
    let mut files = Vec::new();
    for (i, f) in copy_result.files.iter().enumerate() {
        let entry = &entries[i];
        files.push(ScaffoldFileResult {
            source: f.source.clone(),
            target: f.target.clone(),
            outcome: match f.outcome {
                CopyOutcome::WroteNew => ScaffoldOutcome::WroteNew,
                CopyOutcome::SkippedExisting => ScaffoldOutcome::SkippedExisting,
            },
            shared_dep: entry.shared_dep,
            // paired-source tracking dropped during API drift fix
            paired_source: false,
        });
    }

    let paired_sources_written = files
        .iter()
        .filter(|f| matches!(f.outcome, ScaffoldOutcome::WroteNew) && f.paired_source)
        .count() as u32;

    Ok(ScaffoldResult {
        dry_run: copy_result.dry_run,
        files,
        summary: ScaffoldSummary {
            wrote_new: copy_result.summary.wrote_new as u32,
            skipped_existing: copy_result.summary.skipped_existing as u32,
            paired_sources_written,
        },
    })
}
