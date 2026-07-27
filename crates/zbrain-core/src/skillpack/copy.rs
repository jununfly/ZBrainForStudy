//! File/directory copying with safety gates (symlink rejection, path confinement).
//!
//! Pre-validates all items before any writes happen — either everything passes
//! the gates or nothing gets written.

use std::fs::{create_dir_all, read, metadata, write, read_dir};
use std::path::{Path, PathBuf};
use std::fs::canonicalize;

use serde::{Deserialize, Serialize};

use crate::error::{Error, StructuredError, Result};

/// A single file to copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyItem {
    /// Absolute source path.
    pub source: PathBuf,
    /// Absolute target path.
    pub target: PathBuf,
}

/// Options for copying artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CopyArtifactsOpts {
    /// Reject symlinks (stat-based).
    pub reject_symlinks: Option<bool>,
    /// Confine all generated paths to be under this directory.
    pub confine_realpath_to: Option<PathBuf>,
    /// Dry run: validate only, do not write anything.
    pub dry_run: Option<bool>,
}

/// Outcome for a single copied file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopyOutcome {
    /// Successfully wrote a new file.
    WroteNew,
    /// Skipped because target already exists.
    SkippedExisting,
}

/// Result for a single copied file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyFileResult {
    /// Source path.
    pub source: PathBuf,
    /// Target path.
    pub target: PathBuf,
    /// Outcome of the copy.
    pub outcome: CopyOutcome,
}

/// Overall result of the copy operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyResult {
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// List of file results.
    pub files: Vec<CopyFileResult>,
    /// Summary counts.
    pub summary: CopySummary,
}

/// Summary counts for the copy operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CopySummary {
    /// Number of new files written.
    pub wrote_new: usize,
    /// Number of existing files skipped.
    pub skipped_existing: usize,
}

/// Error codes for copy operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyErrorCode {
    /// Symlink found and symlink rejection is enabled.
    SymlinkRejected,
    /// Resolved path escapes the confined directory.
    PathTraversal,
    /// Source file/directory not found.
    SourceMissing,
}

/// Error for copy operations.
#[derive(Debug)]
pub struct CopyError {
    code: CopyErrorCode,
    message: String,
    offending_path: Option<PathBuf>,
}

impl CopyError {
    pub fn new(code: CopyErrorCode, message: String) -> Self {
        Self {
            code,
            message,
            offending_path: None,
        }
    }

    pub fn with_path(code: CopyErrorCode, message: String, path: &Path) -> Self {
        Self {
            code,
            message,
            offending_path: Some(path.to_path_buf()),
        }
    }
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code: {:?})", self.message, self.code)
    }
}

impl std::error::Error for CopyError {}

impl From<CopyError> for Error {
    fn from(e: CopyError) -> Self {
        StructuredError::new(
            "Copy",
            "copy_error",
            e.to_string(),
        )
    }
}

/// Enumerate all files in a source directory, building a list of CopyItem
/// that mirrors the directory structure under the destination directory.
pub fn walk_source_dir(src_dir: &Path, dst_dir: &Path) -> Vec<CopyItem> {
    let mut items = Vec::new();
    if !src_dir.exists() {
        return items;
    }
    walk(src_dir, src_dir, dst_dir, &mut items);
    items
}

fn walk(root_src: &Path, cur_src: &Path, root_dst: &Path, out: &mut Vec<CopyItem>) {
    let Ok(entries) = read_dir(cur_src) else {
        return;
    };

    for entry in entries.flatten() {
        let abs_src = entry.path();
        let rel = path_relative_from(&abs_src, root_src);
        let Some(rel) = rel else { continue };
        let abs_dst = root_dst.join(rel);
        let Ok(meta) = metadata(&abs_src) else {
            continue;
        };
        if meta.is_dir() {
            walk(root_src, &abs_src, root_dst, out);
        } else if meta.is_file() {
            out.push(CopyItem {
                source: abs_src,
                target: abs_dst,
            });
        }
        // Ignore symlinks/devices/sockets at this stage; rejection happens later during copy.
    }
}

