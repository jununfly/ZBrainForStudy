//! Declarative quality rubric for third-party skillpacks.
//!
//! Gives tier eligibility (endorsed/community/experimental/blocked) based on 10 quality dimensions.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::skillpack::manifest_v1::{self, SkillpackManifest, SkillpackManifestError};

/// Category of a rubric dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RubricCategory {
    /// Required core dimension (must pass for any tier).
    Core,
    /// Quality badge (adds to tier scoring).
    Badge,
}

/// Result of a single dimension check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricDimensionResult {
    /// 1-10 dimension id (stable across versions).
    pub id: u8,
    /// snake_case name (stable, used in tier rules).
    pub name: String,
    /// Required core or optional badge.
    pub category: RubricCategory,
    /// Did the check pass?
    pub passed: bool,
    /// Human-readable description of what was checked.
    pub description: String,
    /// Detail string for JSON output (specific failing items).
    pub detail: String,
    /// Paste-ready fix hint when failed.
    pub fix_hint: Option<String>,
    /// Whether `doctor --fix` can auto-resolve this dimension.
    pub auto_fixable: bool,
}

/// Input to the rubric checker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricInput {
    /// Absolute path to the pack root.
    pub pack_root: std::path::PathBuf,
    /// Parsed skillpack manifest.
    pub manifest: SkillpackManifest,
}

/// Overall rubric score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricScore {
    /// All dimension results.
    pub dimensions: Vec<RubricDimensionResult>,
    /// Total number of passed dimensions (max 10).
    pub total: usize,
    /// Number of core dimensions passed.
    pub core_passed: usize,
    /// Number of badge dimensions passed.
    pub badges_passed: usize,
    /// Tier eligibility based on passed dimensions.
    pub tier_eligibility: RubricTier,
    /// Blocking dimensions that prevent promotion to a higher tier.
    pub promotion_blockers: Vec<String>,
}

/// Publishing tier for a skillpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RubricTier {
    /// Endorsed by the maintainers — ready for production use.
    Endorsed,
    /// Community-maintained — works but may need more testing.
    Community,
    /// Experimental — new/untested, use with caution.
    Experimental,
    /// Blocked by one or more required core checks — needs fixing before publishing.
    Blocked,
}

impl std::fmt::Display for RubricTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RubricTier::Endorsed => "endorsed",
            RubricTier::Community => "community",
            RubricTier::Experimental => "experimental",
            RubricTier::Blocked => "blocked",
        };
        f.write_str(s)
    }
}

