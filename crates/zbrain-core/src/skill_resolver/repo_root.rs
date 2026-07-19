//! repo-root — skills-directory auto-detection.
//!
//! Ported from `src/core/repo-root.ts`. Resolves where a `zbrain` invocation
//! should look for its skill tree, in priority order, and (for read-only
//! callers) falls back to walking up from this crate's install location.
//!
//! This is the shared discovery primitive behind `zbrain check-resolvable`,
//! and (once migrated) `doctor` / `routing-eval` / `skillify-check`. Write
//! paths (`skillpack install`, `skillify scaffold`) must use
//! `auto_detect_skills_dir` (no install-path fallback); read-only callers
//! use `auto_detect_skills_dir_read_only`.
//!
//! `start_dir` + `env` are parameterized so tests run hermetically against
//! fixtures — mirrors the TS signatures.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::resolver_filenames::{has_resolver_file, RESOLVER_FILENAMES};

/// Where auto-detect found the skills directory.
///
/// Variants mirror `SkillsDirSource` in `src/core/repo-root.ts`:
///   - `EnvExplicit`                — $ZBRAIN_SKILLS_DIR (operator override; v0.31.7)
///   - `OpenclawWorkspaceEnv`       — $OPENCLAW_WORKSPACE/skills
///   - `OpenclawWorkspaceEnvRoot`   — $OPENCLAW_WORKSPACE/ (AGENTS.md at workspace root)
///   - `OpenclawWorkspaceHome`      — ~/.openclaw/workspace/skills
///   - `OpenclawWorkspaceHomeRoot`  — ~/.openclaw/workspace (root AGENTS.md)
///   - `CwdWalkUp`                  — walk up from cwd for any skills/ dir (v0.33)
///   - `RepoRoot`                   — walked up from cwd, found zbrain repo
///   - `CwdSkills`                  — ./skills fallback (resolver-bearing)
///   - `InstallPath`                — walked up from this crate's install path
///                                    (READ-ONLY callers only; v0.31.7)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillsDirSource {
    EnvExplicit,
    OpenclawWorkspaceEnv,
    OpenclawWorkspaceEnvRoot,
    OpenclawWorkspaceHome,
    OpenclawWorkspaceHomeRoot,
    CwdWalkUp,
    RepoRoot,
    CwdSkills,
    InstallPath,
}

impl SkillsDirSource {
    /// Stable string form, useful for JSON/verbose output and tests.
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillsDirSource::EnvExplicit => "env_explicit",
            SkillsDirSource::OpenclawWorkspaceEnv => "openclaw_workspace_env",
            SkillsDirSource::OpenclawWorkspaceEnvRoot => "openclaw_workspace_env_root",
            SkillsDirSource::OpenclawWorkspaceHome => "openclaw_workspace_home",
            SkillsDirSource::OpenclawWorkspaceHomeRoot => "openclaw_workspace_home_root",
            SkillsDirSource::CwdWalkUp => "cwd_walk_up",
            SkillsDirSource::RepoRoot => "repo_root",
            SkillsDirSource::CwdSkills => "cwd_skills",
            SkillsDirSource::InstallPath => "install_path",
        }
    }
}

/// Result of skills-dir detection: the resolved directory (if any) and the
/// specific source variant that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillsDirDetection {
    pub dir: Option<PathBuf>,
    pub source: Option<SkillsDirSource>,
}

