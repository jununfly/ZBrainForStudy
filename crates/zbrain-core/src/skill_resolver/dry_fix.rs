//! dry_fix — DRY-violation auto-repair (the `--fix` write path).
//!
//! Ported from `src/core/dry-fix.ts` + `src/core/skill-fix-gates.ts`.
//! Scans every skill in the manifest and repairs two violation classes the
//! read-only `check_resolvable` surfaces:
//!
//!   * **REPLACE** (`CROSS_CUTTING_PATTERNS`): inline cross-cutting rules
//!     (e.g. the Iron Law of back-linking) are replaced with a
//!     `> **Convention:** ...` reference line.
//!   * **INSERT** (`MISSING_RULE_PATTERNS`): skills that call external
//!     lookup tools but never declare brain-first compliance get a
//!     `> **Convention:** see conventions/brain-first.md ...` callout
//!     inserted near the top.
//!
//! All writes are guarded by `skill-fix-gates` (the git-is-backup contract):
//! a file is only mutated when it is tracked by git with a clean working
//! tree, the match is not inside a code fence, it isn't already delegated,
//! and there is exactly one match (ambiguous blocks refuse). `--dry-run`
//! returns the proposed edits without touching disk.
//!
//! Slice plan (roadmap 1-6-5-8):
//!   - 1-6-5-8-1 : safety gates (code-fence + git working-tree) + shared types
//!   - 1-6-5-8-2 : REPLACE cross-cutting (block expansion + attemptFix)
//!   - 1-6-5-8-3 : brain-first analyzer + INSERT missing-rule
//!   - 1-6-5-8-4 : wire `check-resolvable --fix` / `--dry-run` CLI

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::skill_resolver::check_resolvable::{
    extract_delegation_targets, CrossCuttingPattern, CROSS_CUTTING_PATTERNS, DelegationRef,
    DRY_PROXIMITY_LINES,
};
use crate::skill_resolver::brain_first::{
    analyze_skill_brain_first, convention_callout_re, BrainFirstStatus,
};
use crate::skill_resolver::skill_frontmatter::parse_skill_frontmatter;
use crate::skill_resolver::skill_manifest::load_or_derive_manifest;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Options for [`auto_fix_dry_violations`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AutoFixOptions {
    /// When true, compute and return proposed edits without writing.
    pub dry_run: bool,
}

/// Outcome status of one fix attempt, mirroring the TS `FixStatus` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FixStatus {
    Applied,
    Proposed,
    Skipped,
    Error,
}

/// Why a fix was skipped/errored. Mirrors the TS `SkipReason` union
/// (serde names match the TS reason strings byte-for-byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    WorkingTreeDirty,
    NoGitBackup,
    InsideCodeFence,
    AlreadyDelegated,
    AmbiguousMultipleMatches,
    BlockIsCallout,
    FileMissing,
    ReadError,
    WriteError,
}

/// One fix outcome (applied / proposed / skipped / error).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FixOutcome {
    pub skill: String,
    #[serde(rename = "skillPath")]
    pub skill_path: String,
    #[serde(rename = "patternLabel")]
    pub pattern_label: String,
    pub status: FixStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<SkipReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Aggregate report returned by [`auto_fix_dry_violations`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AutoFixReport {
    /// Applied writes (or proposals when `dry_run`).
    pub fixed: Vec<FixOutcome>,
    /// Skips and errors.
    pub skipped: Vec<FixOutcome>,
}

// ---------------------------------------------------------------------------
// Safety gates (ported from src/core/skill-fix-gates.ts)
// ---------------------------------------------------------------------------

/// Git working-tree state of a single skill file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingTreeStatus {
    /// Tracked, no uncommitted changes — safe to write.
    Clean,
    /// Uncommitted changes — refuse (would mix with the user's edits).
    Dirty,
    /// Not under git at all — refuse (no rollback path).
    NotARepo,
}

/// True when the byte `offset` sits inside a fenced code block
/// (``` ... ```). Counts triple-backtick fence lines before `offset`; an
/// odd count means "inside a fence". Mirrors `isInsideCodeFence`.
pub fn is_inside_code_fence(content: &str, offset: usize) -> bool {
    let upper = offset.min(content.len());
    let before = &content[..upper];
    let fence_count = before.lines().filter(|l| l.starts_with("```")).count();
    fence_count % 2 == 1
}

