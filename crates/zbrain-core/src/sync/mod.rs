//! Sync pipeline — import files from git repos into the knowledge base.
//!
//! Sub-modules:
//! - `failures`: JSONL-based failure recording and acknowledgement.
//! - `anchor`:   Sync anchor management (last_commit, chunker_version).
//! - `walker`:   File system traversal with inode cycle detection and strategy filtering.
//! - `manifest`: Git diff parsing and syncable file filtering.
//! - `concurrency`: Engine-type detection → concurrency strategy selection.
//! - `import`:   Single-file import: read → capture → parse_markdown → put_page + add_tag.
//! - `core`:     Main sync loop: `perform_sync` / `perform_full_sync`.
//! - `lock`:     Advisory cross-process file lock (roadmap 1-6-7-13 Q4).

pub mod anchor;
pub mod concurrency;
pub mod core;
pub mod failures;
pub mod import;
pub mod lock;
pub mod manifest;
pub mod walker;
