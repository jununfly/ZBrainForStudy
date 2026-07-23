//! skillpack reference generation — compare bundle vs local and output diffs.
//!
//! Generates per-file unified diffs so the agent can decide which changes to integrate.
//! Does not apply changes automatically — that's for `apply-clean-hunks`.

use std::fs::{read, metadata};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::skillpack::{bundle, diff_text};
use diff_text::unified_diff;

/// Options for the reference command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceOptions {
    /// Absolute path to the zbrain repo root (source of truth bundle).
    pub zbrain_root: PathBuf,
    /// Absolute path to the target workspace (local copy).
    pub target_workspace: PathBuf,
    /// Single skill slug, None for --all (summary per skill).
    pub skill_slug: Option<String>,
}

/// Status of a file compared between bundle and local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceStatus {
    /// Files are identical.
    Identical,
    /// Files differ.
    Differs,
    /// File is in bundle but missing locally.
    Missing,
}

/// Result for a single file comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceFileResult {
    /// Source path (absolute from zbrain root).
    pub source: PathBuf,
    /// Target path (absolute from target root).
    pub target: PathBuf,
    /// Comparison status.
    pub status: ReferenceStatus,
    /// Whether this is from shared-deps.
    pub shared_dep: bool,
    /// Always false for shared deps (they are already paired by definition).
    pub paired_source: bool,
    /// Unified diff when status is differs.
    pub unified_diff: String,
    /// Source file size in bytes.
    pub source_bytes: u64,
    /// Target file size in bytes.
    pub target_bytes: u64,
}

/// Overall reference result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceResult {
    /// Framing line explaining to the agent what this is.
    pub framing: String,
    /// Per-file comparison results.
    pub files: Vec<ReferenceFileResult>,
    /// Summary counts.
    pub summary: ReferenceSummary,
}

/// Summary counts for the reference command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReferenceSummary {
    pub identical: usize,
    pub differs: usize,
    pub missing: usize,
}

/// Per-skill summary for the all-skill case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillReferenceSummary {
    pub slug: String,
    pub summary: ReferenceSummary,
}

/// Overall result for the all-skill case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceAllResult {
    pub framing: String,
    pub skills: Vec<SkillReferenceSummary>,
}

/// Get the framing message explaining what the reference means.
pub fn framing_message(ref_path: &Path) -> String {
    format!(
        "These files live in {} as reference. Read them and decide what (if anything) to integrate into your local skills/. Your local edits are intentional — do not blindly overwrite.",
        ref_path.display()
    )
}

/// Compare a single file between the bundle and local target.
pub fn diff_one(target_dir: &Path, entry: &bundle::BundleEntry, shared_dep: bool) -> Result<ReferenceFileResult> {
    let source = &entry.source;
    let target = target_dir.join(&entry.rel_target);

    let source_meta = metadata(source).ok();
    let source_bytes = source_meta.as_ref().map(|m| m.len()).unwrap_or(0);

    if !target.exists() {
        return Ok(ReferenceFileResult {
            source: source.clone(),
            target: target,
            status: ReferenceStatus::Missing,
            shared_dep: shared_dep,
            paired_source: false,
            unified_diff: String::new(),
            source_bytes,
            target_bytes: 0,
        });
    }

    let target_meta = metadata(&target).unwrap();
    let target_bytes = target_meta.len();

    let source_content = read(source).unwrap_or_default();
    let target_content = read(&target).unwrap_or_default();

    let status = if source_content == target_content {
        ReferenceStatus::Identical
    } else {
        ReferenceStatus::Differs
    };

    let unified_diff = if status == ReferenceStatus::Differs {
        let source_str = String::from_utf8_lossy(&source_content);
        let target_str = String::from_utf8_lossy(&target_content);
        diff_text::unified_diff(
            &target_str,
            &source_str,
            diff_text::UnifiedDiffOpts::default(),
        )
    } else {
        String::new()
    };

    Ok(ReferenceFileResult {
        source: source.clone(),
        target,
        status,
        shared_dep: shared_dep,
        paired_source: false,
        unified_diff,
        source_bytes,
        target_bytes,
    })
}

/// Run reference on a single skill.
pub fn run_reference(opts: &ReferenceOptions) -> Result<ReferenceResult> {
    let zbrain_root = &opts.zbrain_root;
    let manifest = bundle::load_bundle_manifest(zbrain_root)?;
    let entries = bundle::enumerate_bundle_files(zbrain_root, &manifest, &[])?;

    let mut files = Vec::with_capacity(entries.len());
    for entry in entries {
        files.push(diff_one(&opts.target_workspace, &entry, entry.shared_dep)?);
    }

    let framing = framing_message(zbrain_root);

    let mut summary = ReferenceSummary::default();
    for f in &files {
        match f.status {
            ReferenceStatus::Identical => summary.identical += 1,
            ReferenceStatus::Differs => summary.differs += 1,
            ReferenceStatus::Missing => summary.missing += 1,
        }
    }

    Ok(ReferenceResult {
        framing,
        files,
        summary,
    })
}
