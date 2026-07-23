/**
 * post_install_advisory.rs (v0.25.1) — agent-readable "what to do next"
 * after `zbrain init` or `zbrain upgrade`.
 *
 * zbrain users typically interact through their host agent (openclaw,
 * claude-code) rather than the zbrain CLI directly. So an interactive
 * TTY prompt at install time misses most of the audience.
 *
 * Instead: every `init` and `post-upgrade` ends by printing an advisory
 * that the agent reads from terminal output. The advisory:
 *
 *   1. Names the version that just landed.
 *   2. Lists the new skills that aren't yet installed in this workspace.
 *   3. Includes a one-line description per skill.
 *   4. Tells the agent EXPLICITLY: ask the user before installing.
 *   5. Prints the exact command to run if the user says yes.
 *
 * Detection: parse the cumulative-slugs receipt in the workspace's
 * managed block (RESOLVER.md / AGENTS.md). Any skill in the recommended
 * set that isn't in the receipt is "not yet installed."
 *
 * Recommended set: hardcoded for v0.25.1 (the 9 new skills). Future
 * releases either bump the constant or read it from the latest
 * migration file's frontmatter; for v0.25.1 the constant is the simpler
 * path.
 *
 * No-op safely:
 *   - No workspace detected → no advisory (don't fabricate paths).
 *   - All recommended skills already installed → no advisory
 *     (don't nag the agent every command).
 *   - Pre-v0.19 fence with no receipt → use the row-extracted slug set.
 */

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use textwrap::wrap;
use crate::repo_root::auto_detect_skills_dir;

/// Parsed receipt from the managed block in AGENTS.md / RESOLVER.md.
#[derive(Debug, Clone, Default)]
pub struct Receipt {
    /// Slugs of already-installed skillpacks.
    pub installed: Vec<String>,
}

/// Parse receipt content from the managed block text.
pub fn parse_receipt(content: &str) -> Option<Receipt> {
    let mut installed = Vec::new();
    let in_managed = content.lines().any(|line| line.contains("<!-- zbrain:skillpack:begin -->"));
    if !in_managed {
        return None;
    }

    // Find the managed block between the begin/end markers
    let mut in_block = false;
    for line in content.lines() {
        if line.contains("<!-- zbrain:skillpack:begin -->") {
            in_block = true;
            continue;
        }
        if line.contains("<!-- zbrain:skillpack:end -->") {
            break;
        }
        if in_block {
            // installed slug lines look like "- `slug`"
            let trimmed = line.trim();
            if trimmed.starts_with("- `") && trimmed.ends_with('`') {
                let slug = trimmed[2..trimmed.len()-1].to_string();
                installed.push(slug);
            }
        }
    }

    Some(Receipt { installed })
}

/// Extract all managed skill slugs from any managed blocks in the content.
pub fn extract_managed_slugs(content: &str) -> Vec<String> {
    let mut slugs = Vec::new();

    if let Some(receipt) = parse_receipt(content) {
        slugs.extend(receipt.installed);
    }

    slugs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecommendedSkill {
    slug: &'static str,
    description: &'static str,
}

/// v0.25.1 recommended skills shipped with this release.
const V0_25_1_RECOMMENDED: &[RecommendedSkill] = &[
    RecommendedSkill {
        slug: "book-mirror",
        description: "FLAGSHIP. Take any book (EPUB/PDF), produce a personalized two-column chapter-by-chapter analysis. Left column preserves the chapter; right column maps every idea to your life using brain context. ~$6 for a 20-chapter book at Opus.",
    },
    RecommendedSkill {
        slug: "article-enrichment",
        description: "Turn raw article dumps into structured pages with executive summary, verbatim quotes, key insights, why-it-matters.",
    },
    RecommendedSkill {
        slug: "strategic-reading",
        description: "Read a book / article / case study through ONE specific problem-lens. Output: applied playbook with do / avoid / watch-for.",
    },
    RecommendedSkill {
        slug: "concept-synthesis",
        description: "Deduplicate raw concept stubs into a tiered intellectual map (T1 Canon to T4 Riff). Trace idea evolution across years.",
    },
    RecommendedSkill {
        slug: "perplexity-research",
        description: "Brain-augmented web research. Sends brain context to Perplexity so the search focuses on what is NEW vs already-known.",
    },
    RecommendedSkill {
        slug: "archive-crawler",
        description: "Universal archivist for personal file archives (Dropbox / B2 / Gmail-takeout). REFUSES to run without a zbrain.yml allow-list — safe-by-default.",
    },
    RecommendedSkill {
        slug: "academic-verify",
        description: "Trace a research claim through publication → methodology → raw data → independent replication. Verdict-shaped brain page.",
    },
    RecommendedSkill {
        slug: "brain-pdf",
        description: "Render any brain page to publication-quality PDF via the gstack make-pdf binary. Optional gstack co-install.",
    },
    RecommendedSkill {
        slug: "voice-note-ingest",
        description: "Capture voice notes with EXACT-PHRASING preservation (never paraphrased). Routes content to originals/concepts/people/companies/ideas.",
    },
];

/// Read the managed block's cumulative-slugs receipt to find what's
/// already installed. Returns the empty set when no managed block
/// exists (fresh workspace).
pub fn detect_installed_slugs(target_skills_dir: &Path, target_workspace: &Path) -> std::collections::HashSet<String> {
    let resolver = find_resolver_file(target_skills_dir)
        .or_else(|| find_resolver_file(target_workspace));

    let Some(resolver) = resolver else {
        return std::collections::HashSet::new();
    };

    if !resolver.exists() {
        return std::collections::HashSet::new();
    }

    let content = match fs::read_to_string(resolver) {
        Ok(c) => c,
        Err(_) => return std::collections::HashSet::new(),
    };

    if let Some(receipt) = parse_receipt(&content) {
        receipt.installed.into_iter().collect()
    } else {
        extract_managed_slugs(&content).into_iter().collect()
    }
}

/// Find resolver file (RESOLVER.md) in the given directory.
fn find_resolver_file(dir: &Path) -> Option<PathBuf> {
    let candidate = dir.join("RESOLVER.md");
    if candidate.exists() {
        return Some(candidate);
    }
    let candidate = dir.join("resolver.md");
    if candidate.exists() {
        return Some(candidate);
    }
    None
}

#[derive(Debug, Clone)]
pub struct BuildAdvisoryOptions {
    pub version: String,
    pub context: AdvisoryContext,
    pub target_workspace: Option<PathBuf>,
    pub target_skills_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdvisoryContext {
    Init,
    Upgrade,
}

impl std::fmt::Display for AdvisoryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdvisoryContext::Init => write!(f, "init"),
            AdvisoryContext::Upgrade => write!(f, "upgrade"),
        }
    }
}

