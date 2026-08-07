//! skillify CLI subcommands — scaffold (real), check (stub → node 1-1-1).
//!
//! Ported from `src/commands/skillify.ts`'s scaffold branch. The `check`
//! half (11-item audit) remains in TS (`src/commands/skillify-check.ts`) and
//! is tracked by roadmap node `1-1-1`.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Parser;

use zbrain_core::skill_resolver::repo_root::auto_detect_skills_dir;
use zbrain_core::skill_resolver::resolver_filenames::RESOLVER_FILENAMES_LABEL;
use zbrain_core::skillify::{
    apply_scaffold, plan_scaffold, ScaffoldFileKind, ScaffoldOptions, ScaffoldVars,
    SkillifyScaffoldError,
};

/// Subcommands for `zbrain skillify`.
#[derive(Debug, Clone, Parser)]
pub enum SkillifySubcommand {
    /// Create SKILL.md, script, routing-eval, test stubs and append a resolver row.
    Scaffold(ScaffoldOptionsCli),

    /// Run the 11-item skillify audit (tracked by roadmap node 1-1-1; still in TS).
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
}

/// Main entry point for `zbrain skillify` subcommands.
pub async fn run_skillify(cmd: SkillifySubcommand) -> Result<()> {
    match cmd {
        SkillifySubcommand::Scaffold(opts) => run_skillify_scaffold(opts).await,
        SkillifySubcommand::Check(_opts) => {
            anyhow::bail!(
                "skillify check not yet migrated to Rust — tracked by roadmap node 1-1-1. \
                 It still runs from TS (src/commands/skillify-check.ts) for now."
            );
        }
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
    let cwd = env::current_dir()?;
    let env: HashMap<String, String> = env::vars().collect();

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
        assert!(SkillifySubcommand::try_parse_from([
            "skillify", "scaffold", "my-skill"
        ])
        .is_err());
    }

    #[test]
    fn check_verb_parses() {
        assert!(SkillifySubcommand::try_parse_from(["skillify", "check", "scripts/foo.mjs"]).is_ok());
        assert!(SkillifySubcommand::try_parse_from(["skillify", "check", "--recent"]).is_ok());
    }
}
