//! skillify CLI subcommands — `scaffold` (real) and `check` (real).
//!
//! Both halves ported from the original TS:
//!   - `scaffold`  ← `src/commands/skillify.ts` scaffold branch
//!   - `check`     ← `src/commands/skillify-check.ts` (12-item audit), now
//!                   delegated to `zbrain_core::skillify::check`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use clap::Parser;

use zbrain_core::skill_resolver::repo_root::auto_detect_skills_dir;
use zbrain_core::skill_resolver::resolver_filenames::RESOLVER_FILENAMES_LABEL;
use zbrain_core::skillify::{
    derive_root, resolve_skills_dir, run_skillify_check_target, apply_scaffold, plan_scaffold,
    CheckResult, ScaffoldFileKind, ScaffoldOptions, ScaffoldVars, SkillifyScaffoldError,
};

/// Subcommands for `zbrain skillify`.
#[derive(Debug, Clone, Parser)]
pub enum SkillifySubcommand {
    /// Create SKILL.md, script, routing-eval, test stubs and append a resolver row.
    Scaffold(ScaffoldOptionsCli),

    /// Run the 12-item skillify audit (post-task). Delegates to zbrain_core::skillify::check.
    Check(CheckOptionsCli),
}

#[derive(Debug, Clone, Parser)]
pub struct ScaffoldOptionsCli {
    /// Skill slug (lowercase kebab-case).
    pub name: String,
    /// One-liner for SKILL.md frontmatter (required).
    #[arg(long)]
    pub description: String,
    /// Trigger phrases (comma-separated; defaults to TBD).
    #[arg(long)]
    pub triggers: Option<String>,
    /// Brain dirs this skill will write to (comma-separated).
    #[arg(long = "writes-to")]
    pub writes_to: Option<String>,
    /// Mark the skill as a brain-page writer.
    #[arg(long)]
    pub writes_pages: bool,
    /// Mark the skill as mutating: true.
    #[arg(long)]
    pub mutating: bool,
    /// Overwrite existing stubs (not resolver rows).
    #[arg(long)]
    pub force: bool,
    /// Print the plan; no writes.
    #[arg(long)]
    pub dry_run: bool,
    /// Machine-readable plan envelope.
    #[arg(long)]
    pub json: bool,
    /// Override auto-detected skills/ directory.
    #[arg(long)]
    pub skills_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct CheckOptionsCli {
    /// Path to the skill script to audit.
    pub path: Option<PathBuf>,
    /// Audit recently-changed skills instead of a path.
    #[arg(long)]
    pub recent: bool,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

/// Main entry point for `zbrain skillify` subcommands.
pub async fn run_skillify(cmd: SkillifySubcommand) -> Result<()> {
    match cmd {
        SkillifySubcommand::Scaffold(opts) => run_skillify_scaffold(opts).await,
        SkillifySubcommand::Check(opts) => run_skillify_check(opts).await,
    }
}

fn split_list(v: &Option<String>) -> Vec<String> {
    match v {
        Some(s) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

fn kind_str(k: ScaffoldFileKind) -> &'static str {
    match k {
        ScaffoldFileKind::New => "new",
        ScaffoldFileKind::Overwrite => "overwrite",
        ScaffoldFileKind::Append => "append",
    }
}

fn error_code(e: &SkillifyScaffoldError) -> &'static str {
    match e {
        SkillifyScaffoldError::InvalidName { .. } => "invalid_name",
        SkillifyScaffoldError::Exists { .. } => "exists",
        SkillifyScaffoldError::NoResolver { .. } => "no_resolver",
        SkillifyScaffoldError::WriteFailed(_) => "write_failed",
    }
}

async fn run_skillify_scaffold(opts: ScaffoldOptionsCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();

    // Resolve skills directory.
    let skills_dir = if let Some(p) = &opts.skills_dir {
        if p.is_absolute() {
            p.clone()
        } else {
            cwd.join(p)
        }
    } else {
        match auto_detect_skills_dir(&cwd, &env).dir {
            Some(d) => d,
            None => {
                return Err(anyhow!(
                    "could not auto-detect skills/. Pass --skills-dir or set $OPENCLAW_WORKSPACE."
                ));
            }
        }
    };

    let scaffold_opts = ScaffoldOptions {
        skills_dir: skills_dir.clone(),
        vars: ScaffoldVars {
            name: opts.name.clone(),
            description: opts.description.clone(),
            triggers: split_list(&opts.triggers),
            writes_to: split_list(&opts.writes_to),
            writes_pages: opts.writes_pages,
            mutating: opts.mutating,
        },
        repo_root: None,
        force: opts.force,
    };

    let plan = match plan_scaffold(&scaffold_opts) {
        Ok(p) => p,
        Err(e) => {
            if opts.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": false,
                        "error": error_code(&e),
                        "message": e.to_string(),
                    }))?
                );
            } else {
                eprintln!("skillify scaffold: {}", e);
            }
            std::process::exit(1);
        }
    };

    if plan.resolver_file.is_none() {
        let msg = format!(
            "{} not found in {} or its parent. Create one before scaffolding skills.",
            RESOLVER_FILENAMES_LABEL,
            skills_dir.display()
        );
        if opts.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": false,
                    "error": "no_resolver",
                    "message": msg,
                }))?
            );
        } else {
            eprintln!("Error: {}", msg);
        }
        std::process::exit(2);
    }

    if opts.dry_run {
        if opts.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "dryRun": true,
                    "files": plan.files.iter().map(|f| serde_json::json!({
                        "path": f.path.display().to_string(),
                        "kind": kind_str(f.kind),
                    })).collect::<Vec<_>>(),
                    "resolverFile": plan.resolver_file.as_ref().map(|p| p.display().to_string()),
                    "resolverAppendBytes": plan.resolver_append.as_ref().map(|s| s.len()).unwrap_or(0),
                }))?
            );
        } else {
            println!(
                "skillify scaffold --dry-run ({} files):",
                plan.files.len()
            );
            for f in &plan.files {
                println!("  [{}] {}", kind_str(f.kind), f.path.display());
            }
            match &plan.resolver_append {
                Some(a) => println!(
                    "  [append] {} (+{} bytes)",
                    plan.resolver_file.as_ref().unwrap().display(),
                    a.len()
                ),
                None => println!(
                    "  [skip] {} (row already present — idempotent)",
                    plan.resolver_file.as_ref().unwrap().display()
                ),
            }
        }
        std::process::exit(0);
    }

    apply_scaffold(&plan)?;

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "dryRun": false,
                "files": plan.files.iter().map(|f| serde_json::json!({
                    "path": f.path.display().to_string(),
                    "kind": kind_str(f.kind),
                })).collect::<Vec<_>>(),
                "resolverFile": plan.resolver_file.as_ref().map(|p| p.display().to_string()),
                "resolverAppended": plan.resolver_append.is_some(),
            }))?
        );
    } else {
        println!("skillify scaffold: wrote {} files.", plan.files.len());
        for f in &plan.files {
            println!("  [{}] {}", kind_str(f.kind), f.path.display());
        }
        if plan.resolver_append.is_some() {
            println!("  [append] {}", plan.resolver_file.as_ref().unwrap().display());
        }
        println!("\nNext:");
        println!("  1. Replace SKILLIFY_STUB sentinels in the generated files.");
        println!("  2. bun test tests/unit/{}.test.ts", opts.name);
        println!(
            "  3. zbrain skillify check skills/{}/scripts/{}.mjs",
            opts.name, opts.name
        );
        println!("  4. zbrain check-resolvable");
    }

    Ok(())
}

