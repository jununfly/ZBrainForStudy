//! Sync file system walker.
//!
//! Walks a source directory using `walkdir`, collecting files that are
//! eligible for sync while:
//! - Detecting symlink/inode cycles (via device+inode tracking).
//! - Filtering out `.git/` directories and other non-syncable paths.
//! - Applying a user-provided `is_syncable` predicate for further filtering.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A file entry discovered by the walker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    /// Absolute path to the file.
    pub abs_path: PathBuf,
    /// Path relative to the source root.
    pub rel_path: PathBuf,
}

/// Options for the sync walker.
#[derive(Debug, Clone)]
pub struct WalkOpts {
    /// Root directory of the source (cloned repo).
    pub root: PathBuf,
    /// Whether to follow symbolic links.
    pub follow_links: bool,
    /// Maximum file size in bytes. Files larger than this are skipped.
    /// `None` means no limit.
    pub max_file_size: Option<u64>,
}

impl Default for WalkOpts {
    fn default() -> Self {
        WalkOpts {
            root: PathBuf::new(),
            follow_links: false,
            max_file_size: Some(10 * 1024 * 1024), // 10 MiB default
        }
    }
}

/// Walk a source directory and return syncable file entries.
///
/// # Cycle detection
///
/// Uses (device, inode) pairs to detect filesystem loops. When a file is
/// encountered with the same (dev, ino) as a previously-seen entry, it is
/// skipped with a warning instead of causing infinite recursion.
///
/// # Default exclusions
///
/// The following are always excluded:
/// - `.git/` directories (entire subtree)
/// - `node_modules/` directories
/// - Hidden files/dirs starting with `.` (except the root itself)
pub fn walk_source<F>(
    opts: &WalkOpts,
    is_syncable: F,
) -> Result<Vec<WalkEntry>, walkdir::Error>
where
    F: Fn(&Path) -> bool,
{
    let mut entries = Vec::new();
    #[cfg(unix)]
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();

    let walker = walkdir::WalkDir::new(&opts.root)
        .follow_links(opts.follow_links)
        .into_iter()
        .filter_entry(|e| {
            // Prune .git and node_modules subtrees entirely
            let file_name = e.file_name();
            if file_name == ".git" || file_name == "node_modules" {
                return false;
            }
            true
        });

    for entry in walker {
        let entry = entry?;
        let path = entry.path();

        // Skip directories
        if entry.file_type().is_dir() {
            continue;
        }

        // Inode-based cycle detection (only when following symlinks)
        #[cfg(unix)]
        if opts.follow_links {
            use std::os::unix::fs::MetadataExt;
            if let Ok(meta) = entry.metadata() {
                let key = (meta.dev(), meta.ino());
                if !seen_inodes.insert(key) {
                    tracing::warn!(
                        "sync_walker: skipping duplicate inode ({}, {}) at {}",
                        key.0,
                        key.1,
                        path.display()
                    );
                    continue;
                }
            }
        }

        // Exclude hidden files/directories
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if file_name.starts_with('.') {
                continue;
            }
        }

        // Check file size limit
        if let Some(max_size) = opts.max_file_size {
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > max_size {
                    continue;
                }
            }
        }

        // Compute relative path
        let Ok(rel_path) = path.strip_prefix(&opts.root) else {
            continue;
        };

        // Apply user predicate
        if !is_syncable(rel_path) {
            continue;
        }

        entries.push(WalkEntry {
            abs_path: path.to_path_buf(),
            rel_path: rel_path.to_path_buf(),
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn create_test_tree(root: &Path) {
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();

        let mut f = fs::File::create(root.join("README.md")).unwrap();
        f.write_all(b"# README").unwrap();

        let mut f = fs::File::create(root.join("docs/guide.md")).unwrap();
        f.write_all(b"# Guide").unwrap();

        let mut f = fs::File::create(root.join("docs/image.png")).unwrap();
        f.write_all(b"PNG_DATA").unwrap();

        let mut f = fs::File::create(root.join("src/main.rs")).unwrap();
        f.write_all(b"fn main() {}").unwrap();

        // .git files should be excluded
        let mut f = fs::File::create(root.join(".git/HEAD")).unwrap();
        f.write_all(b"ref: refs/heads/main").unwrap();

        // node_modules files should be excluded
        let mut f = fs::File::create(root.join("node_modules/pkg/index.js")).unwrap();
        f.write_all(b"module.exports = {}").unwrap();
    }

    #[test]
    fn walk_collects_all_syncable_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        create_test_tree(root);

        let opts = WalkOpts {
            root: root.to_path_buf(),
            ..Default::default()
        };

        let entries = walk_source(&opts, |_| true).unwrap();

        // Should find: README.md, docs/guide.md, docs/image.png, src/main.rs
        // Should NOT find: .git/HEAD, node_modules/pkg/index.js
        let rel_paths: Vec<&Path> = entries
            .iter()
            .map(|e| e.rel_path.as_path())
            .collect();

        assert!(rel_paths.contains(&Path::new("README.md")));
        assert!(rel_paths.contains(&Path::new("docs/guide.md")));
        assert!(rel_paths.contains(&Path::new("docs/image.png")));
        assert!(rel_paths.contains(&Path::new("src/main.rs")));

        // Excluded paths
        for p in &rel_paths {
            let s = p.to_string_lossy();
            assert!(!s.starts_with(".git"), "should exclude .git: {s}");
            assert!(!s.starts_with("node_modules"), "should exclude node_modules: {s}");
        }
    }

    #[test]
    fn walk_respects_is_syncable_filter() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        create_test_tree(root);

        let opts = WalkOpts {
            root: root.to_path_buf(),
            ..Default::default()
        };

        // Only .md files
        let entries = walk_source(&opts, |p| {
            p.extension().is_some_and(|ext| ext == "md")
        })
        .unwrap();

        let rel_paths: Vec<&Path> = entries.iter().map(|e| e.rel_path.as_path()).collect();

        assert!(rel_paths.contains(&Path::new("README.md")));
        assert!(rel_paths.contains(&Path::new("docs/guide.md")));
        assert!(!rel_paths.contains(&Path::new("docs/image.png")));
        assert!(!rel_paths.contains(&Path::new("src/main.rs")));
    }

    #[test]
    fn walk_respects_max_file_size() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        fs::create_dir_all(root).unwrap();
        let mut f = fs::File::create(root.join("small.md")).unwrap();
        f.write_all(b"small").unwrap();

        let mut f = fs::File::create(root.join("large.md")).unwrap();
        f.write_all(&vec![0u8; 1000]).unwrap();

        let opts = WalkOpts {
            root: root.to_path_buf(),
            max_file_size: Some(100), // 100 bytes
            ..Default::default()
        };

        let entries = walk_source(&opts, |_| true).unwrap();
        let names: Vec<&Path> = entries
            .iter()
            .map(|e| e.rel_path.as_path())
            .collect();

        assert!(names.contains(&Path::new("small.md")), "small.md should be included");
        assert!(!names.contains(&Path::new("large.md")), "large.md should be excluded");
    }

    #[test]
    fn walk_empty_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let opts = WalkOpts {
            root: root.to_path_buf(),
            ..Default::default()
        };

        let entries = walk_source(&opts, |_| true).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn walk_nonexistent_directory_is_error() {
        let opts = WalkOpts {
            root: PathBuf::from("/nonexistent/path/for/testing"),
            ..Default::default()
        };

        let result = walk_source(&opts, |_| true);
        assert!(result.is_err());
    }
}
