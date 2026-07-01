//! Sync manifest — parse git diff output to determine which files changed.
//!
//! Uses `git diff --name-status <from> <to>` to produce a list of changed
//! files, then applies `is_syncable` filtering to determine which files
//! need to be synced, re-synced, or deleted.

use std::path::{Path, PathBuf};

/// Status of a file in the git diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffStatus {
    /// New file added.
    Added,
    /// Existing file modified.
    Modified,
    /// File deleted.
    Deleted,
    /// File renamed (old_path → new_path).
    Renamed,
    /// File copied.
    Copied,
}

/// A single entry from `git diff --name-status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub status: DiffStatus,
    /// For Renamed/Copied: the original path.
    pub old_path: Option<PathBuf>,
    /// The current path.
    pub path: PathBuf,
}

/// Parse the output of `git diff --name-status` into a list of `DiffEntry`.
///
/// Expected input format (one entry per line):
/// ```text
/// A\tpath/to/file.md
/// M\tpath/to/other.md
/// D\tpath/to/deleted.md
/// R100\told/path.md\tnew/path.md
/// C100\told/path.md\tnew/path.md
/// ```
pub fn parse_diff_name_status(output: &str) -> Vec<DiffEntry> {
    let mut entries = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split('\t').collect();
        if parts.is_empty() {
            continue;
        }

        let status_code = parts[0];
        let entry = match status_code.chars().next() {
            Some('A') if parts.len() >= 2 => DiffEntry {
                status: DiffStatus::Added,
                old_path: None,
                path: PathBuf::from(parts[1]),
            },
            Some('M') if parts.len() >= 2 => DiffEntry {
                status: DiffStatus::Modified,
                old_path: None,
                path: PathBuf::from(parts[1]),
            },
            Some('D') if parts.len() >= 2 => DiffEntry {
                status: DiffStatus::Deleted,
                old_path: None,
                path: PathBuf::from(parts[1]),
            },
            Some('R') if parts.len() >= 3 => DiffEntry {
                status: DiffStatus::Renamed,
                old_path: Some(PathBuf::from(parts[1])),
                path: PathBuf::from(parts[2]),
            },
            Some('C') if parts.len() >= 3 => DiffEntry {
                status: DiffStatus::Copied,
                old_path: Some(PathBuf::from(parts[1])),
                path: PathBuf::from(parts[2]),
            },
            _ => continue, // Skip unrecognized lines
        };

        entries.push(entry);
    }

    entries
}

/// A file that needs to be synced (added, modified, renamed, or copied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncableFile {
    pub rel_path: PathBuf,
    /// For renames: the old path that should have its page updated.
    pub old_rel_path: Option<PathBuf>,
    pub status: DiffStatus,
}

/// A file that needs to be deleted from the knowledge base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletableFile {
    pub rel_path: PathBuf,
}

/// Result of computing the sync manifest from a git diff.
#[derive(Debug, Clone)]
pub struct SyncManifest {
    /// Files that need to be imported or updated.
    pub to_sync: Vec<SyncableFile>,
    /// Files that need to be deleted from the KB.
    pub to_delete: Vec<DeletableFile>,
}

