//! `zbrain skillify check` — post-task audit (ported from
//! `src/commands/skillify-check.ts`, the 11/12-item checklist).
//!
//! Pure-ish filesystem analysis plus two delegations to already-migrated
//! Rust analyzers:
//!   - item 8  `check-resolvable`  → `skill_resolver::check_resolvable`
//!   - item 12 `brain-first`       → `skill_resolver::brain_first::analyze_skill_brain_first`
//!   - item 11 `cross-modal eval`  → `super::receipt` (informational)
//!
//! Items 1-7, 9, 10 are pure filesystem/existence checks reimplemented here.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::paths::zbrain_home;
use crate::skill_resolver::brain_first::{
    analyze_skill_brain_first, build_brain_first_fix_hint, build_brain_first_summary_line,
};
use crate::skill_resolver::check_resolvable::check_resolvable;
use crate::skill_resolver::repo_root::auto_detect_skills_dir;
use crate::skill_resolver::skill_frontmatter::parse_skill_frontmatter;

use super::receipt::{describe_receipt_status, find_receipt_for_skill};

/// One audit item.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckItem {
    pub name: String,
    pub passed: bool,
    pub required: bool,
    pub detail: Option<String>,
}

/// Result for one audited target.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckResult {
    pub path: String,
    pub skill_name: String,
    pub items: Vec<CheckItem>,
    pub score: usize,
    pub total: usize,
    pub recommendation: String,
}

fn check(name: &str, passed: bool, detail: Option<String>) -> CheckItem {
    CheckItem {
        name: name.to_string(),
        passed,
        required: true,
        detail,
    }
}

fn check_optional(name: &str, passed: bool, detail: Option<String>) -> CheckItem {
    CheckItem {
        name: name.to_string(),
        passed,
        required: false,
        detail,
    }
}

const CODE_EXTS: &[&str] = &["ts", "mjs", "js", "py"];

fn strip_code_ext(name: &str) -> String {
    for ext in CODE_EXTS {
        if let Some(stripped) = name.strip_suffix(&format!(".{ext}")) {
            return stripped.to_string();
        }
    }
    name.to_string()
}

/// Owned base file name of a target path (no extension), safe to borrow.
fn target_base_name(target: &Path) -> String {
    target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Infer the skill name from a target script path: prefer a `skills/<name>/`
/// segment in the path, else fall back to the basename (minus code ext) and
/// try to match a sibling directory under `skills_dir` (trimming common
/// suffixes like `-scraper`, `-monitor`, ...).
pub fn infer_skill_name(target: &Path, skills_dir: &Path) -> String {
    let path_str = target.to_string_lossy().replace('\\', "/");
    let re = Regex::new(r"skills/([^/]+)/").unwrap();
    if let Some(cap) = re.captures(&path_str) {
        if let Some(m) = cap.get(1) {
            return m.as_str().to_string();
        }
    }
    let base = strip_code_ext(&target_base_name(target));
    if skills_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(skills_dir) {
            for e in entries.flatten() {
                let d = e.file_name().to_string_lossy().to_string();
                if d == base {
                    return d;
                }
                let normalized = strip_suffixes(&base);
                let d_flat = d.replace('-', "");
                let n_flat = normalized.replace(['-', '_'], "");
                if d == normalized || d_flat == n_flat {
                    return d;
                }
            }
        }
    }
    base
}

