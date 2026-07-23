/**
 * skillpack/scrub_legacy.rs — `zbrain skillpack scrub-legacy-fence-rows`
 * (TODO-2 folded into v0.33).
 *
 * Opt-in companion to `migrate-fence`. After the agent confirms it
 * walks frontmatter `triggers:` for routing, this command removes the
 * legacy table rows that `migrate-fence` left behind.
 *
 * Gate (two conditions must BOTH hold for a row to be removed):
 *   1. `skills/<slug>/` exists on host (it was a real scaffold)
 *   2. That skill's frontmatter declares a non-empty `triggers:` array
 *      (proof that frontmatter discovery covers this skill)
 *
 * Rows whose slug fails either gate are preserved — user-owned rows
 * the migration shouldn't touch.
 */

use std::fs;
use std::path::{Path, PathBuf};
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::skill_resolver::resolver_filenames::find_resolver_file;
use crate::markdown::{parse_markdown, ParsedMarkdown};

const MANAGED_BEGIN: &str = "<!-- zbrain:skillpack:begin -->";
const MANAGED_END: &str = "<!-- zbrain:skillpack:end -->";

// Row shape that migrate-fence leaves behind:
//   | "trigger phrase" | `skills/<slug>/SKILL.md` |
// Anchored to the start of a line so we don't accidentally strip
// rows the user typed differently.
lazy_static::lazy_static! {
    static ref LEGACY_ROW_RE: Regex = Regex::new(r#"^\| .*" \| `skills/([^/`]+)/SKILL\.md` \|\s*$"#).unwrap();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrubLegacyOptions {
    pub target_workspace: PathBuf,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrubLegacyResult {
    pub resolver_file: Option<PathBuf>,
    /// Slugs whose rows were removed.
    pub removed: Vec<String>,
    /// Slugs whose rows survived (skill missing OR no triggers declared).
    pub preserved: Vec<String>,
    pub dry_run: bool,
}

/// Run the legacy fence row scrub.
pub fn run_scrub_legacy(opts: ScrubLegacyOptions) -> ScrubLegacyResult {
    let dry_run = opts.dry_run;
    let skills_dir = opts.target_workspace.join("skills");
    let resolver_file = find_resolver_file(&skills_dir)
        .or_else(|| find_resolver_file(&opts.target_workspace));

    let Some(resolver_file) = resolver_file else {
        return ScrubLegacyResult {
            resolver_file: None,
            removed: Vec::new(),
            preserved: Vec::new(),
            dry_run,
        };
    };

    let content = match fs::read_to_string(&resolver_file) {
        Ok(c) => c,
        Err(_) => {
            return ScrubLegacyResult {
                resolver_file: Some(resolver_file),
                removed: Vec::new(),
                preserved: Vec::new(),
                dry_run,
            };
        }
    };

    // Determine "outside any current fence" ranges. After migrate-fence,
    // the markers should be gone — but defensively skip rows still
    // inside a fence (user might run scrub-legacy without having run
    // migrate-fence first).
    let begin_idx = content.find(MANAGED_BEGIN);
    let end_idx = content.find(MANAGED_END);
    let in_fence_range = match (begin_idx, end_idx) {
        (Some(begin), Some(end)) if end > begin => {
            Some((begin, end + MANAGED_END.len()))
        }
        _ => None,
    };

    let mut removed = Vec::new();
    let mut preserved = Vec::new();
    let mut out_lines = Vec::new();
    let mut offset = 0;

    for line in content.lines() {
        let line_start = offset;
        offset += line.len() + 1; // +1 for the newline

        let Some(captures) = LEGACY_ROW_RE.captures(line) else {
            out_lines.push(line.to_string());
            continue;
        };

        let Some(slug) = captures.get(1).map(|m| m.as_str().to_string()) else {
            out_lines.push(line.to_string());
            continue;
        };

        // Skip rows inside an existing fence (defensive).
        if let Some((start, end)) = in_fence_range {
            if line_start >= start && line_start < end {
                out_lines.push(line.to_string());
                continue;
            }
        }

        // Gate: skill dir exists AND frontmatter triggers are declared.
        if !skill_has_frontmatter_triggers(&opts.target_workspace, &slug) {
            out_lines.push(line.to_string());
            preserved.push(slug);
            continue;
        }

        // Row qualifies for removal.
        removed.push(slug);
        // (do NOT push the line — it's dropped)
    }

    if !dry_run && !removed.is_empty() {
        let _ = fs::write(&resolver_file, out_lines.join("\n"));
    }

    ScrubLegacyResult {
        resolver_file: Some(resolver_file),
        removed,
        preserved,
        dry_run,
    }
}

/// Check if the skill exists on disk and its frontmatter declares `triggers:`.
fn skill_has_frontmatter_triggers(workspace: &Path, slug: &str) -> bool {
    let skill_md = workspace.join("skills").join(slug).join("SKILL.md");
    if !skill_md.exists() {
        return false;
    }

    let raw = match fs::read_to_string(&skill_md) {
        Ok(r) => r,
        Err(_) => return false,
    };

    let skill_md_str = skill_md.to_string_lossy();
    let parsed = parse_markdown(&raw, &skill_md_str, None);

    if let Some(triggers) = parsed.frontmatter.get("triggers") {
        triggers.as_array().map_or(false, |arr| !arr.is_empty())
    } else {
        false
    }
}
