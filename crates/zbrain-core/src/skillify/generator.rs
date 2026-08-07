//! skillify/generator — pure file-tree generator for `zbrain skillify scaffold`.
//!
//! Ported from `src/core/skillify/generator.ts`. [`plan_scaffold`] computes a
//! [`ScaffoldPlan`] (dry-run, no writes) and [`apply_scaffold`] materializes
//! it. Idempotency contract (D-CX-7): `--force` regenerates stub files but
//! NEVER re-appends a resolver row that already references this skill path.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use super::templates::ScaffoldVars;
use crate::skill_resolver::resolver_filenames::find_resolver_file;

/// The compiled skill-name pattern: `[a-z][a-z0-9]*(?:-[a-z0-9]+)*`.
///
/// Mirrors the TS `SKILL_NAME_PATTERN` RegExp. Returns a lazily-compiled,
/// shared [`Regex`] so callers can validate names with the same rule.
#[allow(non_snake_case)]
pub fn SKILL_NAME_PATTERN() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$").unwrap())
}

/// Convenience predicate: is `name` a valid lowercase-kebab-case skill name?
pub fn skill_name_is_valid(name: &str) -> bool {
    SKILL_NAME_PATTERN().is_match(name)
}

/// Errors raised while planning or applying a scaffold.
///
/// Codes mirror the TS `SkillifyScaffoldError` (`invalid_name`, `exists`,
/// `no_resolver`, `write_failed`). `no_resolver` is produced by the CLI layer
/// (after planning) when no resolver file can be located; the core planner
/// only emits `invalid_name` / `exists`, and `apply_scaffold` maps IO failures
/// to `write_failed`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SkillifyScaffoldError {
    #[error(
        "'{name}' is not a valid skill name. Must be lowercase-kebab-case (examples: webhook-verify, context-now)."
    )]
    InvalidName { name: String },

    #[error(
        "'{path}' already exists. Pass --force to regenerate stubs (destructive to any local edits), or edit the file directly."
    )]
    Exists { path: String },

    #[error(
        "RESOLVER.md or AGENTS.md not found in {skills_dir} or its parent. Create one before scaffolding skills."
    )]
    NoResolver { skills_dir: PathBuf },

    #[error("write_failed: {0}")]
    WriteFailed(String),
}

/// A single planned file write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldFile {
    pub path: PathBuf,
    pub kind: ScaffoldFileKind,
    pub content: String,
}

/// Whether a planned file is new or overwrites an existing stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldFileKind {
    New,
    Overwrite,
    Append,
}

/// A complete, previewable scaffold plan. No writes happen here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldPlan {
    pub files: Vec<ScaffoldFile>,
    /// Resolver file chosen for the append (`RESOLVER.md` or `AGENTS.md`), or
    /// `None` when no resolver exists.
    pub resolver_file: Option<PathBuf>,
    /// The resolver-row append to write, or `None` when a row for this skill
    /// already exists (idempotent — D-CX-7).
    pub resolver_append: Option<String>,
}

/// Inputs to [`plan_scaffold`].
#[derive(Debug, Clone)]
pub struct ScaffoldOptions {
    /// Absolute path to the target `skills/` dir.
    pub skills_dir: PathBuf,
    /// Scaffold variables (name, description, triggers, etc.).
    pub vars: ScaffoldVars,
    /// Repo root for `tests/unit/` and `scripts/` directories. Falls back to
    /// `skills_dir.parent()` when `None`. Callers pass explicit values in tests.
    pub repo_root: Option<PathBuf>,
    /// When true, overwrite existing skill files. Per-file (D-CX-7).
    pub force: bool,
}

/// Compute a [`ScaffoldPlan`] without performing any writes.
pub fn plan_scaffold(opts: &ScaffoldOptions) -> Result<ScaffoldPlan, SkillifyScaffoldError> {
    let vars = &opts.vars;
    if !skill_name_is_valid(&vars.name) {
        return Err(SkillifyScaffoldError::InvalidName {
            name: vars.name.clone(),
        });
    }

    let repo_root = opts
        .repo_root
        .clone()
        .or_else(|| opts.skills_dir.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| opts.skills_dir.clone());

    let skill_dir = opts.skills_dir.join(&vars.name);
    let skill_md_path = skill_dir.join("SKILL.md");
    let script_path = skill_dir.join("scripts").join(format!("{}.mjs", vars.name));
    let routing_eval_path = skill_dir.join("routing-eval.jsonl");
    let test_path = repo_root.join("tests/unit").join(format!("{}.test.ts", vars.name));

    let mut files: Vec<ScaffoldFile> = Vec::new();
    files.push(plan_want(skill_md_path, super::templates::skill_md_template(vars), opts.force)?);
    files.push(plan_want(script_path, super::templates::script_template(vars), opts.force)?);
    files.push(plan_want(
        routing_eval_path,
        super::templates::routing_eval_template(vars),
        opts.force,
    )?);
    files.push(plan_want(test_path, super::templates::test_template(vars), opts.force)?);

    // Resolver row — append to whichever file exists; `None` both fields if
    // no resolver exists (caller handles the setup error).
    let resolver_file = find_resolver_file(&opts.skills_dir)
        .or_else(|| opts.skills_dir.parent().and_then(|p| find_resolver_file(p)));

    let resolver_append = if let Some(rf) = &resolver_file {
        if detect_existing_resolver_row(rf, &vars.name) {
            None
        } else {
            Some(build_resolver_append(rf, vars))
        }
    } else {
        None
    };

    Ok(ScaffoldPlan {
        files,
        resolver_file,
        resolver_append,
    })
}