/// Query git for the working-tree state of `skill_path`.
///
/// Runs `git status --porcelain -- <skill_path>` from the file's parent dir
/// (array args, no shell — odd paths can't inject commands). Three outcomes:
///   * `clean`     — tracked + no uncommitted changes
///   * `dirty`     — tracked + uncommitted changes present
///   * `not_a_repo` — git errored (e.g. exit 128 outside a repo)
pub fn get_working_tree_status(skill_path: &Path) -> WorkingTreeStatus {
    let parent = skill_path.parent().unwrap_or_else(|| Path::new("."));
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .arg("--")
        .arg(skill_path)
        .current_dir(parent)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout);
            if out.trim().is_empty() {
                WorkingTreeStatus::Clean
            } else {
                WorkingTreeStatus::Dirty
            }
        }
        _ => WorkingTreeStatus::NotARepo,
    }
}

/// Legacy coarse check: true only when the file is tracked and dirty.
/// Mirrors `isWorkingTreeDirty`.
pub fn is_working_tree_dirty(skill_path: &Path) -> bool {
    get_working_tree_status(skill_path) == WorkingTreeStatus::Dirty
}

// ---------------------------------------------------------------------------
// Wire into the crate module tree.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// REPLACE — cross-cutting DRY inlining (ported from src/core/dry-fix.ts)
// ---------------------------------------------------------------------------

/// A contiguous markdown block (0-indexed inclusive line span).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub start_line: usize,
    pub end_line: usize,
}

/// Shape of the markdown block a match line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockShape {
    Bullet,
    Blockquote,
    Paragraph,
}

fn bullet_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^(\s*)(?:[-*]\s|\d+\.\s)").unwrap())
}
fn indent_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^(\s*)").unwrap())
}
fn blockquote_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^>\s").unwrap())
}
fn callout_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\*\*(?:Convention|Filing rule):\*\*").unwrap())
}

/// Detect which block shape the line at `line_idx` belongs to.
pub fn detect_block_shape(lines: &[&str], line_idx: usize) -> BlockShape {
    let line = lines.get(line_idx).copied().unwrap_or("");
    if bullet_re().is_match(line) {
        BlockShape::Bullet
    } else if blockquote_re().is_match(line) {
        BlockShape::Blockquote
    } else {
        BlockShape::Paragraph
    }
}

/// Expand a bullet item: start at the bullet line, end at the next sibling
/// or shallower bullet (sub-bullets included). Mirrors `expandBullet`.
pub fn expand_bullet(lines: &[&str], line_idx: usize) -> Option<Block> {
    let line = lines.get(line_idx).copied().unwrap_or("");
    let indent_match = bullet_re().captures(line)?;
    let base_indent = indent_match.get(1)?.as_str().len();

    let mut start = line_idx;
    while start > 0 {
        let prev = lines.get(start - 1).copied().unwrap_or("");
        let prev_is_bullet = bullet_re().is_match(prev);
        let prev_indent = indent_re()
            .captures(prev)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().len())
            .unwrap_or(0);
        if prev_is_bullet && prev_indent <= base_indent {
            break;
        }
        if prev.trim().is_empty() {
            break;
        }
        start -= 1;
    }

    let mut end = line_idx;
    for i in (line_idx + 1)..lines.len() {
        let l = lines[i];
        if l.trim().is_empty() {
            break;
        }
        let is_bullet = bullet_re().is_match(l);
        let indent = indent_re()
            .captures(l)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().len())
            .unwrap_or(0);
        if is_bullet && indent <= base_indent {
            break;
        }
        end = i;
    }
    Some(Block { start_line: start, end_line: end })
}

/// Expand a blockquote: contiguous `>` lines. Returns `None` when the block
/// is itself a `> **Convention:**` / `> **Filing rule:**` callout (don't
/// rewrite a reference into a reference). Mirrors `expandBlockquote`.
pub fn expand_blockquote(lines: &[&str], line_idx: usize) -> Option<Block> {
    if !blockquote_re().is_match(lines.get(line_idx).copied().unwrap_or("")) {
        return None;
    }
    let mut start = line_idx;
    while start > 0 && blockquote_re().is_match(lines[start - 1]) {
        start -= 1;
    }
    let mut end = line_idx;
    while end + 1 < lines.len() && blockquote_re().is_match(lines[end + 1]) {
        end += 1;
    }
    let first_line = lines.get(start).copied().unwrap_or("");
    if callout_re().is_match(first_line) {
        return None;
    }
    Some(Block { start_line: start, end_line: end })
}