/// Build a sync manifest from git diff output, applying a syncability filter.
///
/// `is_syncable` determines whether a file should be synced. Files that
/// are deleted are always included in `to_delete` regardless of the filter.
pub fn build_manifest<F>(diff_output: &str, is_syncable: F) -> SyncManifest
where
    F: Fn(&Path) -> bool,
{
    let diff_entries = parse_diff_name_status(diff_output);
    let mut to_sync = Vec::new();
    let mut to_delete = Vec::new();

    for entry in diff_entries {
        match entry.status {
            DiffStatus::Deleted => {
                to_delete.push(DeletableFile {
                    rel_path: entry.path,
                });
            }
            DiffStatus::Added | DiffStatus::Modified | DiffStatus::Copied => {
                if is_syncable(&entry.path) {
                    to_sync.push(SyncableFile {
                        rel_path: entry.path,
                        old_rel_path: None,
                        status: entry.status,
                    });
                }
            }
            DiffStatus::Renamed => {
                if is_syncable(&entry.path) {
                    to_sync.push(SyncableFile {
                        rel_path: entry.path,
                        old_rel_path: entry.old_path,
                        status: DiffStatus::Renamed,
                    });
                }
            }
        }
    }

    SyncManifest { to_sync, to_delete }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_added_modified_deleted() {
        let output = "A\tdocs/new.md\nM\tsrc/main.rs\nD\told/deprecated.md\n";
        let entries = parse_diff_name_status(output);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, DiffStatus::Added);
        assert_eq!(entries[0].path, PathBuf::from("docs/new.md"));
        assert_eq!(entries[0].old_path, None);

        assert_eq!(entries[1].status, DiffStatus::Modified);
        assert_eq!(entries[1].path, PathBuf::from("src/main.rs"));

        assert_eq!(entries[2].status, DiffStatus::Deleted);
        assert_eq!(entries[2].path, PathBuf::from("old/deprecated.md"));
    }

    #[test]
    fn parse_renamed_and_copied() {
        let output = "R100\told/name.md\tnew/name.md\nC080\tsrc/orig.rs\tsrc/copy.rs\n";
        let entries = parse_diff_name_status(output);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].status, DiffStatus::Renamed);
        assert_eq!(entries[0].old_path, Some(PathBuf::from("old/name.md")));
        assert_eq!(entries[0].path, PathBuf::from("new/name.md"));

        assert_eq!(entries[1].status, DiffStatus::Copied);
        assert_eq!(entries[1].old_path, Some(PathBuf::from("src/orig.rs")));
        assert_eq!(entries[1].path, PathBuf::from("src/copy.rs"));
    }

    #[test]
    fn parse_empty_output() {
        let entries = parse_diff_name_status("");
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_output_with_blank_lines() {
        let output = "A\tfile.md\n\nM\tother.md\n";
        let entries = parse_diff_name_status(output);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn build_manifest_filters_by_syncability() {
        let output = "A\tdocs/readme.md\nA\timage.png\nM\tsrc/main.rs\nD\told.txt\n";

        let manifest = build_manifest(output, |p| {
            p.extension().is_some_and(|ext| ext == "md" || ext == "rs")
        });

        // to_sync: only .md and .rs files (added or modified)
        assert_eq!(manifest.to_sync.len(), 2);
        let sync_paths: Vec<&Path> = manifest.to_sync.iter().map(|f| f.rel_path.as_path()).collect();
        assert!(sync_paths.contains(&Path::new("docs/readme.md")));
        assert!(sync_paths.contains(&Path::new("src/main.rs")));

        // image.png is not syncable → not in to_sync
        assert!(!sync_paths.contains(&Path::new("image.png")));

        // to_delete: always includes deleted files (regardless of filter)
        assert_eq!(manifest.to_delete.len(), 1);
        assert_eq!(manifest.to_delete[0].rel_path, PathBuf::from("old.txt"));
    }

    #[test]
    fn build_manifest_handles_renames() {
        let output = "R100\tdocs/old.md\tdocs/new.md\n";

        let manifest = build_manifest(output, |_| true);

        assert_eq!(manifest.to_sync.len(), 1);
        assert_eq!(manifest.to_sync[0].rel_path, PathBuf::from("docs/new.md"));
        assert_eq!(manifest.to_sync[0].old_rel_path, Some(PathBuf::from("docs/old.md")));
        assert_eq!(manifest.to_sync[0].status, DiffStatus::Renamed);
        assert!(manifest.to_delete.is_empty());
    }

    #[test]
    fn build_manifest_empty_diff() {
        let manifest = build_manifest("", |_| true);
        assert!(manifest.to_sync.is_empty());
        assert!(manifest.to_delete.is_empty());
    }
}