const CHECK_HELP: &str = "zbrain skillify check [path] [--recent] [--json]

Run the 12-item skillify audit (post-task). Reports whether each item
passes and what to create next.

Arguments:
  path            Path to the script/file to audit.
  --recent        Audit all files modified in the last 7 days.
  --json          Emit JSON.
  -h, --help      Show this message.

Exit code 0 when all REQUIRED items pass; 1 otherwise.
";

async fn run_skillify_check(opts: CheckOptionsCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    if opts.path.is_none() && !opts.recent {
        println!("{CHECK_HELP}");
        std::process::exit(1);
    }

    let skills_dir = resolve_skills_dir(&cwd);
    let root = derive_root(&skills_dir);

    let targets: Vec<PathBuf> = if opts.recent {
        recently_modified(&root, 7)
    } else {
        match &opts.path {
            Some(p) => vec![if p.is_absolute() { p.clone() } else { cwd.join(p) }],
            None => vec![],
        }
    };
    if targets.is_empty() {
        eprintln!("No targets. Pass a path or --recent.");
        std::process::exit(1);
    }

    let results: Vec<CheckResult> = targets
        .iter()
        .map(|t| run_skillify_check_target(t, &skills_dir, &root))
        .collect();

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        for r in &results {
            println!(
                "\n{}  [{}]  {}/{}",
                r.path, r.skill_name, r.score, r.total
            );
            for item in &r.items {
                let mark = if item.passed {
                    '✓'
                } else if item.required {
                    '✗'
                } else {
                    '·'
                };
                let tag = if item.required {
                    String::new()
                } else {
                    " (optional)".to_string()
                };
                let detail = item
                    .detail
                    .as_ref()
                    .map(|d| format!("  — {d}"))
                    .unwrap_or_default();
                println!("  {mark} {}{}{}", item.name, tag, detail);
            }
            println!("  → {}", r.recommendation);
        }
    }

    let any_failed = results
        .iter()
        .any(|r| r.items.iter().any(|i| !i.passed && i.required));
    std::process::exit(if any_failed { 1 } else { 0 });
}