/// Decide the kind of a single planned file and surface an `Exists` error when
/// it already exists without `--force`.
fn plan_want(
    path: PathBuf,
    content: String,
    force: bool,
) -> Result<ScaffoldFile, SkillifyScaffoldError> {
    if path.exists() {
        if !force {
            return Err(SkillifyScaffoldError::Exists {
                path: path.to_string_lossy().to_string(),
            });
        }
        Ok(ScaffoldFile {
            path,
            kind: ScaffoldFileKind::Overwrite,
            content,
        })
    } else {
        Ok(ScaffoldFile {
            path,
            kind: ScaffoldFileKind::New,
            content,
        })
    }
}

/// Check whether the resolver already references `skills/<name>/SKILL.md` in
/// ANY form: backticked, single-quoted, double-quoted, or bare (surrounded by
/// non-word chars). Idempotency contract — if any form is present, we never
/// re-append a row for this skill, even with `--force`. Broader than the
/// original backtick-only match so hand-normalized resolver rows don't cause
/// duplicate appends.
pub fn detect_existing_resolver_row(resolver_file: &Path, name: &str) -> bool {
    let content = match fs::read_to_string(resolver_file) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let escaped = regex::escape(name);
    let pattern = format!(
        "(?m)(?:^|[`'\"/\\s\\(\\[])skills/{}/SKILL\\.md(?:[`'\"/\\s\\)\\]]|$)",
        escaped
    );
    match Regex::new(&pattern) {
        Ok(re) => re.is_match(&content),
        Err(_) => false,
    }
}

/// Build the resolver-append string. Appends under a `## Uncategorized`
/// section, creating it if absent.
pub fn build_resolver_append(resolver_file: &Path, vars: &ScaffoldVars) -> String {
    let content = fs::read_to_string(resolver_file).unwrap_or_default();
    let row = super::templates::resolver_row(vars);
    let has_uncategorized = Regex::new(r"(?m)^## Uncategorized\s*$")
        .unwrap()
        .is_match(&content);
    if has_uncategorized {
        return format!("\n{}\n", row);
    }
    let needs_leading_newline = if content.ends_with('\n') { "" } else { "\n" };
    format!(
        "{}\n## Uncategorized\n\n| Trigger | Skill |\n|---------|-------|\n{}\n",
        needs_leading_newline, row
    )
}