/// Expand a paragraph: previous blank line -> next blank line.
pub fn expand_paragraph(lines: &[&str], line_idx: usize) -> Option<Block> {
    let mut start = line_idx;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    let mut end = line_idx;
    while end + 1 < lines.len() && !lines[end + 1].trim().is_empty() {
        end += 1;
    }
    Some(Block { start_line: start, end_line: end })
}

/// Expand the block at `line_idx` according to its detected shape.
pub fn expand_block(lines: &[&str], line_idx: usize) -> Option<Block> {
    match detect_block_shape(lines, line_idx) {
        BlockShape::Bullet => expand_bullet(lines, line_idx),
        BlockShape::Blockquote => expand_blockquote(lines, line_idx),
        BlockShape::Paragraph => expand_paragraph(lines, line_idx),
    }
}

/// Attempt a REPLACE fix for one skill + cross-cutting pattern.
///
/// Mirrors `attemptFix` in `dry-fix.ts`. Returns:
///   * `None` when the pattern has no match in this skill (silent).
///   * `Some(skipped)` outcome when a safety gate blocks the write.
///   * `Some(proposed)` outcome (dry_run) with before/after preview.
///   * `Some(applied)` outcome on successful write.
///   * `Some(error)` outcome on read/write failure.
fn attempt_fix(
    skill_name: &str,
    skill_path: &Path,
    content: &str,
    delegations: &[DelegationRef],
    cut: &CrossCuttingPattern,
    opts: &AutoFixOptions,
) -> Option<FixOutcome> {
    let base_path = skill_path.to_string_lossy().to_string();

    let re = match regex::Regex::new(cut.pattern) {
        Ok(r) => r,
        Err(_) => return None,
    };
    let matches: Vec<regex::Match> = re.find_iter(content).collect();
    if matches.is_empty() {
        return None;
    }
    if matches.len() > 1 {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: cut.label.to_string(),
            status: FixStatus::Skipped,
            reason: Some(SkipReason::AmbiguousMultipleMatches),
            before: None,
            after: None,
        });
    }

    let m = matches[0];
    let offset = m.start();

    if is_inside_code_fence(content, offset) {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: cut.label.to_string(),
            status: FixStatus::Skipped,
            reason: Some(SkipReason::InsideCodeFence),
            before: None,
            after: None,
        });
    }

    let match_line = content[..offset].matches('\n').count() + 1;
    let already_delegated = delegations.iter().any(|d| {
        cut.conventions.contains(&d.convention.as_str()) && d.line.abs_diff(match_line) <= DRY_PROXIMITY_LINES
    });
    if already_delegated {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: cut.label.to_string(),
            status: FixStatus::Skipped,
            reason: Some(SkipReason::AlreadyDelegated),
            before: None,
            after: None,
        });
    }

    match get_working_tree_status(skill_path) {
        WorkingTreeStatus::Dirty => {
            return Some(FixOutcome {
                skill: skill_name.to_string(),
                skill_path: base_path,
                pattern_label: cut.label.to_string(),
                status: FixStatus::Skipped,
                reason: Some(SkipReason::WorkingTreeDirty),
                before: None,
                after: None,
            });
        }
        WorkingTreeStatus::NotARepo => {
            return Some(FixOutcome {
                skill: skill_name.to_string(),
                skill_path: base_path,
                pattern_label: cut.label.to_string(),
                status: FixStatus::Skipped,
                reason: Some(SkipReason::NoGitBackup),
                before: None,
                after: None,
            });
        }
        WorkingTreeStatus::Clean => {}
    }

    let lines: Vec<&str> = content.split('\n').collect();
    let line_idx = match_line - 1;
    let block = match expand_block(&lines, line_idx) {
        Some(b) => b,
        None => {
            return Some(FixOutcome {
                skill: skill_name.to_string(),
                skill_path: base_path,
                pattern_label: cut.label.to_string(),
                status: FixStatus::Skipped,
                reason: Some(SkipReason::BlockIsCallout),
                before: None,
                after: None,
            });
        }
    };

    let canonical = cut.conventions[0];
    let replacement = format!("> **Convention:** See `skills/{}` for {}.", canonical, cut.label);
    let original_block = lines[block.start_line..=block.end_line].join("\n");

    let before_part = if block.start_line > 0 {
        Some(lines[..block.start_line].join("\n"))
    } else {
        None
    };
    let after_part = if block.end_line + 1 < lines.len() {
        Some(lines[block.end_line + 1..].join("\n"))
    } else {
        None
    };

    let mut next = String::new();
    if let Some(b) = &before_part {
        next.push_str(b);
        next.push('\n');
    }
    next.push_str(&replacement);
    if let Some(a) = &after_part {
        next.push('\n');
        next.push_str(a);
    }
    if content.ends_with('\n') && !next.ends_with('\n') {
        next.push('\n');
    }

    if opts.dry_run {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: cut.label.to_string(),
            status: FixStatus::Proposed,
            reason: None,
            before: Some(original_block),
            after: Some(replacement),
        });
    }

    if let Err(_) = fs::write(skill_path, &next) {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: cut.label.to_string(),
            status: FixStatus::Error,
            reason: Some(SkipReason::WriteError),
            before: Some(original_block),
            after: Some(replacement),
        });
    }

    Some(FixOutcome {
        skill: skill_name.to_string(),
        skill_path: base_path,
        pattern_label: cut.label.to_string(),
        status: FixStatus::Applied,
        reason: None,
        before: Some(original_block),
        after: Some(replacement),
    })
}