/// Trim trailing `-`/`_`-joined role suffixes (scraper, monitor, check, ...).
fn strip_suffixes(s: &str) -> String {
    let suffixes = ["scraper", "monitor", "check", "poll", "sync", "ingest", "core"];
    let mut out = s.to_string();
    loop {
        let mut changed = false;
        for suf in suffixes {
            for sep in ['-', '_'] {
                let pat = format!("{sep}{suf}");
                if let Some(stripped) = out.strip_suffix(&pat) {
                    if !stripped.is_empty() {
                        out = stripped.to_string();
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// Locate the test directory under `root`, preferring `tests/unit`.
fn detect_test_dir(root: &Path) -> Option<PathBuf> {
    for cand in ["tests/unit", "__tests__", "tests", "spec"] {
        let p = root.join(cand);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// Find test files related to `target` within `test_dir`.
pub fn find_related_tests(target: &Path, test_dir: &Path) -> Vec<PathBuf> {
    let base = strip_code_ext(&target_base_name(target));
    let base_under = base.replace('-', "_");
    let patterns = [
        format!("{base}.test.ts"),
        format!("{base}.test.mjs"),
        format!("{base}.test.js"),
        format!("test-{base}.ts"),
        format!("{base_under}.test.ts"),
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for p in patterns {
        let f = test_dir.join(&p);
        if f.is_file() {
            out.push(f);
        }
    }
    if let Ok(entries) = fs::read_dir(test_dir) {
        for e in entries.flatten() {
            let f = e.file_name().to_string_lossy().to_string();
            let normalized = f
                .replace('-', "")
                .replace(".test.ts", "")
                .replace(".test.mjs", "")
                .replace("test-", "")
                .to_lowercase();
            let nbase = base.replace('-', "").to_lowercase();
            if normalized.contains(&nbase) || nbase.contains(&normalized) {
                let fp = test_dir.join(&f);
                if !out.contains(&fp) {
                    out.push(fp);
                }
            }
        }
    }
    out
}

/// Whether any resolver file references the skill by path / name / file base.
fn is_in_resolver(skill_name: &str, target: &Path, skills_dir: &Path) -> bool {
    let candidates = [
        skills_dir.join("RESOLVER.md"),
        skills_dir.join("AGENTS.md"),
        skills_dir
            .parent()
            .map(|p| p.join("AGENTS.md"))
            .unwrap_or_else(|| skills_dir.join("AGENTS.md")),
    ];
    let present = candidates.iter().find(|p| p.is_file());
    let Some(present) = present else {
        return false;
    };
    let Ok(content) = fs::read_to_string(present) else {
        return false;
    };
    let base = strip_code_ext(&target_base_name(target));
    content.contains(&format!("skills/{skill_name}"))
        || content.contains(skill_name)
        || content.contains(&base)
}

/// Whether the script source writes brain pages (item 10 heuristic).
fn writes_brain(target: &Path) -> bool {
    let Ok(src) = fs::read_to_string(target) else {
        return false;
    };
    src.contains("addPage")
        || src.contains("upsertPage")
        || src.contains("addBrainPage")
        || src.contains("putPage")
}

/// Run the full audit for one target script. `skills_dir` is the resolved
/// skills directory (used for SKILL.md, resolver, trigger-eval, and
/// check-resolvable); `root` is used for the optional `brain/RESOLVER.md`
/// lookup.
pub fn run_skillify_check_target(target: &Path, skills_dir: &Path, root: &Path) -> CheckResult {
    let skill_name = infer_skill_name(target, skills_dir);
    let target_base = target_base_name(target);
    let skill_md = skills_dir.join(&skill_name).join("SKILL.md");
    let test_dir = detect_test_dir(root);

    let mut items: Vec<CheckItem> = Vec::new();

    // 1. SKILL.md exists
    items.push(check(
        "SKILL.md exists",
        skill_md.is_file(),
        Some(skill_md.to_string_lossy().to_string()),
    ));

    // 2. Code file exists
    items.push(check(
        "Code file exists",
        target.is_file(),
        Some(target.to_string_lossy().to_string()),
    ));

    // 3. Unit tests (required — matched by find_related_tests)
    let unit_tests = test_dir
        .as_ref()
        .map(|td| find_related_tests(target, td))
        .unwrap_or_default();
    let no_test_detail = test_dir
        .as_ref()
        .map(|t| t.to_string_lossy().to_string())
        .unwrap_or_else(|| "(no test dir)".to_string());
    items.push(check(
        "Unit tests",
        !unit_tests.is_empty(),
        Some(
            unit_tests
                .first()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| format!("no matching *.test.* in {}", no_test_detail)),
        ),
    ));

    // 4. Integration tests (E2E) — optional
    let e2e_dir = test_dir.as_ref().map(|td| td.join("e2e"));
    let has_e2e = e2e_dir
        .as_ref()
        .map(|d| {
            d.is_dir()
                && fs::read_dir(d).map_or(false, |ents| {
                    ents.flatten().any(|e| {
                        let f = e.file_name().to_string_lossy().to_string();
                        f.contains(&skill_name)
                            || f.contains(&strip_code_ext(&target_base))
                    })
                })
        })
        .unwrap_or(false);
    items.push(check_optional(
        "Integration tests (E2E)",
        has_e2e,
        e2e_dir
            .as_ref()
            .map(|d| d.to_string_lossy().to_string())
            .or_else(|| Some("no e2e dir".to_string())),
    ));

    // 5. LLM evals — optional
    let has_evals = test_dir.as_ref().map_or(false, |td| {
        fs::read_dir(td).map_or(false, |ents| {
            ents.flatten().any(|e| {
                let f = e.file_name().to_string_lossy().to_string();
                f.contains("eval")
                    && (f.contains(&skill_name) || f.contains(&strip_code_ext(&target_base)))
            })
        })
    });
    items.push(check_optional("LLM evals", has_evals, None));

    // 6. Resolver entry — required
    items.push(check(
        "Resolver entry",
        is_in_resolver(&skill_name, target, skills_dir),
        None,
    ));

    // 7. Resolver trigger eval — optional
    let has_trigger_eval = test_dir.as_ref().map_or(false, |td| {
        let resolver_test = td.join("resolver.test.ts");
        let mut found = resolver_test.is_file()
            && fs::read_to_string(&resolver_test)
                .map_or(false, |c| c.contains(&skill_name));
        if !found {
            let routing = skills_dir.join(&skill_name).join("routing-eval.jsonl");
            found = routing.is_file();
        }
        found
    });
    items.push(check_optional(
        "Resolver trigger eval",
        has_trigger_eval,
        None,
    ));

    // 8. check-resolvable gate — optional delegation
    let resolver_report = check_resolvable(skills_dir);
    let (cr_ok, cr_detail) = if resolver_report.ok {
        (true, "all skill-tree checks pass".to_string())
    } else {
        let count = resolver_report.errors.len() + resolver_report.warnings.len();
        (
            false,
            format!("{} issue(s) — run: zbrain check-resolvable", count),
        )
    };
    items.push(check_optional(
        "check-resolvable gate",
        cr_ok,
        Some(cr_detail),
    ));

    // 9. E2E test (required copy of #4)
    items.push(check(
        "E2E test (either under e2e/ or integration test)",
        has_e2e,
        Some("try /qa or tests/unit/e2e/".to_string()),
    ));

    // 10. Brain filing — optional (only when the script writes brain pages)
    let wb = writes_brain(target);
    let brain_resolver = root.join("brain").join("RESOLVER.md");
    let has_brain_entry = wb
        && brain_resolver.is_file()
        && fs::read_to_string(&brain_resolver)
            .map_or(false, |c| c.contains(&skill_name));
    items.push(check_optional(
        "Brain filing (RESOLVER entry for brain writes)",
        !wb || has_brain_entry,
        Some(if wb {
            if has_brain_entry {
                "entry present".to_string()
            } else {
                "writes brain but no brain/RESOLVER.md entry".to_string()
            }
        } else {
            "n/a".to_string()
        }),
    ));

    // 11. Cross-modal eval receipt — informational optional
    let cm_detail;
    let cm_passed;
    match zbrain_home() {
        Some(home) => {
            let receipt_dir = home.join("eval-receipts");
            let status = find_receipt_for_skill(&skill_md, &receipt_dir);
            cm_detail = describe_receipt_status(&skill_name, &status);
            cm_passed = matches!(status, super::receipt::ReceiptStatus::Found { .. });
        }
        None => {
            cm_detail = "no ZBRAIN_HOME — skipping cross-modal eval check".to_string();
            cm_passed = true;
        }
    }
    items.push(check_optional(
        "Cross-modal eval (informational)",
        cm_passed,
        Some(cm_detail),
    ));

    // 12. Brain-first compliance — REQUIRED delegation
    let bf_detail;
    let bf_passed;
    if !skill_md.is_file() {
        bf_detail = "brain-first check skipped: SKILL.md missing".to_string();
        bf_passed = true;
    } else {
        match fs::read_to_string(&skill_md) {
            Ok(content) => {
                let fm = parse_skill_frontmatter(&content);
                let analysis = analyze_skill_brain_first(&content, &skill_name, fm.as_ref());
                match analysis.status {
                    crate::skill_resolver::brain_first::BrainFirstStatus::Ok => {
                        bf_passed = true;
                        bf_detail = format!(
                            "{} ({})",
                            build_brain_first_summary_line(&analysis),
                            skill_name
                        );
                    }
                    crate::skill_resolver::brain_first::BrainFirstStatus::Warn => {
                        bf_passed = false;
                        bf_detail = format!(
                            "{} — {}",
                            build_brain_first_summary_line(&analysis),
                            build_brain_first_fix_hint()
                        );
                    }
                }
            }
            Err(_) => {
                bf_detail = "brain-first check skipped: SKILL.md unreadable".to_string();
                bf_passed = true;
            }
        }
    }
    items.push(check("Brain-first compliance", bf_passed, Some(bf_detail)));

    let score = items.iter().filter(|i| i.passed).count();
    let total = items.len();
    let missing: Vec<String> = items
        .iter()
        .filter(|i| !i.passed && i.required)
        .map(|i| i.name.clone())
        .collect();

    let recommendation = if missing.is_empty() {
        "properly skilled".to_string()
    } else if missing.len() <= 2 {
        format!("close — create: {}", missing.join(", "))
    } else {
        format!(
            "needs skillify — run /skillify on {}; missing: {}",
            target.to_string_lossy(),
            missing.join(", ")
        )
    };

    CheckResult {
        path: target.to_string_lossy().to_string(),
        skill_name,
        items,
        score,
        total,
        recommendation,
    }
}

/// Resolve the skills directory for the audit, falling back to `<cwd>/skills`.
pub fn resolve_skills_dir(cwd: &Path) -> PathBuf {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    auto_detect_skills_dir(cwd, &env)
        .dir
        .unwrap_or_else(|| cwd.join("skills"))
}

/// Derive the project root for a skills dir (parent when named `skills`).
pub fn derive_root(skills_dir: &Path) -> PathBuf {
    if skills_dir.file_name().map_or(false, |n| n == "skills") {
        skills_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| skills_dir.to_path_buf())
    } else {
        skills_dir.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir per call.
    ///
    /// Keying only on the pid made every test in this binary share one path,
    /// and each call opened with `remove_dir_all` — so two tests running in
    /// parallel wiped each other's fixture (flaky on many-core Linux, latent
    /// on Windows). The per-call counter restores test isolation.
    fn tmp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "zb_chk_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn strip_code_ext_variants() {
        assert_eq!(strip_code_ext("foo.ts"), "foo");
        assert_eq!(strip_code_ext("foo.mjs"), "foo");
        assert_eq!(strip_code_ext("foo.py"), "foo");
        assert_eq!(strip_code_ext("foo"), "foo");
    }

    #[test]
    fn strip_suffixes_trims_roles() {
        assert_eq!(strip_suffixes("web-scraper"), "web");
        assert_eq!(strip_suffixes("price-monitor"), "price");
        assert_eq!(strip_suffixes("plain"), "plain");
    }

    #[test]
    fn infer_from_skills_segment() {
        let s = infer_skill_name(Path::new("skills/foo/scripts/foo.ts"), Path::new("skills"));
        assert_eq!(s, "foo");
    }

    #[test]
    fn recommendation_levels() {
        let root = tmp_root();
        let skills = root.join("skills");
        fs::create_dir_all(&skills).unwrap();
        let target = root.join("src").join("foo.ts");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "export const x = 1;").unwrap();
        // No SKILL.md, no resolver → many required items fail.
        let r = run_skillify_check_target(&target, &skills, &root);
        assert!(r.score < r.total);
        assert!(r.recommendation.contains("needs skillify"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fully_compliant_skill_passes() {
        let root = tmp_root();
        let skills = root.join("skills");
        let sk = skills.join("hello");
        fs::create_dir_all(&sk).unwrap();
        // Minimal SKILL.md without external lookups → brain-first exempt.
        fs::write(
            sk.join("SKILL.md"),
            "---\nname: hello\n---\n\nLocal helper skill.\n",
        )
        .unwrap();
        let target = root.join("scripts").join("hello.ts");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "export const x = 1;").unwrap();
        // Resolver entry present.
        fs::write(skills.join("RESOLVER.md"), "skills/hello\n").unwrap();
        // A unit test exists.
        let td = root.join("tests").join("unit");
        fs::create_dir_all(&td).unwrap();
        fs::write(td.join("hello.test.ts"), "test('x', () => {});").unwrap();
        // E2E test present (required — item 9).
        let e2e = td.join("e2e");
        fs::create_dir_all(&e2e).unwrap();
        fs::write(e2e.join("hello.e2e.ts"), "test('e2e', () => {});").unwrap();
        // check-resolvable runs against skills dir with one skill (no errors).
        let r = run_skillify_check_target(&target, &skills, &root);
        assert_eq!(r.recommendation, "properly skilled", "{:?}", r.items);
        let _ = fs::remove_dir_all(&root);
    }
}
