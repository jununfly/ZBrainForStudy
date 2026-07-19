//! skill_resolver — Rust port of the skill-tree resolver/validation stack.
//!
//! This module is the unified home for the skill/resolver validation
//! subsystem that previously lived across several TS modules
//! (`src/core/{resolver-filenames,skill-frontmatter,skill-manifest,
//! skill-trigger-index,check-resolvable,repo-root}.ts`). It is the shared
//! primitive behind `zbrain check-resolvable` and (once migrated) `doctor`
//! and `skillify-check`.
//!
//! Slice plan (roadmap 1-6-5):
//!   - resolver_filenames : filename policy (RESOLVER.md / AGENTS.md)
//!   - skill_frontmatter  : content-based SKILL.md frontmatter parser
//!   - skill_manifest      : manifest load-or-derive
//!   - trigger_index       : unified trigger index (UNION frontmatter+resolver)
//!   - check_resolvable    : reachability / MECE / DRY / stub checks (1-6-5-2)
//!   - repo_root           : skills-dir auto-detection (1-6-5-3)

pub mod resolver_filenames;
pub mod skill_frontmatter;
pub mod skill_manifest;
pub mod trigger_index;
pub mod check_resolvable;
pub mod repo_root;