// ---------------------------------------------------------------------------
// INSERT — brain-first missing-rule (ported from src/core/dry-fix.ts
// MISSING_RULE_PATTERNS path, 1-6-5-8-3)
// ---------------------------------------------------------------------------

/// One INSERT-missing-rule pattern. Sibling of `CrossCuttingPattern` but with
/// INSERT semantics: `detect` decides whether THIS skill is missing the rule,
/// `idempotent_check` decides whether it is ALREADY present.
pub struct MissingRulePattern {
    /// Stable label for reporting.
    pub label: &'static str,
    /// Returns true when the rule is MISSING for this skill (needs insert).
    pub detect: fn(&str, &str) -> bool,
    /// Returns true when the rule is ALREADY present (skip insert).
    pub idempotent_check: fn(&str) -> bool,
    /// The literal callout line to insert.
    pub callout: &'static str,
}

/// Detect closure for brain-first compliance: needs insert when the analyzer
/// returns `Warn`.
fn detect_brain_first(content: &str, skill_name: &str) -> bool {
    let fm = parse_skill_frontmatter(content);
    analyze_skill_brain_first(content, skill_name, fm.as_ref()).status == BrainFirstStatus::Warn
}

/// Idempotency closure for brain-first: already present when a Convention
/// callout referencing brain-first exists anywhere in the file.
fn idempotent_brain_first(content: &str) -> bool {
    convention_callout_re().is_match(content)
}

/// v0.36.x missing-rule patterns. Currently one entry — the brain-first
/// Convention callout (motivated by the 2026-05-19 tweet-shield incident).
pub static MISSING_RULE_PATTERNS: &[MissingRulePattern] = &[MissingRulePattern {
    label: "brain-first compliance",
    detect: detect_brain_first,
    idempotent_check: idempotent_brain_first,
    callout:
        "> **Convention:** see [conventions/brain-first.md](../conventions/brain-first.md) for the lookup chain (search -> query -> get_page -> external).",
}];

