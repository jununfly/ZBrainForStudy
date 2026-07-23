//! Bundle manifest enumeration — built-in skillpack from zbrain repo.
//!
//! Reads the openclaw.plugin.json and enumerates all files to install.

use std::fs::{read_dir, read_to_string, metadata, Metadata};
use std::path::{Path, PathBuf};
use std::env;

use serde::{Deserialize, Serialize};

use crate::error::{Error, StructuredError, from_std_error, Result};

/// Manifest for a built-in skill bundle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleManifest {
    /// Bundle name.
    pub name: String,
    /// Bundle version.
    pub version: String,
    /// Optional one-line description.
    #[serde(default)]
    pub description: Option<String>,
    /// List of skill directories (relative to zbrain root).
    pub skills: Vec<String>,
    /// List of shared dependency files/directories (relative to zbrain root).
    #[serde(default)]
    pub shared_deps: Vec<String>,
    /// Optional list of skills to exclude from install by default.
    #[serde(default)]
    pub excluded_from_install: Option<Vec<String>>,
}

/// Error codes for bundle loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleErrorCode {
    /// Manifest file not found.
    ManifestNotFound,
    /// Manifest is not valid JSON.
    ManifestMalformed,
    /// No skills listed in manifest.
    SkillNotFound,
    /// Could not find a zbrain repo root.
    ZbrainRootNotFound,
}

/// Error for bundle operations.
#[derive(Debug)]
pub struct BundleError {
    code: BundleErrorCode,
    message: String,
}

impl BundleError {
    pub fn new(code: BundleErrorCode, message: String) -> Self {
        Self { code, message }
    }

    /// Error code for this bundle failure.
    pub fn code(&self) -> BundleErrorCode {
        self.code
    }

    /// Human-readable message for this bundle failure.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code: {:?})", self.message, self.code)
    }
}

impl std::error::Error for BundleError {}

impl From<BundleError> for Error {
    fn from(e: BundleError) -> Self {
        StructuredError::new(
            "Bundle",
            "bundle_error",
            e.to_string(),
        )
    }
}

/// An entry in the enumeration of files to install.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleEntry {
    /// Absolute source path under zbrain root.
    pub source: PathBuf,
    /// Relative target path under install directory.
    pub rel_target: PathBuf,
    /// Whether this entry comes from shared dependencies.
    pub shared_dep: bool,
}

/// Alias for scaffolding entry (same as BundleEntry).
pub type ScaffoldEntry = BundleEntry;

/// Enumerate all files that need to be scaffolded from the bundle.
/// Same as `enumerate_bundle_files` but explicitly named for scaffolding API.
pub fn enumerate_scaffold_entries(
    zbrain_root: &Path,
    manifest: &BundleManifest,
    excluded: &[String],
) -> crate::error::Result<Vec<ScaffoldEntry>> {
    enumerate_bundle_files(zbrain_root, manifest, excluded)
}

/// Walk up from starting directory looking for a zbrain repo root.
/// Looks for `openclaw.plugin.json` sibling to `src/cli.ts`.
pub fn find_zbrain_root(start: Option<&Path>) -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    let start = start.unwrap_or(cwd.as_path());
    let mut dir = start.to_path_buf();

    for _ in 0..10 {
        if dir.join("openclaw.plugin.json").exists() && dir.join("src").join("cli.ts").exists() {
            return Some(dir);
        }
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        } else {
            break;
        }
    }

    None
}

/// Load and validate the bundle manifest from a zbrain root.
pub fn load_bundle_manifest(zbrain_root: &Path) -> Result<BundleManifest> {
    let manifest_path = zbrain_root.join("openclaw.plugin.json");

    if !manifest_path.exists() {
        return Err(StructuredError::from(BundleError::new(
            BundleErrorCode::ManifestNotFound,
            format!("openclaw.plugin.json not found at {}", manifest_path.display()),
        )));
    }

    let content = read_to_string(&manifest_path).map_err(|e| {
        StructuredError::from(BundleError::new(
            BundleErrorCode::ManifestMalformed,
            format!("Failed to read manifest: {e}"),
        ))
    })?;

    let parsed: BundleManifest = serde_json::from_str(&content).map_err(|e| {
        StructuredError::from(BundleError::new(
            BundleErrorCode::ManifestMalformed,
            format!("manifest is not valid JSON: {e}"),
        ))
    })?;

    if parsed.name.is_empty() || parsed.version.is_empty() {
        return Err(StructuredError::from(BundleError::new(
            BundleErrorCode::ManifestMalformed,
            "name and version must be non-empty strings".to_string(),
        )));
    }

    Ok(parsed)
}