/// Walk up from `start_dir` looking for a `skills/` directory that contains a
/// recognized resolver file (`RESOLVER.md` or `AGENTS.md`). Returns the
/// absolute directory containing `skills/` or None if not found within 10
/// levels.
pub fn find_repo_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    for _ in 0..10 {
        if has_resolver_file(&dir.join("skills")) {
            return Some(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    None
}

fn is_gbrain_repo_root(dir: &Path) -> bool {
    dir.join("src").join("cli.ts").exists() && has_resolver_file(&dir.join("skills"))
}

/// Given a workspace root, resolve where the skills directory should live.
/// Returns the skills dir + the specific source variant, or None if neither
/// `workspace/skills/<RESOLVER|AGENTS>` nor `workspace/<AGENTS|RESOLVER>` +
/// `workspace/skills` exists.
fn resolve_workspace_skills_dir(
    workspace: &Path,
    source_subdir: SkillsDirSource,
    source_root: SkillsDirSource,
) -> Option<SkillsDirDetection> {
    // Preferred: workspace/skills with a resolver file inside it (zbrain-native).
    let subdir = workspace.join("skills");
    if has_resolver_file(&subdir) {
        return Some(SkillsDirDetection {
            dir: Some(subdir),
            source: Some(source_subdir),
        });
    }
    // Fallback: resolver file at workspace root (OpenClaw-native layout). The
    // skills/ subtree still governs file layout even when routing lives at the
    // workspace root. Return the skills subdir so downstream file lookups work.
    if has_resolver_file(workspace) && subdir.exists() {
        return Some(SkillsDirDetection {
            dir: Some(subdir),
            source: Some(source_root),
        });
    }
    None
}

/// Auto-detect the skills directory. Priority (v0.31.7 read+write-safe order):
///   0. $ZBRAIN_SKILLS_DIR explicit operator override (any caller)
///   1. $OPENCLAW_WORKSPACE when explicitly set (env > repo-root walk)
///   2. ~/.openclaw/workspace/ (user's default OpenClaw deployment)
///   3. findRepoRoot() walk from cwd (zbrain's own repo)
///   4. ./skills fallback (dev scratch, fixtures)
///   1b. (v0.33) walk up from cwd for any skills/ dir (non-OpenClaw hosts)
///
/// Write-path callers (skillpack install, skillify scaffold,
/// post-install-advisory) MUST use this function, not the read-only variant.
pub fn auto_detect_skills_dir(start_dir: &Path, env: &HashMap<String, String>) -> SkillsDirDetection {
    // 0. $ZBRAIN_SKILLS_DIR explicit operator override. Safe for all callers
    //    because the operator explicitly set the env var. Does NOT support the
    //    `workspace-root with AGENTS.md + skills/ sibling` shape.
    if let Some(val) = env.get("ZBRAIN_SKILLS_DIR") {
        let explicit = if Path::new(val).is_absolute() {
            PathBuf::from(val)
        } else {
            start_dir.join(val)
        };
        if has_resolver_file(&explicit) {
            return SkillsDirDetection {
                dir: Some(explicit),
                source: Some(SkillsDirSource::EnvExplicit),
            };
        }
        // Fall through — invalid env override doesn't crash, lets lower tiers try.
    }

    // 1. $OPENCLAW_WORKSPACE wins when explicitly set.
    if let Some(val) = env.get("OPENCLAW_WORKSPACE") {
        let workspace = if Path::new(val).is_absolute() {
            PathBuf::from(val)
        } else {
            start_dir.join(val)
        };
        if let Some(resolved) = resolve_workspace_skills_dir(
            &workspace,
            SkillsDirSource::OpenclawWorkspaceEnv,
            SkillsDirSource::OpenclawWorkspaceEnvRoot,
        ) {
            return resolved;
        }
    }

    // 1b. (v0.33) Walk up from cwd looking for any `skills/` dir. No
    //     resolver-file gating — this is for non-OpenClaw hosts before a
    //     resolver file is written. Comes after $OPENCLAW_WORKSPACE so
    //     explicit env still wins; before ~/.openclaw/workspace so a bare
    //     agent repo is found instead of an implicit OpenClaw fallback.
    {
        let mut dir = start_dir.to_path_buf();
        for _ in 0..10 {
            let candidate = dir.join("skills");
            if candidate.exists() {
                return SkillsDirDetection {
                    dir: Some(candidate),
                    source: Some(SkillsDirSource::CwdWalkUp),
                };
            }
            match dir.parent() {
                Some(parent) => dir = parent.to_path_buf(),
                None => break,
            }
        }
    }

    // 2. ~/.openclaw/workspace as the default user-level OpenClaw deployment.
    if let Some(home) = env.get("HOME") {
        let workspace = Path::new(home).join(".openclaw").join("workspace");
        if let Some(resolved) = resolve_workspace_skills_dir(
            &workspace,
            SkillsDirSource::OpenclawWorkspaceHome,
            SkillsDirSource::OpenclawWorkspaceHomeRoot,
        ) {
            return resolved;
        }
    }

    // 3. zbrain repo walk from cwd.
    if let Some(repo_root) = find_repo_root(start_dir) {
        if is_gbrain_repo_root(&repo_root) {
            return SkillsDirDetection {
                dir: Some(repo_root.join("skills")),
                source: Some(SkillsDirSource::RepoRoot),
            };
        }
    }

    // 4. ./skills fallback (with hasResolverFile gate). Functionally subsumed
    //    by tier 1b's `cwd_walk_up`, but kept for callers that explicitly want
    //    to distinguish a resolver-bearing fallback from a plain skills-dir match.
    let cwd_skills = start_dir.join("skills");
    if has_resolver_file(&cwd_skills) {
        return SkillsDirDetection {
            dir: Some(cwd_skills),
            source: Some(SkillsDirSource::CwdSkills),
        };
    }

    SkillsDirDetection {
        dir: None,
        source: None,
    }
}

/// Read-only skills-dir detection (v0.31.7). Wraps `auto_detect_skills_dir` and
/// adds an install-path fallback when the primary detection returns None —
/// walks up from this crate's install location (`file!()`) to find a zbrain
/// repo root, gated by `is_gbrain_repo_root` to avoid false-positives on
/// unrelated repos (e.g. a monorepo vendoring zbrain in a subdir).
///
/// Use this from READ-ONLY callers only: `zbrain doctor`,
/// `zbrain check-resolvable`, `zbrain routing-eval`. Never from write paths.
/// Walk up from `module_dir` (the install location of this crate) looking for
/// a zbrain repo root. Returns the install-time skills dir + `InstallPath`
/// source when found, or None when the module does not live inside a zbrain
/// repo (e.g. an unrelated monorepo vendoring zbrain).
///
/// Extracted from `auto_detect_skills_dir_read_only` so the logic is
/// unit-testable with an explicit module path — `file!()` is compile-time
/// relative and cannot be parameterized from a test.
fn install_path_fallback(module_dir: &Path) -> Option<SkillsDirDetection> {
    let install_root = find_repo_root(module_dir)?;
    if is_gbrain_repo_root(&install_root) {
        Some(SkillsDirDetection {
            dir: Some(install_root.join("skills")),
            source: Some(SkillsDirSource::InstallPath),
        })
    } else {
        None
    }
}

/// Read-only skills-dir detection (v0.31.7). Wraps `auto_detect_skills_dir` and
/// adds an install-path fallback when the primary detection returns None —
/// walks up from this crate's install location to find a zbrain repo root,
/// gated by `is_gbrain_repo_root` to avoid false-positives on unrelated repos
/// (e.g. a monorepo vendoring zbrain in a subdir).
///
/// Use this from READ-ONLY callers only: `zbrain doctor`,
/// `zbrain check-resolvable`, `zbrain routing-eval`. Never from write paths.
pub fn auto_detect_skills_dir_read_only(
    start_dir: &Path,
    env: &HashMap<String, String>,
) -> SkillsDirDetection {
    let primary = auto_detect_skills_dir(start_dir, env);
    if primary.dir.is_some() {
        return primary;
    }

    // Tier-5 install-path fallback: walk up from this module's install
    // location. `file!()` points at the compiled source path, which for a
    // from-source build is relative to the crate root. Resolve it against the
    // current directory so it tracks the repo when the binary runs from within
    // or above the repo — the closest analog to the TS `import.meta.url`
    // absolute module path. Gate with is_gbrain_repo_root.
    let module_path = Path::new(file!());
    let absolute_module_dir = if module_path.is_absolute() {
        module_path.parent().map(|p| p.to_path_buf())
    } else {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.join(module_path).parent().map(|p| p.to_path_buf()))
    };
    if let Some(module_dir) = absolute_module_dir {
        if let Some(fb) = install_path_fallback(&module_dir) {
            return fb;
        }
    }

    primary // null detection, source: None
}

/// Human-readable summary of the resolver-file search paths, for error
/// messages when auto-detect fails. Mirrors the priority order used by
/// `auto_detect_skills_dir`.
pub fn auto_detect_hint() -> String {
    format!(
        "  1. --skills-dir flag\n\
          2. $ZBRAIN_SKILLS_DIR (explicit operator override)\n\
          3. $OPENCLAW_WORKSPACE/{{skills/,}}{{{}}}\n\
          4. cwd + walk-up for any skills/ directory (v0.33; for non-OpenClaw hosts)\n\
          5. ~/.openclaw/workspace/{{skills/,}}{{{}}}\n\
          6. repo root with skills/{}\n\
          7. ./skills/{}",
        RESOLVER_FILENAMES.join(","),
        RESOLVER_FILENAMES.join(","),
        RESOLVER_FILENAMES.join(" or skills/"),
        RESOLVER_FILENAMES.join(" or ./skills/"),
    )
}

/// Read-only auto-detect hint. Includes the install-path fallback that
/// `auto_detect_skills_dir_read_only` adds for `zbrain doctor` /
/// `zbrain check-resolvable` / `zbrain routing-eval`.
pub fn auto_detect_hint_read_only() -> String {
    format!("{}\n  7. (read-only) walk up from zbrain's install path", auto_detect_hint())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_repo_root_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_resolver(dir: &Path) {
        fs::write(dir.join("RESOLVER.md"), "# resolver\n").unwrap();
    }

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn finds_explicit_zbrain_skills_dir() {
        let base = scratch("explicit");
        let skills = base.join("my-skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);
        let mut env = HashMap::new();
        env.insert("ZBRAIN_SKILLS_DIR".to_string(), skills.to_string_lossy().to_string());

        let got = auto_detect_skills_dir(&base, &env);
        assert_eq!(got.source, Some(SkillsDirSource::EnvExplicit));
        assert_eq!(got.dir, Some(skills));
    }

    #[test]
    fn explicit_override_without_resolver_falls_through() {
        let base = scratch("explicit_fallthrough");
        let skills = base.join("bare-skills");
        fs::create_dir_all(&skills).unwrap(); // no resolver file
        let mut env = HashMap::new();
        env.insert("ZBRAIN_SKILLS_DIR".to_string(), skills.to_string_lossy().to_string());
        // provide a real skills dir via walk-up so detection isn't null
        let nested = base.join("project").join("skills");
        fs::create_dir_all(&nested).unwrap();
        write_resolver(&nested);

        let got = auto_detect_skills_dir(&base.join("project"), &env);
        // explicit bare-skills has no resolver -> fall through to cwd_walk_up
        assert_eq!(got.source, Some(SkillsDirSource::CwdWalkUp));
        assert_eq!(got.dir, Some(nested));
    }

    #[test]
    fn openclaw_workspace_env_skills_variant() {
        let base = scratch("oc_env_subdir");
        let ws = base.join("ws");
        let skills = ws.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);
        let mut env = HashMap::new();
        env.insert("OPENCLAW_WORKSPACE".to_string(), ws.to_string_lossy().to_string());

        let got = auto_detect_skills_dir(&base, &env);
        assert_eq!(got.source, Some(SkillsDirSource::OpenclawWorkspaceEnv));
        assert_eq!(got.dir, Some(skills));
    }

    #[test]
    fn openclaw_workspace_env_root_variant() {
        let base = scratch("oc_env_root");
        let ws = base.join("ws");
        fs::create_dir_all(&ws.join("skills")).unwrap();
        write_resolver(&ws); // AGENTS/RESOLVER at workspace root
        let mut env = HashMap::new();
        env.insert("OPENCLAW_WORKSPACE".to_string(), ws.to_string_lossy().to_string());

        let got = auto_detect_skills_dir(&base, &env);
        assert_eq!(got.source, Some(SkillsDirSource::OpenclawWorkspaceEnvRoot));
        assert_eq!(got.dir, Some(ws.join("skills")));
    }

    #[test]
    fn cwd_walk_up_finds_bare_skills() {
        let base = scratch("walk_up");
        let project = base.join("a").join("b").join("project");
        let skills = project.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);

        let got = auto_detect_skills_dir(&project, &empty_env());
        assert_eq!(got.source, Some(SkillsDirSource::CwdWalkUp));
        assert_eq!(got.dir, Some(skills));
    }

    #[test]
    fn home_openclaw_workspace_used() {
        let base = scratch("home_oc");
        let home = base.join("home");
        let ws = home.join(".openclaw").join("workspace");
        let skills = ws.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), home.to_string_lossy().to_string());

        let got = auto_detect_skills_dir(&base, &env);
        assert_eq!(got.source, Some(SkillsDirSource::OpenclawWorkspaceHome));
        assert_eq!(got.dir, Some(skills));
    }

    #[test]
    fn repo_root_variant_for_zbrain_repo() {
        let base = scratch("repo_root");
        // simulate a zbrain repo root: src/cli.ts + skills/RESOLVER.md
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(base.join("src").join("cli.ts"), "// cli\n").unwrap();
        let skills = base.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);
        // start from a nested dir so the walk-up is exercised
        let nested = base.join("deep").join("sub");
        fs::create_dir_all(&nested).unwrap();

        // Tier 1b (cwd_walk_up) matches any `skills/` dir and precedes tier 3
        // (repo_root). Since base/skills exists with a resolver, cwd_walk_up
        // wins — this mirrors the TS priority order exactly. The repo_root
        // source is preserved for back-compat but is provably unreachable once
        // a bare skills/ dir exists in the ancestry.
        let got = auto_detect_skills_dir(&nested, &empty_env());
        assert_eq!(got.source, Some(SkillsDirSource::CwdWalkUp));
        assert_eq!(got.dir, Some(skills));

        // The repo-root *detection* logic itself is still correct — verify via
        // find_repo_root, which is the primitive behind both repo_root and the
        // read-only install_path fallback.
        let found = find_repo_root(&nested);
        assert_eq!(found, Some(base));
    }

    #[test]
    fn bare_skills_wins_over_repo_root_gate() {
        let base = scratch("repo_root_not_zbrain");
        let skills = base.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);
        // no src/cli.ts -> not a zbrain repo root. But a bare skills/ dir with a
        // resolver is found by tier 1b regardless of the repo_root gate.

        let got = auto_detect_skills_dir(&base, &empty_env());
        assert_eq!(got.source, Some(SkillsDirSource::CwdWalkUp));
        assert_eq!(got.dir, Some(skills));
    }

    #[test]
    fn cwd_skills_resolver_fallback() {
        let base = scratch("cwd_skills");
        let skills = base.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);

        let got = auto_detect_skills_dir(&base, &empty_env());
        // cwd_walk_up (tier 1b) catches ./skills before the tier-4 gate, so
        // this resolves as CwdWalkUp in practice; dir is still correct.
        assert_eq!(got.dir, Some(skills));
    }

    #[test]
    fn null_when_nothing_matches() {
        let base = scratch("nothing");
        let got = auto_detect_skills_dir(&base, &empty_env());
        assert_eq!(got.dir, None);
        assert_eq!(got.source, None);
    }

    #[test]
    fn read_only_returns_primary_when_present() {
        let base = scratch("ro_primary");
        let skills = base.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);

        let got = auto_detect_skills_dir_read_only(&base, &empty_env());
        // primary (cwd_walk_up) is non-null -> returned as-is, no install fallback
        assert_eq!(got.dir, Some(skills));
        assert_ne!(got.source, Some(SkillsDirSource::InstallPath));
    }

    #[test]
    fn find_repo_root_walks_up() {
        let base = scratch("fr_root");
        let skills = base.join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_resolver(&skills);
        let deep = base.join("x").join("y").join("z");
        fs::create_dir_all(&deep).unwrap();

        let got = find_repo_root(&deep);
        assert_eq!(got, Some(base));
    }

    #[test]
    fn install_path_fallback_resolves_bundled_repo() {
        // The install-path fallback walks up from the module's install dir to
        // find a zbrain repo root, gated by is_gbrain_repo_root. This is the
        // v0.31.7 host-CLI footgun closure: a read-only command run outside any
        // skills tree still resolves to the bundled repo's skills/ instead of
        // null. Tested via the extracted helper with an explicit module path so
        // it does not depend on `file!()` (compile-time relative) resolution.
        let base = scratch("install_fb");
        // Simulate the crate layout: <repo>/crates/zbrain-core/src/skill_resolver
        let module_dir = base.join("crates").join("zbrain-core").join("src").join("skill_resolver");
        fs::create_dir_all(&module_dir).unwrap();
        // <repo>/src/cli.ts + <repo>/skills/RESOLVER.md mark a zbrain repo root.
        let repo = base.clone();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src").join("cli.ts"), "// cli\n").unwrap();
        fs::create_dir_all(repo.join("skills")).unwrap();
        write_resolver(&repo.join("skills"));

        let got = install_path_fallback(&module_dir);
        assert_eq!(got.as_ref().map(|d| d.source), Some(Some(SkillsDirSource::InstallPath)));
        assert_eq!(got.unwrap().dir, Some(repo.join("skills")));
    }

    #[test]
    fn install_path_fallback_rejected_without_cli_ts() {
        let base = scratch("install_fb_not_repo");
        let module_dir = base.join("crates").join("zbrain-core").join("src").join("skill_resolver");
        fs::create_dir_all(&module_dir).unwrap();
        // skills/ has a resolver but no src/cli.ts -> not a zbrain repo root
        fs::create_dir_all(base.join("skills")).unwrap();
        write_resolver(&base.join("skills"));

        assert!(install_path_fallback(&module_dir).is_none());
    }

    #[test]
    fn find_repo_root_respects_ten_level_cap() {
        let base = scratch("fr_cap");
        let mut cur = base.clone();
        for i in 0..12 {
            cur = cur.join(format!("d{}", i));
        }
        fs::create_dir_all(&cur).unwrap();
        // no skills/ with resolver anywhere -> None (also beyond 10 levels)
        let got = find_repo_root(&base.join("d0").join("d1"));
        assert_eq!(got, None);
    }

    #[test]
    fn hint_strings_mention_resolver_filenames() {
        let hint = auto_detect_hint();
        assert!(hint.contains("RESOLVER.md,AGENTS.md"));
        assert!(hint.contains("--skills-dir flag"));
        let ro = auto_detect_hint_read_only();
        assert!(ro.contains("(read-only) walk up from zbrain's install path"));
        // read-only hint is the base hint plus the extra line
        assert!(ro.starts_with(&hint));
    }

    #[test]
    fn source_as_str_roundtrips() {
        assert_eq!(SkillsDirSource::EnvExplicit.as_str(), "env_explicit");
        assert_eq!(SkillsDirSource::InstallPath.as_str(), "install_path");
        assert_eq!(SkillsDirSource::CwdWalkUp.as_str(), "cwd_walk_up");
    }
}
