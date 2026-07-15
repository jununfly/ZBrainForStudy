//! Skill conformance check — migrated from the TS `checkSkillConformance`
//! doctor check (TS→Rust endgame).
//!
//! Reads `<skills_dir>/manifest.json`, then for each declared skill verifies
//! the referenced file exists and begins with YAML frontmatter (`---`).
//!
//! This is a pure filesystem check — no `BrainEngine` needed — so it runs even
//! when the DB is unreachable. The skills directory is discovered by the CLI
//! (cwd walk-up + zbrain home); the TS original resolved it via the resolver,
//! which is a separate, still-unmigrated doctor slice.

use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct SkillManifest {
    #[serde(default)]
    skills: Vec<SkillEntry>,
}

#[derive(Debug, Deserialize)]
struct SkillEntry {
    name: String,
    path: String,
}

/// Outcome of a skill-conformance scan.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SkillConformanceStatus {
    Ok,
    Warn,
}

/// Mirror of TS `checkSkillConformance(skillsDir)`.
///
/// Returns `(status, message)` where `Ok` means every declared skill's file
/// exists and carries frontmatter, and `Warn` means the manifest is missing,
/// unparseable, or at least one skill fails its file/frontmatter check.
pub fn check_skill_conformance(skills_dir: &Path) -> (SkillConformanceStatus, String) {
    let manifest_path = skills_dir.join("manifest.json");
    if !manifest_path.exists() {
        return (SkillConformanceStatus::Warn, "manifest.json not found".to_string());
    }

    let raw = match fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(_) => return (SkillConformanceStatus::Warn, "Could not read manifest.json".to_string()),
    };

    let manifest: SkillManifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(_) => return (SkillConformanceStatus::Warn, "Could not parse manifest.json".to_string()),
    };

    let total = manifest.skills.len();
    let mut passing = 0usize;
    let mut failing: Vec<String> = Vec::new();

    for skill in &manifest.skills {
        let skill_path = skills_dir.join(&skill.path);
        if !skill_path.exists() {
            failing.push(format!("{}: file missing", skill.name));
            continue;
        }
        let body = match fs::read_to_string(&skill_path) {
            Ok(b) => b,
            Err(_) => {
                failing.push(format!("{}: unreadable", skill.name));
                continue;
            }
        };
        if !body.starts_with("---") {
            failing.push(format!("{}: no frontmatter", skill.name));
            continue;
        }
        passing += 1;
    }

    if failing.is_empty() {
        (SkillConformanceStatus::Ok, format!("{}/{} skills pass", passing, total))
    } else {
        (
            SkillConformanceStatus::Warn,
            format!("{}/{} pass. Failing: {}", passing, total, failing.join(", ")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_skillconf_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_manifest_is_warn() {
        let dir = scratch("missing_manifest");
        let (s, m) = check_skill_conformance(&dir);
        assert_eq!(s, SkillConformanceStatus::Warn);
        assert_eq!(m, "manifest.json not found");
    }

    #[test]
    fn unparseable_manifest_is_warn() {
        let dir = scratch("bad_manifest");
        fs::write(dir.join("manifest.json"), "{not json").unwrap();
        let (s, m) = check_skill_conformance(&dir);
        assert_eq!(s, SkillConformanceStatus::Warn);
        assert_eq!(m, "Could not parse manifest.json");
    }

    #[test]
    fn all_skills_pass() {
        let dir = scratch("all_pass");
        fs::write(
            dir.join("manifest.json"),
            r#"{"skills":[{"name":"a","path":"a/SKILL.md"},{"name":"b","path":"b/SKILL.md"}]}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a/SKILL.md"), "---\nname: a\n---\nbody").unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();
        fs::write(dir.join("b/SKILL.md"), "---\nname: b\n---\nbody").unwrap();
        let (s, m) = check_skill_conformance(&dir);
        assert_eq!(s, SkillConformanceStatus::Ok);
        assert_eq!(m, "2/2 skills pass");
    }

    #[test]
    fn detects_missing_file_and_no_frontmatter() {
        let dir = scratch("failures");
        fs::write(
            dir.join("manifest.json"),
            r#"{"skills":[{"name":"a","path":"a/SKILL.md"},{"name":"b","path":"b/SKILL.md"},{"name":"c","path":"c/SKILL.md"}]}"#,
        )
        .unwrap();
        // `a` exists but has no frontmatter delimiter
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a/SKILL.md"), "no frontmatter here").unwrap();
        // `b` is missing entirely
        // `c` is fine
        fs::create_dir_all(dir.join("c")).unwrap();
        fs::write(dir.join("c/SKILL.md"), "---\nname: c\n---\nbody").unwrap();
        let (s, m) = check_skill_conformance(&dir);
        assert_eq!(s, SkillConformanceStatus::Warn);
        assert!(m.contains("a: no frontmatter"), "msg={}", m);
        assert!(m.contains("b: file missing"), "msg={}", m);
        assert!(m.contains("1/3 pass"), "msg={}", m);
    }
}