/// Find the 0-indexed line at which to insert a new Convention callout.
///
/// Insertion strategy `after-h1-paragraph`:
///   1. After frontmatter closing `---`
///   2. After the first `# Title` H1 if present
///   3. After the leading paragraph following the H1 if present
///   4. Before the first `## H2` heading
///   5. Fallback: append at body end if no H2 exists
///
/// Mirrors `findInsertionLine` in `dry-fix.ts`.
pub fn find_insertion_line(content: &str) -> usize {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut cursor = 0;

    // Step 1: skip leading frontmatter fence if present.
    if lines.first().copied() == Some("---") {
        for i in 1..lines.len() {
            if lines[i] == "---" {
                cursor = i + 1;
                break;
            }
        }
    }

    // Step 2: skip blank lines after frontmatter.
    while cursor < lines.len() && lines[cursor].trim().is_empty() {
        cursor += 1;
    }

    // Step 3: if there's a leading H1, advance past it.
    if cursor < lines.len() && h1_re().is_match(lines[cursor]) {
        cursor += 1;
        // Step 4: skip blank lines + the leading paragraph following the H1.
        while cursor < lines.len() && lines[cursor].trim().is_empty() {
            cursor += 1;
        }
        while cursor < lines.len()
            && !lines[cursor].trim().is_empty()
            && !h2_re().is_match(lines[cursor])
            && !frontmatter_line_re().is_match(lines[cursor])
        {
            cursor += 1;
        }
        // Step 5: skip trailing blank lines after the leading paragraph.
        while cursor < lines.len() && lines[cursor].trim().is_empty() {
            cursor += 1;
        }
    }

    // Cursor is now at first H2 OR end of file. Insert here.
    cursor
}

fn h1_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^#\s+").unwrap())
}
fn h2_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^##+\s+").unwrap())
}
fn frontmatter_line_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^---\s*$").unwrap())
}

/// Attempt an INSERT-missing-rule fix for one skill + pattern.
///
/// Mirrors `attemptInsertFix` in `dry-fix.ts`. Returns:
///   * `None` when the detector decides this skill doesn't need the rule
///     (silent — not every skill needs every missing-rule pattern).
///   * `Some(skipped)` outcome when a safety gate blocks the write.
///   * `Some(proposed)` outcome (dryRun) with before/after preview.
///   * `Some(applied)` outcome on successful write.
///   * `Some(error)` outcome on write failure.
fn attempt_insert_fix(
    skill_name: &str,
    skill_path: &Path,
    content: &str,
    mrp: &MissingRulePattern,
    opts: &AutoFixOptions,
) -> Option<FixOutcome> {
    let base_path = skill_path.to_string_lossy().to_string();

    // Detector gate: does this skill NEED the rule inserted?
    if !(mrp.detect)(content, skill_name) {
        return None;
    }

    // Idempotency: is the rule already declared somehow?
    if (mrp.idempotent_check)(content) {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: mrp.label.to_string(),
            status: FixStatus::Skipped,
            reason: Some(SkipReason::AlreadyDelegated),
            before: None,
            after: None,
        });
    }

    match get_working_tree_status(skill_path) {
        WorkingTreeStatus::Dirty => {
            return Some(FixOutcome {
                skill: skill_name.to_string(),
                skill_path: base_path,
                pattern_label: mrp.label.to_string(),
                status: FixStatus::Skipped,
                reason: Some(SkipReason::WorkingTreeDirty),
                before: None,
                after: None,
            });
        }
        WorkingTreeStatus::NotARepo => {
            return Some(FixOutcome {
                skill: skill_name.to_string(),
                skill_path: base_path,
                pattern_label: mrp.label.to_string(),
                status: FixStatus::Skipped,
                reason: Some(SkipReason::NoGitBackup),
                before: None,
                after: None,
            });
        }
        WorkingTreeStatus::Clean => {}
    }

    let insert_at = find_insertion_line(content);
    let lines: Vec<&str> = content.split('\n').collect();
    let mut inserted: Vec<&str> = Vec::new();
    inserted.extend_from_slice(&lines[..insert_at]);
    inserted.push(mrp.callout);
    inserted.push("");
    inserted.extend_from_slice(&lines[insert_at..]);
    let mut next = inserted.join("\n");
    if content.ends_with('\n') && !next.ends_with('\n') {
        next.push('\n');
    }

    if opts.dry_run {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: mrp.label.to_string(),
            status: FixStatus::Proposed,
            reason: None,
            before: Some("(no prior block — inserting new callout)".to_string()),
            after: Some(mrp.callout.to_string()),
        });
    }

    if let Err(_) = fs::write(skill_path, &next) {
        return Some(FixOutcome {
            skill: skill_name.to_string(),
            skill_path: base_path,
            pattern_label: mrp.label.to_string(),
            status: FixStatus::Error,
            reason: Some(SkipReason::WriteError),
            before: Some("(no prior block — inserting new callout)".to_string()),
            after: Some(mrp.callout.to_string()),
        });
    }

    Some(FixOutcome {
        skill: skill_name.to_string(),
        skill_path: base_path,
        pattern_label: mrp.label.to_string(),
        status: FixStatus::Applied,
        reason: None,
        before: Some("(no prior block — inserting new callout)".to_string()),
        after: Some(mrp.callout.to_string()),
    })
}

