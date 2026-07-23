/**
 * skillpack/init_scaffold.rs — `zbrain skillpack init <name>` scaffold.
 *
 * Cathedral default per codex T4 + DX-Round-2: lands a complete 10/10
 * pack tree out of the box. Publisher edits or deletes what they don't
 * need; `zbrain skillpack doctor --quick` on a freshly-init'd pack
 * passes 10/10 immediately.
 *
 * `--minimal` flag drops tests/unit/, e2e/, evals/ for power users who
 * explicitly opt out.
 *
 * Refuses to overwrite any existing file — same contract as v0.36's
 * scaffold command.
 */

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::skillpack::manifest_v1::{
    SkillpackManifest, SkillpackRunbook, EVAL_SCHEMA_VERSION, RUNBOOK_SCHEMA_VERSION,
    SKILLPACK_API_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum InitScaffoldError {
    #[error("name \"{0}\" is not lowercase kebab-case (must match ^[a-z][a-z0-9-]{{1,63}}$)")]
    InvalidName(String),

    #[error("target directory exists and is not empty")]
    TargetExistsNotEmpty,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitScaffoldOptions {
    /// Target directory (created if missing). Becomes the pack root.
    pub target_dir: PathBuf,
    /// Pack name (lowercase kebab; becomes manifest.name).
    pub name: String,
    /// Skip tests/unit/, e2e/, evals/ for power users.
    #[serde(default)]
    pub minimal: bool,
    /// Optional initial skill slug (default: <pack-name>).
    pub first_skill_slug: Option<String>,
    /// Pre-fill author + license + homepage.
    pub author: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    /// Dry-run: report intent without writing.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitScaffoldResult {
    pub target_dir: PathBuf,
    pub files_written: Vec<PathBuf>,
    pub files_skipped_existing: Vec<PathBuf>,
    pub manifest: SkillpackManifest,
}

lazy_static::lazy_static! {
    static ref NAME_RE: Regex = Regex::new(r"^[a-z][a-z0-9-]{1,63}$").unwrap();
}

/// Build the cathedral scaffold tree.
pub fn run_init_scaffold(opts: InitScaffoldOptions) -> Result<InitScaffoldResult, InitScaffoldError> {
    if !NAME_RE.is_match(&opts.name) {
        return Err(InitScaffoldError::InvalidName(opts.name));
    }

    let first_slug = opts.first_skill_slug.clone().unwrap_or_else(|| opts.name.clone());
    if !NAME_RE.is_match(&first_slug) {
        return Err(InitScaffoldError::InvalidName(first_slug));
    }

    let date_iso = Utc::now().format("%Y-%m-%d").to_string();

    let mut manifest = SkillpackManifest {
        api_version: SKILLPACK_API_VERSION.to_string(),
        name: opts.name.clone(),
        version: "0.1.0".to_string(),
        description: format!("(edit me) one-line description of the {} skillpack", opts.name),
        author: opts.author.unwrap_or_else(|| "Your Name <you@example.com>".to_string()),
        license: opts.license.unwrap_or_else(|| "MIT".to_string()),
        homepage: opts.homepage.unwrap_or_else(|| format!("https://github.com/your-user/skillpack-{}", opts.name)),
        zbrain_min_version: "0.36.0".to_string(),
        runbook_schema_version: Some(RUNBOOK_SCHEMA_VERSION),
        eval_schema_version: Some(EVAL_SCHEMA_VERSION),
        skills: vec![format!("skills/{}", first_slug)],
        shared_deps: None,
        excluded_from_install: None,
        runbooks: Some(SkillpackRunbook { bootstrap: Some("runbooks/bootstrap.md".to_string()) }),
        changelog: Some("CHANGELOG.md".to_string()),
        unit_tests: None,
        e2e_tests: None,
        llm_evals: None,
        routing_evals: Some(vec![format!("skills/{}/routing-eval.jsonl", first_slug)]),
    };

    if !opts.minimal {
        manifest.unit_tests = Some(vec!["tests/unit/**/*.test.ts".to_string()]);
        manifest.e2e_tests = Some(vec!["e2e/**/*.test.ts".to_string()]);
        manifest.llm_evals = Some(vec![format!("evals/{}.judge.json", opts.name)]);
    }

    // Plan the writes.
    let mut plan: Vec<(PathBuf, String)> = Vec::new();

    let manifest_json = serde_json::to_string_pretty(&manifest)? + "\n";
    plan.push((opts.target_dir.join("skillpack.json"), manifest_json));

    let skill_md = format!(
r#"---
name: {first_slug}
description: (edit me) one-line description of what {first_slug} does
mutating: false
triggers:
  - example trigger phrase 1 for {first_slug}
  - example trigger phrase 2 for {first_slug}
---

# {first_slug}

(edit me) Markdown body describing what the skill does, what tools it uses,
and the user-facing contract. Agents read this top-to-bottom when the user
phrasing matches one of the `triggers:` above.
"#);
    plan.push((opts.target_dir.join(format!("skills/{first_slug}/SKILL.md")), skill_md));

    // 5 routing-eval intents to clear dimension 3.
    let mut routing_eval_lines = Vec::new();
    for i in 1..=5 {
        let json = serde_json::json!({
            "intent": format!("example phrase {i} for {first_slug}"),
            "expected_skill": first_slug
        });
        routing_eval_lines.push(serde_json::to_string(&json)?);
    }
    let routing_eval_content = routing_eval_lines.join("\n") + "\n";
    plan.push((opts.target_dir.join(format!("skills/{first_slug}/routing-eval.jsonl")), routing_eval_content));

    let bootstrap_content = format!(
r#"# Bootstrap

Post-scaffold steps. zbrain displays this but does NOT auto-execute.
The agent reads it and walks per-step at its own discretion.

1. show user: "{} is installed. Try one of the trigger phrases from skills/{}/SKILL.md."
2. (edit me) agent: zbrain put_page wiki/_-config --frontmatter type=config
"#, opts.name, first_slug);
    plan.push((opts.target_dir.join("runbooks/bootstrap.md"), bootstrap_content));

    let changelog_content = format!(
r#"# Changelog

All notable changes documented in Keep-a-Changelog shape.

## [0.1.0] - {date_iso}

- Initial release.
"#);
    plan.push((opts.target_dir.join("CHANGELOG.md"), changelog_content));

    let readme_content = format!(
r#"# {}

{}

## Install

```bash
zbrain skillpack scaffold your-user/skillpack-{}
```

## What it does

(edit me) Explain what the pack adds to the user's agent.

## Skills

- `skills/{}/` — (edit me) one-line description
"#, manifest.name, manifest.description, manifest.name, first_slug);
    plan.push((opts.target_dir.join("README.md"), readme_content));

    let license_content = format!(
r#"{} License

(edit me) Replace with the full license text matching the SPDX id above.
"#, manifest.license);
    plan.push((opts.target_dir.join("LICENSE"), license_content));

    let gitignore_content = "node_modules/\n.DS_Store\n*.tgz\n".to_string();
    plan.push((opts.target_dir.join(".gitignore"), gitignore_content));

    if !opts.minimal {
        let example_unit_test = r#"import { describe, test, expect } from 'bun:test';

describe('example unit test', () => {
  test('placeholder — replace with real assertions', () => {
    expect(1 + 1).toBe(2);
  });
});
"#;
        plan.push((opts.target_dir.join("tests/unit/example.test.ts"), example_unit_test.to_string()));

        let example_e2e = r#"import { describe, test, expect } from 'bun:test';

describe.skipIf(!process.env.DATABASE_URL)('example E2E test', () => {
  test('placeholder — replace with a real integration scenario', () => {
    expect(process.env.DATABASE_URL).toBeDefined();
  });
});
"#;
        plan.push((opts.target_dir.join("e2e/example.e2e.test.ts"), example_e2e.to_string()));

        let judge_json = serde_json::json!({
            "task": format!("(edit me) Describe the task this LLM-judge eval scores {} against.", opts.name),
            "output": "{{output-from-skill}}",
            "cases": [
                {"name": "happy path", "criteria": "output satisfies the task"},
                {"name": "edge case", "criteria": "output handles a corner input gracefully"},
                {"name": "failure mode", "criteria": "output refuses gracefully on ambiguous input"},
            ]
        });
        let judge_json_pretty = serde_json::to_string_pretty(&judge_json)? + "\n";
        plan.push((opts.target_dir.join(format!("evals/{}.judge.json", opts.name)), judge_json_pretty));
    }

    // Apply plan.
    let mut written = Vec::new();
    let mut skipped = Vec::new();

    for (path, content) in plan {
        if path.exists() {
            skipped.push(path);
            continue;
        }

        if !opts.dry_run {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut file = fs::File::create(&path)?;
            file.write_all(content.as_bytes())?;
        }

        written.push(path);
    }

    Ok(InitScaffoldResult {
        target_dir: opts.target_dir,
        files_written: written,
        files_skipped_existing: skipped,
        manifest,
    })
}
