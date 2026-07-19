//! skill-manifest — unified manifest loader for resolver checks.
//!
//! Ported from `src/core/skill-manifest.ts`. Two paths converge:
//!   1. `skillsDir/manifest.json` exists + parses → use verbatim.
//!   2. Otherwise walk `skillsDir/*` dirs; for each dir with a `SKILL.md`,
//!      derive `{name, path}` from frontmatter `name:` (fallback dirname).
//!
//! Dotfile / underscore-prefixed dirs (`_conventions/`, `conventions/`,
//! `migrations/`, `_brain-filing-rules.md`) are excluded — they are not
//! skills in the routing sense.

use std::fs;
use std::path::{Path, PathBuf};

use crate::skill_resolver::skill_frontmatter::parse_skill_frontmatter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub name: String,
    /// Relative to skillsDir, e.g. "query/SKILL.md".
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct ManifestLoadResult {
    pub skills: Vec<ManifestEntry>,
    /// True when manifest.json was missing/unparseable and the skill set
    /// was derived from walking skillsDir.
    pub derived: bool,
}

/// Canonical entry point. Loads the manifest from `skillsDir/manifest.json`
/// or derives it by walking `skillsDir`.
pub fn load_or_derive_manifest(skills_dir: &Path) -> ManifestLoadResult {
    let manifest_path = skills_dir.join("manifest.json");
    if manifest_path.exists() {
        if let Ok(content) = fs::read_to_string(&manifest_path) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(skills) = parsed.get("skills").and_then(|s| s.as_array()) {
                    let valid = skills.iter().all(|s| {
                        matches!(s, serde_json::Value::Object(o) if
                            o.get("name").and_then(|v| v.as_str()).is_some() &&
                            o.get("path").and_then(|v| v.as_str()).is_some())
                    });
                    if valid {
                        let skills: Vec<ManifestEntry> = skills
                            .iter()
                            .map(|s| {
                                let o = s.as_object().unwrap();
                                ManifestEntry {
                                    name: o["name"].as_str().unwrap().to_string(),
                                    path: o["path"].as_str().unwrap().to_string(),
                                }
                            })
                            .collect();
                        return ManifestLoadResult {
                            skills,
                            derived: false,
                        };
                    }
                }
            }
        }
    }
    ManifestLoadResult {
        skills: derive_manifest(skills_dir),
        derived: true,
    }
}

/// Walk skillsDir; return every `<skillsDir>/<dir>/SKILL.md` as a
/// ManifestEntry. Dotfile / underscore-prefixed dirs are skipped.
fn derive_manifest(skills_dir: &Path) -> Vec<ManifestEntry> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        let subdir = entry.path();
        let meta = match subdir.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let skill_md = subdir.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        let frontmatter_name = parse_skill_frontmatter(&fs::read_to_string(&skill_md).unwrap_or_default())
            .and_then(|fm| fm.name);
        let name = if let Some(n) = frontmatter_name {
            if n.is_empty() {
                name.clone()
            } else {
                n
            }
        } else {
            name.clone()
        };
        out.push(ManifestEntry {
            name,
            path: format!("{}/SKILL.md", entry.file_name().to_string_lossy()),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_man_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, fm_name: &str) {
        let sub = dir.join(name);
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join("SKILL.md"),
            format!("---\nname: {}\n---\nbody", fm_name),
        )
        .unwrap();
    }

    #[test]
    fn derives_from_walk() {
        let dir = scratch("derive");
        write_skill(&dir, "query", "query");
        write_skill(&dir, "ingest", "ingest");
        let res = load_or_derive_manifest(&dir);
        assert!(res.derived);
        assert_eq!(res.skills.len(), 2);
        assert_eq!(res.skills[0].name, "ingest");
        assert_eq!(res.skills[0].path, "ingest/SKILL.md");
    }

    #[test]
    fn uses_explicit_manifest() {
        let dir = scratch("explicit");
        write_skill(&dir, "query", "query");
        fs::write(
            dir.join("manifest.json"),
            r#"{"skills":[{"name":"query","path":"query/SKILL.md"}]}"#,
        )
        .unwrap();
        let res = load_or_derive_manifest(&dir);
        assert!(!res.derived);
        assert_eq!(res.skills.len(), 1);
    }

    #[test]
    fn malformed_manifest_derives() {
        let dir = scratch("malformed");
        write_skill(&dir, "query", "query");
        fs::write(dir.join("manifest.json"), "{not json").unwrap();
        let res = load_or_derive_manifest(&dir);
        assert!(res.derived);
        assert_eq!(res.skills.len(), 1);
    }

    #[test]
    fn skips_underscore_and_dotfile_dirs() {
        let dir = scratch("skip");
        write_skill(&dir, "real", "real");
        fs::create_dir_all(dir.join("_conventions")).unwrap();
        fs::create_dir_all(dir.join(".hidden")).unwrap();
        let res = load_or_derive_manifest(&dir);
        assert_eq!(res.skills.len(), 1);
    }
}