/// Auto-repair DRY violations across every skill in the manifest.
///
/// Slice 1-6-5-8-2 implements the REPLACE (cross-cutting DRY inlining) path.
/// Slice 1-6-5-8-3 adds the INSERT (brain-first missing-rule) path.
pub fn auto_fix_dry_violations(skills_dir: &Path, opts: &AutoFixOptions) -> AutoFixReport {
    let mut report = AutoFixReport::default();
    let manifest = load_or_derive_manifest(skills_dir).skills;

    for skill in &manifest {
        let skill_path = skills_dir.join(&skill.path);
        if !skill_path.exists() {
            continue;
        }
        let mut content = match fs::read_to_string(&skill_path) {
            Ok(c) => c,
            Err(_) => {
                report.skipped.push(FixOutcome {
                    skill: skill.name.clone(),
                    skill_path: skill_path.to_string_lossy().to_string(),
                    pattern_label: "(all)".to_string(),
                    status: FixStatus::Error,
                    reason: Some(SkipReason::ReadError),
                    before: None,
                    after: None,
                });
                continue;
            }
        };

        let mut delegations = extract_delegation_targets(&content);

        for cut in CROSS_CUTTING_PATTERNS {
            if let Some(outcome) = attempt_fix(
                &skill.name,
                &skill_path,
                &content,
                &delegations,
                cut,
                opts,
            ) {
                match outcome.status {
                    FixStatus::Applied | FixStatus::Proposed => {
                        let was_applied = outcome.status == FixStatus::Applied;
                        report.fixed.push(outcome);
                        if was_applied {
                            match fs::read_to_string(&skill_path) {
                                Ok(new) => {
                                    content = new;
                                    delegations = extract_delegation_targets(&content);
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    _ => report.skipped.push(outcome),
                }
            }
        }

        // INSERT (brain-first missing-rule) patterns — run AFTER REPLACE so a
        // freshly-inserted Convention callout from REPLACE doesn't get a
        // second INSERT layered on top.
        for mrp in MISSING_RULE_PATTERNS {
            if let Some(outcome) = attempt_insert_fix(&skill.name, &skill_path, &content, mrp, opts) {
                match outcome.status {
                    FixStatus::Applied | FixStatus::Proposed => {
                        let was_applied = outcome.status == FixStatus::Applied;
                        report.fixed.push(outcome);
                        if was_applied {
                            if let Ok(new) = fs::read_to_string(&skill_path) {
                                content = new;
                            } else {
                                break;
                            }
                        }
                    }
                    _ => report.skipped.push(outcome),
                }
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_df_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create a git repo at `dir` with `name` committed (clean tree).
    /// Returns the absolute path to the committed file.
    fn git_repo_with_committed_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let f = dir.join(name);
        fs::write(&f, content).unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git available in test env")
        };
        run(&["init", "-q"]);
        run(&["add", name]);
        run(&[
            "-c", "user.email=test@test", "-c", "user.name=test", "commit", "-q", "-m", "c",
        ]);
        f
    }

    /// Init a git repo at `root`, write `rel` (nested path allowed), add +
    /// commit so the working tree is clean. Used by REPLACE/INSERT tests
    /// that need a tracked, clean file.
    fn git_repo_committed(root: &Path, rel: &str, content: &str) {
        let full = root.join(rel);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, content).unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("git available in test env")
        };
        run(&["init", "-q"]);
        run(&["add", rel]);
        run(&[
            "-c", "user.email=test@test", "-c", "user.name=test", "commit", "-q", "-m", "c",
        ]);
    }

    #[test]
    fn code_fence_outside_is_false() {
        let c = "intro\n\n```js\nlet x = 1;\n```\n\nafter fence";
        // offset inside the fence body
        let off = c.find("let x").unwrap();
        assert!(is_inside_code_fence(c, off));
        // offset before the fence
        let off2 = c.find("intro").unwrap();
        assert!(!is_inside_code_fence(c, off2));
        // offset after the closing fence
        let off3 = c.find("after fence").unwrap();
        assert!(!is_inside_code_fence(c, off3));
    }

    #[test]
    fn code_fence_even_count_is_false() {
        // Two complete fence blocks:
        //   line0 ```   (open 1)      line3 ```   (open 2)
        //   line1 a                 line4 b
        //   line2 ```   (close 1)     line5 ```   (close 2)
        // The newline at byte offset 9 sits right after close-1 and before
        // open-2: exactly 2 fences precede it -> even -> outside the block.
        // (Parity with TS: isInsideCodeFence returns `fenceCount % 2 === 1`
        //  over fences strictly before the offset.)
        let c = "```\na\n```\n```\nb\n```";
        let off = 9usize;
        assert_eq!(c.as_bytes()[off], b'\n');
        assert!(!is_inside_code_fence(c, off));
        // Before the very first fence (0 fences) is also outside.
        assert!(!is_inside_code_fence(c, 0));
        // After the final closing fence (4 pairs, 6 fences total) is outside.
        assert!(!is_inside_code_fence(c, c.len()));
    }

    // --- 1-6-5-8-2: REPLACE cross-cutting DRY ---

    #[test]
    fn replace_paragraph_applied() {
        let root = scratch("repl_para");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\ntriggers:\n  - t\n---\n\nWe follow the iron law of back-linking here.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        assert_eq!(report.fixed.len(), 1, "outcomes: {:?}", report);
        assert_eq!(report.fixed[0].status, FixStatus::Applied);
        let new = fs::read_to_string(root.join("a/SKILL.md")).unwrap();
        assert!(new.contains("> **Convention:** See `skills/conventions/quality.md` for Iron Law back-linking."));
        assert!(!new.contains("iron law of back-linking here"));
    }

    #[test]
    fn replace_dry_run_proposes_only() {
        let root = scratch("repl_dry");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nWe follow the iron law of back-linking here.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions { dry_run: true });
        assert_eq!(report.fixed.len(), 1);
        assert_eq!(report.fixed[0].status, FixStatus::Proposed);
        let new = fs::read_to_string(root.join("a/SKILL.md")).unwrap();
        // File untouched in dry-run.
        assert!(new.contains("iron law of back-linking here"));
        assert!(report.fixed[0].before.as_deref().unwrap().contains("iron law"));
        assert!(report.fixed[0].after.as_deref().unwrap().contains("Convention"));
    }

    #[test]
    fn replace_skipped_inside_code_fence() {
        let root = scratch("repl_fence");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\n```\nWe follow the iron law of back-linking here.\n```\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        assert!(report.fixed.is_empty());
        assert!(report
            .skipped
            .iter()
            .any(|o| o.reason == Some(SkipReason::InsideCodeFence)));
    }

    #[test]
    fn replace_skipped_ambiguous_multiple_matches() {
        let root = scratch("repl_ambig");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nWe follow the iron law of back-linking here.\n\nAlso the iron law of back-linking is important.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        assert!(report.fixed.is_empty());
        assert!(report
            .skipped
            .iter()
            .any(|o| o.reason == Some(SkipReason::AmbiguousMultipleMatches)));
    }

    #[test]
    fn replace_skipped_already_delegated() {
        let root = scratch("repl_del");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nWe follow the iron law of back-linking here. See `skills/conventions/quality.md` for details.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        assert!(report.fixed.is_empty());
        assert!(report
            .skipped
            .iter()
            .any(|o| o.reason == Some(SkipReason::AlreadyDelegated)));
    }

    #[test]
    fn replace_bullet_expands_and_applies() {
        let root = scratch("repl_bullet");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\n- We follow the iron law of back-linking here.\n- Another unrelated bullet.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        assert_eq!(report.fixed.len(), 1, "outcomes: {:?}", report);
        assert_eq!(report.fixed[0].status, FixStatus::Applied);
        let new = fs::read_to_string(root.join("a/SKILL.md")).unwrap();
        assert!(new.contains("> **Convention:** See `skills/conventions/quality.md` for Iron Law back-linking."));
        // The sibling bullet is preserved.
        assert!(new.contains("- Another unrelated bullet."));
    }

    // --- 1-6-5-8-3: INSERT brain-first missing-rule ---

    #[test]
    fn insert_brain_first_callout_applied() {
        let root = scratch("ins_apply");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\ntriggers:\n  - t\n---\n\nUse web_search to look things up.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        // REPLACE produces 0 (no cross-cutting match); INSERT produces 1.
        assert_eq!(report.fixed.len(), 1, "outcomes: {:?}", report);
        assert_eq!(report.fixed[0].status, FixStatus::Applied);
        assert!(report.fixed[0].pattern_label.contains("brain-first"));
        let new = fs::read_to_string(root.join("a/SKILL.md")).unwrap();
        assert!(new.contains("Convention:** see [conventions/brain-first.md]"));
    }

    #[test]
    fn insert_brain_first_dry_run_proposes_only() {
        let root = scratch("ins_dry");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nUse web_search to look things up.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions { dry_run: true });
        assert_eq!(report.fixed.len(), 1);
        assert_eq!(report.fixed[0].status, FixStatus::Proposed);
        assert!(report
            .fixed
            .iter()
            .any(|o| o.after.as_deref().unwrap_or("").contains("brain-first")));
        // File untouched in dry-run.
        let new = fs::read_to_string(root.join("a/SKILL.md")).unwrap();
        assert!(!new.contains("brain-first.md"));
    }

    #[test]
    fn insert_skipped_when_already_compliant() {
        // Skill already carries the canonical callout -> detect() is false ->
        // attempt_insert_fix returns None (silently, no outcome recorded).
        let root = scratch("ins_ok");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\n> **Convention:** see [conventions/brain-first.md](../conventions/brain-first.md) for the lookup chain.\n\nUse web_search here.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        assert!(report.fixed.is_empty(), "outcomes: {:?}", report);
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn insert_skipped_when_no_external_pattern() {
        // No external-lookup pattern in body -> exempt, no INSERT.
        let root = scratch("ins_noext");
        git_repo_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nJust does local stuff.\n",
        );
        let report = auto_fix_dry_violations(&root, &AutoFixOptions::default());
        assert!(report.fixed.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn insert_skipped_dirty_working_tree() {
        // Working tree dirty -> INSERT refused (git-is-backup contract).
        let dir = scratch("ins_dirty");
        fs::create_dir_all(dir.join("a")).unwrap();
        let p = dir.join("a/SKILL.md");
        fs::write(
            &p,
            "---\nname: a\n---\n\nUse web_search to look things up.\n",
        )
        .unwrap();
        // No git init -> not_a_repo would also skip, but here we exercise the
        // dirty path: init + commit, then modify without committing.
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git available")
        };
        run(&["init", "-q"]);
        run(&["add", "a/SKILL.md"]);
        run(&[
            "-c", "user.email=test@test", "-c", "user.name=test", "commit", "-q", "-m", "c",
        ]);
        fs::write(
            &p,
            "---\nname: a\n---\n\nUse web_search to look things up, edited.\n",
        )
        .unwrap();
        let report = auto_fix_dry_violations(&dir, &AutoFixOptions::default());
        assert!(report.fixed.is_empty());
        assert!(report
            .skipped
            .iter()
            .any(|o| o.reason == Some(SkipReason::WorkingTreeDirty)));
    }

    #[test]
    fn working_tree_clean_dirty_not_a_repo() {
        let dir = scratch("gatest");
        let f = git_repo_with_committed_file(&dir, "SKILL.md", "---\nname: a\n---\nbody\n");
        assert_eq!(get_working_tree_status(&f), WorkingTreeStatus::Clean);

        // Dirty: modify without committing.
        fs::write(&f, "---\nname: a\n---\nbody changed\n").unwrap();
        assert_eq!(get_working_tree_status(&f), WorkingTreeStatus::Dirty);

        // Not a repo: temp dir with no .git.
        let dir2 = scratch("gatest_norepo");
        let f2 = dir2.join("SKILL.md");
        fs::write(&f2, "x").unwrap();
        assert_eq!(get_working_tree_status(&f2), WorkingTreeStatus::NotARepo);
    }
}