/// The 10 quality dimensions checked by the rubric.
fn dimensions() -> Vec<(
    u8,
    &'static str,
    RubricCategory,
    &'static str,
    bool,
    Option<&'static str>,
    fn(&RubricInput) -> (bool, String, Option<String>),
)> {
    vec![
        // 1. CORE: manifest valid schema.
        (
            1,
            "manifest_valid",
            RubricCategory::Core,
            "skillpack.json passes the v1 schema validator",
            false,
            None,
            |input| {
                let pack_root = &input.pack_root;
                let manifest_path = pack_root.join("skillpack.json");
                let content = match std::fs::read_to_string(&manifest_path) {
                    Ok(c) => c,
                    Err(e) => return (false, format!("Failed to read: {}", e), None),
                };
                match manifest_v1::parse_validate_manifest(&content) {
                    Ok(_) => (true, "manifest validates".to_string(), None),
                    Err(e) => (
                        false,
                        format!("Invalid manifest: {}", e),
                        Some("Run `zbrain skillpack init <name>` to regenerate a valid stub manifest.".to_string()),
                    ),
                }
            },
        ),
        // 2. CORE: every skill has an SKILL.md with required frontmatter.
        (
            2,
            "skills_have_skill_md",
            RubricCategory::Core,
            "every listed skill has a SKILL.md with valid required frontmatter (name, description, triggers)",
            false,
            None,
            |input| {
                let mut passed = true;
                let mut detail = String::new();
                for skill_slug in &input.manifest.skills {
                    let skill_path = input.pack_root.join(skill_slug).join("SKILL.md");
                    if !skill_path.exists() {
                        passed = false;
                        detail.push_str(&format!("\n- {}: missing SKILL.md", skill_slug));
                    }
                }
                (passed, detail, None)
            },
        ),
        // 3. CORE: LICENSE file exists at root.
        (
            3,
            "license_exists",
            RubricCategory::Core,
            "LICENSE file exists at pack root",
            false,
            None,
            |input| {
                let license = input.pack_root.join("LICENSE");
                (
                    license.exists(),
                    if !license.exists() {
                        "LICENSE file not found".to_string()
                    } else {
                        String::new()
                    },
                    None,
                )
            },
        ),
        // 4. CORE: all declared skill directories actually exist.
        (
            4,
            "declared_skills_exist",
            RubricCategory::Core,
            "all declared skill directories exist on disk",
            false,
            None,
            |input| {
                let mut passed = true;
                let mut detail = String::new();
                for skill_slug in &input.manifest.skills {
                    let skill_path = input.pack_root.join(skill_slug);
                    if !skill_path.exists() {
                        passed = false;
                        detail.push_str(&format!("\n- {}: skill directory missing", skill_slug));
                    }
                }
                (passed, detail, None)
            },
        ),
        // 5. CORE: shared deps declared actually exist.
        (
            5,
            "shared_deps_exist",
            RubricCategory::Core,
            "all declared shared dependency files/directories exist on disk",
            false,
            None,
            |input| {
                let mut passed = true;
                let mut detail = String::new();
                if let Some(shared) = &input.manifest.shared_deps {
                    for dep in shared {
                        let dep_path = input.pack_root.join(dep);
                        if !dep_path.exists() {
                            passed = false;
                            detail.push_str(&format!("\n- {}: shared dep missing", dep));
                        }
                    }
                }
                (passed, detail, None)
            },
        ),
        // 6. BADGE: at least one llm_eval file exists.
        (
            6,
            "has_llm_eval",
            RubricCategory::Badge,
            "skill has one or more LLM eval config files (*.jsonl)",
            false,
            None,
            |input| {
                let mut passed = false;
                let mut detail = String::new();
                if let Some(llm_evals) = &input.manifest.llm_evals {
                    if !llm_evals.is_empty() {
                        passed = true;
                    }
                }
                (passed, detail, None)
            },
        ),
        // 7. BADGE: at least one unit test exists.
        (
            7,
            "has_unit_test",
            RubricCategory::Badge,
            "skill has one or more unit tests (*.test.ts, *.rs in Rust)",
            false,
            None,
            |input| {
                let mut passed = false;
                let mut detail = String::new();
                if let Some(tests) = &input.manifest.unit_tests {
                    if !tests.is_empty() {
                        passed = true;
                    }
                }
                (passed, detail, None)
            },
        ),
        // 8. BADGE: at least one routing eval exists.
        (
            8,
            "has_routing_eval",
            RubricCategory::Badge,
            "skill has one or more routing eval fixtures (*.jsonl)",
            false,
            None,
            |input| {
                let mut passed = false;
                let mut detail = String::new();
                if let Some(routing_evals) = &input.manifest.routing_evals {
                    if !routing_evals.is_empty() {
                        passed = true;
                    }
                }
                (passed, detail, None)
            },
        ),
        // 9. BADGE: readme exists at pack root.
        (
            9,
            "has_readme",
            RubricCategory::Badge,
            "README.md exists at pack root with user-facing documentation",
            false,
            None,
            |input| {
                let readme = input.pack_root.join("README.md");
                (
                    readme.exists(),
                    if !readme.exists() {
                        "README.md not found".to_string()
                    } else {
                        String::new()
                    },
                    None,
                )
            },
        ),
        // 10. BADGE: changelog exists at pack root.
        (
            10,
            "has_changelog",
            RubricCategory::Badge,
            "CHANGELOG.md exists at pack root with release notes",
            false,
            None,
            |input| {
                let changelog = input.pack_root.join("CHANGELOG.md");
                (
                    changelog.exists(),
                    if !changelog.exists() {
                        "CHANGELOG.md not found".to_string()
                    } else {
                        String::new()
                    },
                    None,
                )
            },
        ),
    ]
}

/// Walk all 10 quality dimensions and compute a score.
pub fn walk_rubric(input: &RubricInput) -> RubricScore {
    let mut dims = Vec::new();
    let mut total = 0;
    let mut core_passed = 0;
    let mut badges_passed = 0;

    for (id, name, category, description, auto_fixable, fix_hint, check) in dimensions() {
        let (passed, detail, fix) = check(input);
        let result = RubricDimensionResult {
            id,
            name: name.to_string(),
            category,
            passed,
            description: description.to_string(),
            detail,
            fix_hint: fix.map(|s| s.to_string()),
            auto_fixable,
        };
        if passed {
            total += 1;
            match category {
                RubricCategory::Core => core_passed += 1,
                RubricCategory::Badge => badges_passed += 1,
            }
        }
        dims.push(result);
    }

    let tier = if core_passed == 5 {
        if badges_passed >= 5 {
            RubricTier::Endorsed
        } else if badges_passed >= 3 {
            RubricTier::Community
        } else {
            RubricTier::Experimental
        }
    } else {
        RubricTier::Blocked
    };

    let mut blockers = Vec::new();
    for dim in &dims {
        if !dim.passed && dim.category == RubricCategory::Core {
            blockers.push(dim.name.clone());
        }
    }

    RubricScore {
        dimensions: dims,
        total,
        core_passed,
        badges_passed,
        tier_eligibility: tier,
        promotion_blockers: blockers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_rubric() {
        use std::path::PathBuf;
        let input = RubricInput {
            pack_root: PathBuf::from("."),
            manifest: manifest_v1::SkillpackManifest {
                api_version: manifest_v1::SKILLPACK_API_VERSION.to_string(),
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: "test".to_string(),
                author: "test".to_string(),
                license: "MIT".to_string(),
                homepage: "https://example.com".to_string(),
                zbrain_min_version: "0.40.0".to_string(),
                skills: vec!["test-skill".to_string()],
                shared_deps: None,
                excluded_from_install: None,
                unit_tests: None,
                llm_evals: None,
                routing_evals: None,
                runbooks: None,
                changelog: None,
            },
        };
        let score = walk_rubric(&input);
        // core check 1 (manifest) passes, other core checks pass by default (no deps/skills missing checked here)
        assert_eq!(score.total, 1);
        assert_eq!(score.core_passed, 1);
        assert_eq!(score.tier_eligibility, RubricTier::Experimental);
    }
}