/// Build the post-install advisory text. Returns None when there's
/// nothing to recommend (no workspace, all recommended skills already
/// installed, etc.) — caller should skip printing entirely on null.
pub fn build_advisory(opts: BuildAdvisoryOptions) -> Option<String> {
    let BuildAdvisoryOptions { version, context, mut target_workspace, mut target_skills_dir } = opts;

    if target_skills_dir.is_none() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let env: std::collections::HashMap<String, String> = std::env::vars().collect();
        if let Some(detected) = Some(auto_detect_skills_dir(&cwd, &env)) {
            if let Some(dir) = detected.dir {
                if target_workspace.is_none() {
                    target_workspace = dir.parent().map(|p| p.to_path_buf());
                }
                target_skills_dir = Some(dir);
            }
        }
    }

    let (workspace, skills_dir) = match (&target_workspace, &target_skills_dir) {
        (Some(w), Some(s)) => (w.clone(), s.clone()),
        _ => {
            return Some(build_advisory_without_workspace(&version, context));
        }
    };

    let installed = detect_installed_slugs(&skills_dir, &workspace);
    let missing: Vec<&RecommendedSkill> = V0_25_1_RECOMMENDED
        .iter()
        .filter(|s| !installed.contains(s.slug))
        .collect();

    if missing.is_empty() {
        return None;
    }

    let install_command = if missing.len() == V0_25_1_RECOMMENDED.len() {
        "zbrain skillpack install --all".to_string()
    } else {
        let slugs: Vec<String> = missing.iter().map(|s| s.slug.to_string()).collect();
        format!("zbrain skillpack install {}", slugs.join(" "))
    };

    Some(render_advisory(&version, context, &missing, &install_command, target_workspace.is_none()))
}

fn build_advisory_without_workspace(version: &str, context: AdvisoryContext) -> String {
    let missing: Vec<&RecommendedSkill> = V0_25_1_RECOMMENDED.iter().collect();
    render_advisory(version, context, &missing, "zbrain skillpack install --all", true)
}

fn render_advisory(
    version: &str,
    context: AdvisoryContext,
    missing: &[&RecommendedSkill],
    install_command: &str,
    workspace_not_detected: bool,
) -> String {
    let mut lines = Vec::new();
    let bar = "=".repeat(72);
    let verb = match context {
        AdvisoryContext::Init => "installed",
        AdvisoryContext::Upgrade => "upgraded to",
    };

    lines.push(String::new());
    lines.push(bar.clone());
    lines.push(format!("zbrain {version} — RECOMMENDED SKILLS FOR THE AGENT TO INSTALL"));
    lines.push(bar.clone());
    lines.push(String::new());

    let plural = if missing.len() == 1 { "skill" } else { "skills" };
    lines.push(format!(
        "The user just {} zbrain {}. This release ships {} {} that the user should \
         consider installing into their workspace.",
        verb, version, missing.len(), plural
    ));
    lines.push(String::new());

    if workspace_not_detected {
        lines.push(
            "(NOTE: No OpenClaw workspace was detected at the default location. \
             Set $OPENCLAW_WORKSPACE or pass --skills-dir to zbrain skillpack install.)".to_string()
        );
        lines.push(String::new());
    }

    lines.push("THE NEW SKILLS:".to_string());
    lines.push(String::new());

    for s in missing {
        lines.push(format!("  - {}", s.slug));
        let wrapped = textwrap::wrap(&s.description, 68);
        for line in wrapped {
            lines.push(format!("      {line}"));
        }
        lines.push(String::new());
    }

    lines.push("ACTION FOR THE AGENT:".to_string());
    lines.push(String::new());
    lines.push("  1. Show this list to the user. Briefly explain each skill.".to_string());
    lines.push("  2. Ask the user explicitly: \"Want me to install these now?\"".to_string());
    lines.push("  3. If YES, run this command:".to_string());
    lines.push(String::new());
    lines.push(format!("         {install_command}"));
    lines.push(String::new());
    lines.push("  4. If NO, tell them they can install any time with:".to_string());
    lines.push(String::new());
    lines.push("         zbrain skillpack install <name>      # one skill".to_string());
    lines.push("         zbrain skillpack install --all       # all bundled".to_string());
    lines.push("         zbrain skillpack list                # see all options".to_string());
    lines.push(String::new());
    lines.push("  Do NOT install without asking. The user owns this decision.".to_string());
    lines.push(bar.clone());
    lines.push(String::new());

    lines.join("\n")
}

/// Print the advisory to stderr at the end of init / post-upgrade.
/// No-op when buildAdvisory returns None.
pub fn print_advisory_if_recommended(opts: BuildAdvisoryOptions) {
    if let Some(advisory) = build_advisory(opts) {
        eprintln!("{advisory}");
    }
}