/// Enumerate all files that need to be installed from the bundle.
/// Walks each skill directory and shared dependency, collects all regular files.
pub fn enumerate_bundle_files(
    zbrain_root: &Path,
    manifest: &BundleManifest,
    excluded: &[String],
) -> Result<Vec<BundleEntry>> {
    let mut out = Vec::new();

    // Enumerate skills
    for skill_rel in &manifest.skills {
        if excluded.contains(&skill_rel) {
            continue;
        }
        let abs_dir = zbrain_root.join(skill_rel);
        if abs_dir.exists() {
            walk_files(&abs_dir, PathBuf::from(skill_rel), false, &mut out);
        }
    }

    // Enumerate shared dependencies
    for shared_rel in &manifest.shared_deps {
        let abs_path = zbrain_root.join(shared_rel);
        if !abs_path.exists() {
            continue;
        }
        let meta = metadata(&abs_path).ok();
        if let Some(meta) = meta {
            if meta.is_file() {
                out.push(BundleEntry {
                    source: abs_path.clone(),
                    rel_target: PathBuf::from(shared_rel),
                    shared_dep: true,
                });
            } else if meta.is_dir() {
                walk_files(&abs_path, PathBuf::from(shared_rel), true, &mut out);
            }
        }
    }

    Ok(out)
}

fn walk_files(abs_dir: &Path, prefix: PathBuf, shared_dep: bool, out: &mut Vec<BundleEntry>) {
    let read_dir = match read_dir(abs_dir) {
        Ok(d) => d,
        Err(_) => return,
    };

    for entry in read_dir.flatten() {
        let abs_path = entry.path();
        let rel_path = prefix.join(entry.file_name());

        if let Ok(meta) = entry.metadata() {
            if meta.is_dir() {
                walk_files(&abs_path, rel_path, shared_dep, out);
            } else if meta.is_file() {
                out.push(BundleEntry {
                    source: abs_path,
                    rel_target: rel_path,
                    shared_dep,
                });
            }
        }
    }
}

/// Read a skill's frontmatter `sources:` validated against the given root.
/// This validates that all the referenced source paths exist
/// and returns them as relative paths anchored at `root`.
pub fn load_skill_sources(root: &Path, skill_rel: &str) -> crate::error::Result<LoadedSkillSources> {
    use crate::markdown::{parse_markdown, ParsedMarkdown};
    use serde::Deserialize;

    let skill_md_path = root.join(skill_rel).join("SKILL.md");
    if !skill_md_path.exists() {
        return Err(crate::error::StructuredError::new(
            "Bundle",
            "skill_md_missing",
            format!("Skill markdown not found at {}", skill_md_path.display()),
        ));
    }

    let content = std::fs::read_to_string(&skill_md_path)
        .map_err(|e| crate::error::from_std_error(&e))?;
    let skill_md_path_str = skill_md_path.to_string_lossy();
    let parsed = parse_markdown(&content, &skill_md_path_str, None);

    #[derive(Deserialize, Default)]
    struct SkillFrontmatter {
        #[serde(default)]
        sources: Vec<String>,
    }

    let fm: SkillFrontmatter = serde_json::from_value(parsed.frontmatter.clone())
        .unwrap_or_default();

    // Validate each source path is relative and exists.
    for src in &fm.sources {
        if src.starts_with('/') || src.starts_with("~/") {
            return Err(crate::error::StructuredError::new(
                "Bundle",
                "invalid_source_path",
                format!("Source path '{}' must be relative (no leading / or ~/)", src),
            ));
        }
        if src.contains("/..") || src.contains("../") {
            return Err(crate::error::StructuredError::new(
                "Bundle",
                "invalid_source_path",
                format!("Source path '{}' cannot contain ..", src),
            ));
        }
        let abs_path = root.join(src);
        if !abs_path.exists() {
            return Err(crate::error::StructuredError::new(
                "Bundle",
                "source_missing",
                format!("Source path '{}' does not exist (resolved to {})",
                src, abs_path.display()
            )));
        }
    }

    Ok(LoadedSkillSources {
        skills_dir: skill_rel.to_string(),
        sources: fm.sources,
    })
}

/// Result from loading skill sources.
#[derive(Debug, Clone)]
pub struct LoadedSkillSources {
    /// Skill directory relative path.
    pub skills_dir: String,
    /// Validated source paths (relative).
    pub sources: Vec<String>,
}