/// Flat scan of `src/commands`, `src/core`, `scripts` for recently-modified
/// code files (mirrors TS `recentlyModified`).
fn recently_modified(root: &Path, days: i64) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let cutoff = SystemTime::now() - Duration::from_secs((days * 24 * 3600) as u64);
    const RECENT_EXTS: &[&str] = &["ts", "mjs", "js", "py"];
    for sub in ["src/commands", "src/core", "scripts"] {
        let dir = root.join(sub);
        if !dir.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let fp = e.path();
                if !fp.is_file() {
                    continue;
                }
                let ext = fp.extension().and_then(|s| s.to_str()).unwrap_or("");
                if !RECENT_EXTS.iter().any(|x| x == &ext) {
                    continue;
                }
                if let Ok(meta) = e.metadata() {
                    if let Ok(mt) = meta.modified() {
                        if mt >= cutoff {
                            out.push(fp);
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn skillify_subcommand_definition_is_valid() {
        SkillifySubcommand::command().debug_assert();
    }

    #[test]
    fn scaffold_verb_parses_with_flags() {
        assert!(SkillifySubcommand::try_parse_from([
            "skillify", "scaffold", "my-skill", "--description", "does a thing",
            "--triggers", "a,b", "--writes-to", "people/,companies/", "--writes-pages",
            "--mutating", "--force", "--json"
        ])
        .is_ok());
        // missing --description must fail (clap-required).
        assert!(SkillifySubcommand::try_parse_from(["skillify", "scaffold", "my-skill"]).is_err());
    }

    #[test]
    fn check_verb_parses() {
        assert!(SkillifySubcommand::try_parse_from(["skillify", "check", "scripts/foo.mjs"]).is_ok());
        assert!(SkillifySubcommand::try_parse_from(["skillify", "check", "--recent"]).is_ok());
        assert!(SkillifySubcommand::try_parse_from(["skillify", "check", "scripts/foo.mjs", "--json"]).is_ok());
    }
}