/// Apply a previously-computed [`ScaffoldPlan`]. I/O only — no planning.
pub fn apply_scaffold(plan: &ScaffoldPlan) -> Result<(), SkillifyScaffoldError> {
    for f in &plan.files {
        if let Some(parent) = f.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                SkillifyScaffoldError::WriteFailed(format!("{}: {}", f.path.display(), e))
            })?;
        }
        fs::write(&f.path, &f.content).map_err(|e| {
            SkillifyScaffoldError::WriteFailed(format!("{}: {}", f.path.display(), e))
        })?;
    }
    if let (Some(rf), Some(append)) = (&plan.resolver_file, &plan.resolver_append) {
        let current = fs::read_to_string(rf).unwrap_or_default();
        fs::write(rf, current + append).map_err(|e| {
            SkillifyScaffoldError::WriteFailed(format!("{}: {}", rf.display(), e))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::create_dir_all(root.join("tests/unit")).unwrap();
        fs::write(
            skills_dir.join("RESOLVER.md"),
            "# RESOLVER\n\n## Brain operations\n\n| Trigger | Skill |\n|---------|-------|\n| \"existing thing\" | `skills/existing/SKILL.md` |\n",
        )
        .unwrap();
        (dir, root, skills_dir)
    }

    fn opts(skills_dir: &Path, root: &Path, name: &str) -> ScaffoldOptions {
        ScaffoldOptions {
            skills_dir: skills_dir.to_path_buf(),
            vars: ScaffoldVars {
                name: name.to_string(),
                description: "demo".to_string(),
                triggers: vec![],
                writes_to: vec![],
                writes_pages: false,
                mutating: false,
            },
            repo_root: Some(root.to_path_buf()),
            force: false,
        }
    }

    #[test]
    fn name_pattern_accepts_and_rejects() {
        assert!(skill_name_is_valid("context-now"));
        assert!(skill_name_is_valid("a"));
        assert!(skill_name_is_valid("calendar-recall-v2"));
        assert!(!skill_name_is_valid("ContextNow"));
        assert!(!skill_name_is_valid("context now"));
        assert!(!skill_name_is_valid("context_now"));
        assert!(!skill_name_is_valid("2-skill"));
    }

    #[test]
    fn throws_on_invalid_name() {
        let (_d, root, skills_dir) = scratch_repo();
        let res = plan_scaffold(&opts(&skills_dir, &root, "Bad Name"));
        assert!(matches!(res, Err(SkillifyScaffoldError::InvalidName { .. })));
    }

    #[test]
    fn plans_four_files_and_resolver_append() {
        let (_d, root, skills_dir) = scratch_repo();
        let plan = plan_scaffold(&opts(&skills_dir, &root, "hello-world")).unwrap();
        assert_eq!(plan.files.len(), 4);
        let paths: Vec<&PathBuf> = plan.files.iter().map(|f| &f.path).collect();
        assert!(paths.contains(&&skills_dir.join("hello-world/SKILL.md")));
        assert!(paths.contains(&&skills_dir.join("hello-world/scripts/hello-world.mjs")));
        assert!(paths.contains(&&skills_dir.join("hello-world/routing-eval.jsonl")));
        assert!(paths.contains(&&root.join("tests/unit/hello-world.test.ts")));
        assert!(plan.files.iter().all(|f| f.kind == ScaffoldFileKind::New));
        assert_eq!(plan.resolver_file, Some(skills_dir.join("RESOLVER.md")));
        assert!(plan.resolver_append.is_some());
        assert!(plan
            .resolver_append
            .as_ref()
            .unwrap()
            .contains("`skills/hello-world/SKILL.md`"));
    }

    #[test]
    fn refuses_existing_file_without_force() {
        let (_d, root, skills_dir) = scratch_repo();
        fs::create_dir_all(skills_dir.join("existing")).unwrap();
        fs::write(
            skills_dir.join("existing/SKILL.md"),
            "---\nname: existing\n---\n",
        )
        .unwrap();
        let res = plan_scaffold(&opts(&skills_dir, &root, "existing"));
        assert!(matches!(res, Err(SkillifyScaffoldError::Exists { .. })));
    }

    #[test]
    fn force_marks_existing_as_overwrite() {
        let (_d, root, skills_dir) = scratch_repo();
        fs::create_dir_all(skills_dir.join("existing")).unwrap();
        fs::write(
            skills_dir.join("existing/SKILL.md"),
            "---\nname: existing\n---\n",
        )
        .unwrap();
        let mut o = opts(&skills_dir, &root, "existing");
        o.vars.triggers = vec!["foo".to_string()];
        o.force = true;
        let plan = plan_scaffold(&o).unwrap();
        let skill_md = plan.files.iter().find(|f| f.path.ends_with("SKILL.md")).unwrap();
        assert_eq!(skill_md.kind, ScaffoldFileKind::Overwrite);
    }

    #[test]
    fn dcx7_resolver_append_null_when_row_present() {
        let (_d, root, skills_dir) = scratch_repo();
        let rp = skills_dir.join("RESOLVER.md");
        let before = fs::read_to_string(&rp).unwrap();
        fs::write(
            &rp,
            format!(
                "{}\n## Uncategorized\n\n| Trigger | Skill |\n|---------|-------|\n| \"do thing\" | `skills/demo/SKILL.md` |\n",
                before
            ),
        )
        .unwrap();
        let mut o = opts(&skills_dir, &root, "demo");
        o.vars.triggers = vec!["do thing".to_string()];
        let plan = plan_scaffold(&o).unwrap();
        assert!(plan.resolver_append.is_none());
    }

    #[test]
    fn detects_bare_path_resolver_row() {
        let (_d, root, skills_dir) = scratch_repo();
        let rp = skills_dir.join("RESOLVER.md");
        let before = fs::read_to_string(&rp).unwrap();
        fs::write(
            &rp,
            format!(
                "{}\n## Uncategorized\n\n| Trigger | Skill |\n|---------|-------|\n| \"do thing\" | skills/demo/SKILL.md |\n",
                before
            ),
        )
        .unwrap();
        let mut o = opts(&skills_dir, &root, "demo");
        o.vars.triggers = vec!["do thing".to_string()];
        let plan = plan_scaffold(&o).unwrap();
        assert!(plan.resolver_append.is_none());
    }

    #[test]
    fn detects_double_quoted_resolver_row() {
        let (_d, root, skills_dir) = scratch_repo();
        let rp = skills_dir.join("RESOLVER.md");
        let before = fs::read_to_string(&rp).unwrap();
        fs::write(
            &rp,
            format!(
                "{}\n## Uncategorized\n\n| Trigger | Skill |\n|---------|-------|\n| \"do thing\" | \"skills/demo/SKILL.md\" |\n",
                before
            ),
        )
        .unwrap();
        let mut o = opts(&skills_dir, &root, "demo");
        o.vars.triggers = vec!["do thing".to_string()];
        let plan = plan_scaffold(&o).unwrap();
        assert!(plan.resolver_append.is_none());
    }

    #[test]
    fn detects_single_quoted_resolver_row() {
        let (_d, root, skills_dir) = scratch_repo();
        let rp = skills_dir.join("RESOLVER.md");
        let before = fs::read_to_string(&rp).unwrap();
        fs::write(
            &rp,
            format!(
                "{}\n## Uncategorized\n\n| Trigger | Skill |\n|---------|-------|\n| \"do thing\" | 'skills/demo/SKILL.md' |\n",
                before
            ),
        )
        .unwrap();
        let mut o = opts(&skills_dir, &root, "demo");
        o.vars.triggers = vec!["do thing".to_string()];
        let plan = plan_scaffold(&o).unwrap();
        assert!(plan.resolver_append.is_none());
    }

    #[test]
    fn no_false_match_for_prefix_skill() {
        let (_d, root, skills_dir) = scratch_repo();
        let rp = skills_dir.join("RESOLVER.md");
        let before = fs::read_to_string(&rp).unwrap();
        fs::write(
            &rp,
            format!(
                "{}\n## Uncategorized\n\n| Trigger | Skill |\n|---------|-------|\n| \"do extended\" | `skills/demo-extended/SKILL.md` |\n",
                before
            ),
        )
        .unwrap();
        let mut o = opts(&skills_dir, &root, "demo");
        o.vars.triggers = vec!["do thing".to_string()];
        let plan = plan_scaffold(&o).unwrap();
        assert!(plan.resolver_append.is_some());
    }

    #[test]
    fn apply_writes_files_and_appends_resolver() {
        let (_d, root, skills_dir) = scratch_repo();
        let mut o = opts(&skills_dir, &root, "hello");
        o.vars.triggers = vec!["say hi".to_string()];
        let plan = plan_scaffold(&o).unwrap();
        apply_scaffold(&plan).unwrap();
        for f in &plan.files {
            assert!(f.path.exists(), "expected {} to exist", f.path.display());
        }
        let resolver = fs::read_to_string(skills_dir.join("RESOLVER.md")).unwrap();
        assert!(resolver.contains("`skills/hello/SKILL.md`"));
    }

    #[test]
    fn apply_twice_with_force_no_duplicate_resolver() {
        let (_d, root, skills_dir) = scratch_repo();
        let base = opts(&skills_dir, &root, "idem");
        let mut first = base.clone();
        first.vars.triggers = vec!["t".to_string()];
        let first_plan = plan_scaffold(&first).unwrap();
        apply_scaffold(&first_plan).unwrap();

        let mut second = base;
        second.force = true;
        second.vars.triggers = vec!["t".to_string()];
        second.vars.description = "second".to_string();
        let second_plan = plan_scaffold(&second).unwrap();
        assert!(second_plan.resolver_append.is_none());
        apply_scaffold(&second_plan).unwrap();

        let resolver = fs::read_to_string(skills_dir.join("RESOLVER.md")).unwrap();
        let count = resolver.matches("`skills/idem/SKILL.md`").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn applies_against_agents_md_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let skills_dir = root.join("workspace/skills");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::create_dir_all(root.join("tests/unit")).unwrap();
        fs::write(
            root.join("workspace/AGENTS.md"),
            "# AGENTS\n\n## Ops\n\n| Trigger | Skill |\n|---------|-------|\n",
        )
        .unwrap();
        let mut o = opts(&skills_dir, &root, "openclaw-demo");
        o.vars.triggers = vec!["do it".to_string()];
        let plan = plan_scaffold(&o).unwrap();
        assert_eq!(plan.resolver_file, Some(root.join("workspace/AGENTS.md")));
        assert!(plan.resolver_append.is_some());
        apply_scaffold(&plan).unwrap();
        let agents = fs::read_to_string(root.join("workspace/AGENTS.md")).unwrap();
        assert!(agents.contains("`skills/openclaw-demo/SKILL.md`"));
    }
}