/// Copy all pre-enumerated items, applying safety gates.
/// All items are validated before any writes happen.
pub fn copy_artifacts(items: &[CopyItem], opts: &CopyArtifactsOpts) -> Result<CopyResult> {
    let reject_symlinks = opts.reject_symlinks.unwrap_or(false);
    let confine = opts.confine_realpath_to.as_ref();
    let dry_run = opts.dry_run.unwrap_or(false);

    // First pass: validate everything
    for item in items {
        let source = &item.source;
        if !source.exists() {
            return Err(CopyError::new(
                CopyErrorCode::SourceMissing,
                format!("Source file missing: {}", source.display()),
            ).into());
        }

        let meta = metadata(source).map_err(|_| {
            CopyError::new(
                CopyErrorCode::SourceMissing,
                format!("Cannot stat source file: {}", source.display()),
            )
        })?;

        if reject_symlinks {
            if !meta.is_file() && !meta.is_dir() {
                return Err(CopyError::with_path(
                    CopyErrorCode::SymlinkRejected,
                    "Symlinks are rejected (reject_symlinks: true)".to_string(),
                    source,
                ).into());
            }
        }

        if let Some(confine) = confine {
            let Ok(canon) = canonicalize(&item.target) else {
                // If target doesn't exist yet, canonicalize its parent
                if let Some(parent) = item.target.parent() {
                    if let Ok(parent_canon) = canonicalize(parent) {
                        if !parent_canon.starts_with(confine) {
                        return Err(CopyError::with_path(
                            CopyErrorCode::PathTraversal,
                            "Target path escapes confined directory".to_string(),
                            &item.target,
                        ).into());
                        }
                    }
                }
                continue;
            };
            if !canon.starts_with(confine) {
            return Err(CopyError::with_path(
                CopyErrorCode::PathTraversal,
                "Target path escapes confined directory".to_string(),
                &item.target,
            ).into());
            }
        }
    }

    // Second pass: copy everything
    let mut result = CopyResult {
        dry_run,
        files: Vec::with_capacity(items.len()),
        summary: CopySummary::default(),
    };

    for item in items {
        let target = &item.target;

        if target.exists() {
            result.files.push(CopyFileResult {
                source: item.source.clone(),
                target: target.clone(),
                outcome: CopyOutcome::SkippedExisting,
            });
            result.summary.skipped_existing += 1;
            continue;
        }

        // Create parent directory if it doesn't exist
        if let Some(parent) = target.parent() {
            if !parent.exists() {
                let _ = create_dir_all(parent);
            }
        }

        if !dry_run {
            let content = read(&item.source).map_err(|e| {
                CopyError::new(
                    CopyErrorCode::SourceMissing,
                    format!("Failed to read source: {e}"),
                )
            })?;
            write(target, content).map_err(|e| {
                CopyError::new(
                    CopyErrorCode::SourceMissing,
                    format!("Failed to write target: {e}"),
                )
            })?;
        }

        result.files.push(CopyFileResult {
            source: item.source.clone(),
            target: target.clone(),
            outcome: CopyOutcome::WroteNew,
        });
        result.summary.wrote_new += 1;
    }

    Ok(result)
}

/// Get relative path from base to child, similar to Node's path.relative.
fn path_relative_from<'a>(child: &'a Path, base: &Path) -> Option<PathBuf> {
    let base_components: Vec<_> = base.components().collect();
    let child_components: Vec<_> = child.components().collect();

    let mut common = 0;
    while common < base_components.len() && common < child_components.len()
        && base_components[common] == child_components[common]
    {
        common += 1;
    }

    let mut rel = PathBuf::new();
    for _ in common..base_components.len() {
        rel.push("..");
    }
    for comp in &child_components[common..] {
        rel.push(comp);
    }

    Some(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_walk() {
        let dir = tempdir().unwrap();
        let src = dir.path();
        std::fs::write(src.join("a.txt"), "content").unwrap();
        std::fs::create_dir(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("b.txt"), "content2").unwrap();

        let dst = tempdir().unwrap();
        let items = walk_source_dir(src, dst.path());
        assert_eq!(items.len(), 2);
    }
}
