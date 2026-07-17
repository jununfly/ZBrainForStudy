//! `zbrain storage status` — page-level storage tiering report.
//!
//! Rust port of the TS `src/commands/storage.ts` + its three helpers
//! (`storage-config.ts`, `disk-walk.ts`, `source-resolver.ts`). Read-only
//! reporting: counts brain pages by storage tier (`db_tracked` / `db_only` /
//! `unspecified`), measures on-disk size per tier, and flags `db_only` pages
//! whose markdown file is missing from the repo.
//!
//! The TS config parser is a deliberately hand-rolled, zero-dependency
//! line scanner (it avoids `gray-matter`, which silently returned `{}` on
//! delimiter-less YAML and broke the feature). We reimplement that exact
//! scanner rather than swapping in `serde_yaml`, to preserve byte-level
//! behavior (deprecated alias normalization, auto `/` normalization,
//! path-segment-exact-prefix tier matching).

use crate::engine::{BrainEngine, PageFilters};
use crate::error::{Result, StructuredError};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use walkdir::WalkDir;

// ── Types ───────────────────────────────────────────────────────────────

/// Canonical storage config loaded from `zbrain.yml`'s `storage:` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageConfig {
    pub db_tracked: Vec<String>,
    pub db_only: Vec<String>,
}

/// A page's storage tier, derived from its slug against the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageTier {
    DbTracked,
    DbOnly,
    Unspecified,
}

/// Per-tier page counts (distinct semantic units from `DiskUsageByTier`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageCountsByTier {
    pub db_tracked: usize,
    pub db_only: usize,
    pub unspecified: usize,
}

/// Per-tier on-disk byte usage.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiskUsageByTier {
    pub db_tracked: u64,
    pub db_only: u64,
    pub unspecified: u64,
}

/// A `db_only` page whose markdown file is absent from the repo.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MissingFile {
    pub slug: String,
    pub expected_path: String,
}

/// Read-only result of a storage-status query. Narrow + stable so it doubles
/// as the `--json` scripting contract (mirrors TS `StorageStatusResult`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StorageStatusResult {
    pub config: Option<StorageConfig>,
    pub repo_path: Option<String>,
    pub total_pages: usize,
    pub pages_by_tier: PageCountsByTier,
    pub missing_files: Vec<MissingFile>,
    pub disk_usage_by_tier: DiskUsageByTier,
    pub warnings: Vec<String>,
}

/// One on-disk markdown file discovered by the repo walk.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskFileEntry {
    pub size: u64,
    pub mtime_ms: f64,
}

// ── Config parsing (port of storage-config.ts) ────────────────────────────

const STORAGE_KEYS: &[&str] = &["db_tracked", "db_only", "git_tracked", "supabase_only"];

static RE_TRAILING_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+#.*$").unwrap());
static RE_LEADING_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#.*$").unwrap());

/// Strip `#` comments: a trailing ` # ...` and a full-line `# ...`.
/// Conservative, matching TS `replace(/\s+#.*$/, '').replace(/^#.*$/, '')`.
fn strip_comment(line: &str) -> String {
    let trailing = RE_TRAILING_COMMENT.replace(line, "");
    let leading = RE_LEADING_COMMENT.replace(&trailing, "");
    leading.into_owned()
}

#[derive(Default, Debug)]
struct RawStorage {
    db_tracked: Vec<String>,
    db_only: Vec<String>,
    git_tracked: Vec<String>,
    supabase_only: Vec<String>,
}

/// Parse the `storage:` section of a `zbrain.yml` using the TS line scanner.
/// Returns `None` when there is no `storage:` key at all (so callers can
/// distinguish "no config" from "empty config").
pub fn parse_storage_yaml(content: &str) -> Option<RawStorage> {
    let mut raw = RawStorage::default();
    let mut in_storage = false;
    let mut saw_storage = false;
    let mut current_list: Option<String> = None;

    for line in content.split('\n') {
        let line = line.replace('\r', "");
        let no_comment = strip_comment(&line);
        if no_comment.trim().is_empty() {
            continue;
        }

        let indented = no_comment.starts_with(' ') || no_comment.starts_with('\t');
        if !indented {
            if let Some(colon) = no_comment.find(':') {
                let key = no_comment[..colon].trim();
                if key == "storage" {
                    in_storage = true;
                    saw_storage = true;
                    current_list = None;
                } else {
                    in_storage = false;
                    current_list = None;
                }
            }
            continue;
        }

        if !in_storage {
            continue;
        }

        let body = no_comment.trim_start();
        if body.starts_with('-') {
            if let Some(list) = &current_list {
                let value = body[1..]
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string();
                if !value.is_empty() {
                    match list.as_str() {
                        "db_tracked" => raw.db_tracked.push(value),
                        "db_only" => raw.db_only.push(value),
                        "git_tracked" => raw.git_tracked.push(value),
                        "supabase_only" => raw.supabase_only.push(value),
                        _ => {}
                    }
                }
            }
            continue;
        }

        if let Some(colon) = body.find(':') {
            let key = body[..colon].trim();
            if STORAGE_KEYS.contains(&key) {
                current_list = Some(key.to_string());
                // Inline empty list (`db_only: []`) — Rust fields default to
                // empty already, so no explicit action is needed.
                let _ = colon;
            } else {
                current_list = None;
            }
        }
    }

    if !saw_storage {
        return None;
    }
    Some(raw)
}

/// Normalize raw parsed keys into the canonical `StorageConfig` shape.
///
/// Resolution order (per TS eng-review pass 2): canonical keys win; else
/// deprecated `git_tracked`/`supabase_only` aliases map to canonical. Returns
/// any deprecation advisories for surfacing to the user.
fn normalize_storage_config(raw: &RawStorage) -> (StorageConfig, Vec<String>) {
    let has_canonical = !raw.db_tracked.is_empty() || !raw.db_only.is_empty();
    let has_deprecated = !raw.git_tracked.is_empty() || !raw.supabase_only.is_empty();

    let mut advisories = Vec::new();
    if has_deprecated {
        let which = [
            if !raw.git_tracked.is_empty() {
                "`git_tracked`"
            } else {
                ""
            },
            if !raw.supabase_only.is_empty() {
                "`supabase_only`"
            } else {
                ""
            },
        ]
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(" and ");
        if has_canonical {
            advisories.push(format!(
                "Warning: {which} in zbrain.yml is deprecated and ignored \
                 (canonical keys db_tracked/db_only are present). Remove the \
                 deprecated keys, or run `zbrain doctor --fix`."
            ));
        } else {
            advisories.push(format!(
                "Warning: {which} in zbrain.yml is deprecated. Rename to \
                 db_tracked / db_only — see docs/storage-tiering.md. Run \
                 `zbrain doctor --fix` for an automated rename."
            ));
        }
    }

    let config = if has_canonical {
        StorageConfig {
            db_tracked: raw.db_tracked.clone(),
            db_only: raw.db_only.clone(),
        }
    } else {
        StorageConfig {
            db_tracked: raw.git_tracked.clone(),
            db_only: raw.supabase_only.clone(),
        }
    };
    (config, advisories)
}

/// Pure validation: returns warning strings (does not mutate). Always runs
/// against the canonical shape, so messages reference `db_only`/`db_tracked`.
pub fn validate_storage_config(config: &StorageConfig) -> Vec<String> {
    let mut warnings = Vec::new();
    let tracked_set: std::collections::HashSet<&str> = config.db_tracked.iter().map(|s| s.as_str()).collect();
    for path in &config.db_only {
        if tracked_set.contains(path.as_str()) {
            warnings.push(format!(
                "Directory \"{path}\" appears in both db_tracked and db_only"
            ));
        }
    }
    for path in config.db_tracked.iter().chain(config.db_only.iter()) {
        if !path.ends_with('/') {
            warnings.push(format!(
                "Directory path \"{path}\" should end with \"/\" for consistency"
            ));
        }
    }
    warnings
}

/// Auto-normalize (silently add trailing `/`) and strict-validate. Throws
/// (`StructuredError`) on semantic overlap between tiers.
pub fn normalize_and_validate_storage_config(input: StorageConfig) -> Result<StorageConfig> {
    let normalize = |paths: &[String]| -> Vec<String> {
        paths
            .iter()
            .map(|p| {
                if p.ends_with('/') {
                    p.clone()
                } else {
                    format!("{p}/")
                }
            })
            .collect()
    };
    let tracked = normalize(&input.db_tracked);
    let dbonly = normalize(&input.db_only);

    let tracked_set: std::collections::HashSet<&str> = tracked.iter().map(|s| s.as_str()).collect();
    for path in &dbonly {
        if tracked_set.contains(path.as_str()) {
            return Err(StructuredError::new(
                "StorageConfig",
                "overlap",
                format!(
                    "zbrain.yml: directory \"{path}\" appears in both db_tracked \
                     and db_only — pick one tier. Edit zbrain.yml to remove the overlap."
                ),
            ));
        }
    }
    Ok(StorageConfig {
        db_tracked: tracked,
        db_only: dbonly,
    })
}

/// Path-segment match: a slug belongs to a tier directory iff the directory
/// is a complete path-segment ancestor (`media/x/` matches `media/x/foo` but
/// NOT `media/xerox/foo`). Requires the configured dir to end with `/`.
pub fn matches_tier_dir(slug: &str, dir: &str) -> bool {
    if !dir.ends_with('/') {
        return false;
    }
    slug.starts_with(dir)
}

/// Resolve a slug's storage tier against the config.
pub fn get_storage_tier(slug: &str, config: &StorageConfig) -> StorageTier {
    if config.db_tracked.iter().any(|d| matches_tier_dir(slug, d)) {
        StorageTier::DbTracked
    } else if config.db_only.iter().any(|d| matches_tier_dir(slug, d)) {
        StorageTier::DbOnly
    } else {
        StorageTier::Unspecified
    }
}

/// Load `zbrain.yml`'s `storage:` config from a repo root.
///
/// Returns `Ok(None)` when there is no `zbrain.yml`, no `storage:` section,
/// or an empty config. Throws (`StructuredError`) on overlap. Deprecation
/// advisories are returned alongside the config for surfacing to the user.
pub fn load_storage_config(
    repo_path: &str,
) -> Result<Option<(StorageConfig, Vec<String>)>> {
    let yaml_path = Path::new(repo_path).join("zbrain.yml");
    if !yaml_path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&yaml_path).map_err(|e| {
        StructuredError::new(
            "StorageConfig",
            "read_failed",
            format!("failed to read {}: {e}", yaml_path.display()),
        )
    })?;

    let raw = match parse_storage_yaml(&content) {
        Some(r) => r,
        None => return Ok(None),
    };

    let (config, advisories) = normalize_storage_config(&raw);
    let config = normalize_and_validate_storage_config(config)?;
    Ok(Some((config, advisories)))
}

// ── Disk walk (port of disk-walk.ts) ──────────────────────────────────────

/// Recursively walk `repo_path`, returning a map of slug → file metadata for
/// every `.md` file. Skips dot-directories (`.git`, `.zbrain`, …) and
/// `node_modules`. The slug is the on-disk path relative to the repo, with
/// the trailing `.md` stripped and `\` normalized to `/` (brain slugs use
/// forward slashes regardless of OS).
pub fn walk_brain_repo(repo_path: &str) -> HashMap<String, DiskFileEntry> {
    let mut result = HashMap::new();
    let repo = Path::new(repo_path);

    let walker = WalkDir::new(repo).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        if name.starts_with('.') {
            return false;
        }
        if name == "node_modules" {
            return false;
        }
        true
    });

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !name.ends_with(".md") {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let rel = match path.strip_prefix(repo) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let slug = match rel_str.strip_suffix(".md") {
            Some(s) => s.to_string(),
            None => continue,
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        result.insert(
            slug,
            DiskFileEntry {
                size: meta.len(),
                mtime_ms,
            },
        );
    }
    result
}

// ── Core status (port of storage.ts getStorageStatus) ─────────────────────

/// Compute storage status against the given engine + optional brain repo
/// path. Side-effect-free apart from the engine call and one filesystem
/// walk. When `repo_path` is `None`, `config` is `None` and every page rolls
/// up into `unspecified`.
pub async fn get_storage_status(
    engine: &dyn BrainEngine,
    repo_path: Option<String>,
) -> Result<StorageStatusResult> {
    let (config, advisories) = match &repo_path {
        Some(rp) => match load_storage_config(rp) {
            Ok(Some((c, adv))) => (Some(c), adv),
            Ok(None) => (None, Vec::new()),
            Err(e) => return Err(e),
        },
        None => (None, Vec::new()),
    };

    let warnings = match &config {
        Some(c) => {
            let mut w = validate_storage_config(c);
            w.extend(advisories);
            w
        }
        None => advisories,
    };

    let mut pages_by_tier = PageCountsByTier::default();
    let mut disk_usage_by_tier = DiskUsageByTier::default();
    let mut missing_files = Vec::new();

    let file_map = match &repo_path {
        Some(rp) => walk_brain_repo(rp),
        None => HashMap::new(),
    };

    let pages = engine
        .list_pages(&PageFilters {
            limit: Some(1_000_000),
            ..Default::default()
        })
        .await?;

    for page in &pages {
        let tier = match &config {
            Some(c) => get_storage_tier(&page.slug, c),
            None => StorageTier::Unspecified,
        };
        match tier {
            StorageTier::DbTracked => pages_by_tier.db_tracked += 1,
            StorageTier::DbOnly => pages_by_tier.db_only += 1,
            StorageTier::Unspecified => pages_by_tier.unspecified += 1,
        }

        if repo_path.is_none() {
            continue;
        }
        match file_map.get(&page.slug) {
            Some(entry) => match tier {
                StorageTier::DbTracked => disk_usage_by_tier.db_tracked += entry.size,
                StorageTier::DbOnly => disk_usage_by_tier.db_only += entry.size,
                StorageTier::Unspecified => disk_usage_by_tier.unspecified += entry.size,
            },
            None => {
                if let (Some(c), StorageTier::DbOnly) = (&config, tier) {
                    let _ = c;
                    let expected = Path::new(repo_path.as_ref().unwrap())
                        .join(format!("{}.md", page.slug))
                        .to_string_lossy()
                        .to_string();
                    missing_files.push(MissingFile {
                        slug: page.slug.clone(),
                        expected_path: expected,
                    });
                }
            }
        }
    }

    Ok(StorageStatusResult {
        config,
        repo_path,
        total_pages: pages.len(),
        pages_by_tier,
        missing_files,
        disk_usage_by_tier,
        warnings,
    })
}

// ── Formatters (port of storage.ts formatters) ────────────────────────────

/// Serialize the result to the stable `--json` contract.
pub fn format_storage_status_json(result: &StorageStatusResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string())
}

fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    let k = 1024f64;
    let sizes = ["B", "KB", "MB", "GB", "TB"];
    let i = ((bytes as f64).ln() / k.ln()).floor() as usize;
    let i = i.min(sizes.len() - 1);
    let val = bytes as f64 / k.powi(i as i32);
    format!("{val:.1} {}", sizes[i])
}

/// Render the result to ASCII terminal text (ASCII separators only — no
/// unicode box-drawing, matching TS D10 lock).
pub fn format_storage_status_human(result: &StorageStatusResult) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("Storage Status".to_string());
    lines.push("==============".to_string());
    lines.push(String::new());

    if result.config.is_none() {
        lines.push("No zbrain.yml configuration found.".to_string());
        if let Some(rp) = &result.repo_path {
            lines.push(format!("Checked: {rp}/zbrain.yml"));
        }
        lines.push(String::new());
        lines.push("All pages are stored in git by default.".to_string());
        lines.push(format!("Total pages: {}", result.total_pages));
        return lines.join("\n");
    }

    lines.push(format!("Repository: {}", result.repo_path.as_deref().unwrap_or("")));
    lines.push(format!("Total pages: {}", result.total_pages));
    lines.push(String::new());
    lines.push("Storage Tiers:".to_string());
    lines.push("-------------".to_string());
    lines.push(format!("DB tracked:     {} pages", result.pages_by_tier.db_tracked));
    lines.push(format!("DB only:        {} pages", result.pages_by_tier.db_only));
    lines.push(format!(
        "Unspecified:    {} pages",
        result.pages_by_tier.unspecified
    ));

    if result.disk_usage_by_tier.db_tracked > 0 || result.disk_usage_by_tier.db_only > 0 {
        lines.push(String::new());
        lines.push("Disk Usage:".to_string());
        lines.push("-----------".to_string());
        if result.disk_usage_by_tier.db_tracked > 0 {
            lines.push(format!(
                "DB tracked:     {}",
                format_bytes(result.disk_usage_by_tier.db_tracked)
            ));
        }
        if result.disk_usage_by_tier.db_only > 0 {
            lines.push(format!(
                "DB only:        {}",
                format_bytes(result.disk_usage_by_tier.db_only)
            ));
        }
        if result.disk_usage_by_tier.unspecified > 0 {
            lines.push(format!(
                "Unspecified:    {}",
                format_bytes(result.disk_usage_by_tier.unspecified)
            ));
        }
    }

    if !result.missing_files.is_empty() {
        lines.push(String::new());
        lines.push("Missing Files (need restore):".to_string());
        lines.push("-----------------------------".to_string());
        for missing in result.missing_files.iter().take(10) {
            lines.push(format!("  {}", missing.slug));
        }
        if result.missing_files.len() > 10 {
            lines.push(format!(
                "  ... and {} more",
                result.missing_files.len() - 10
            ));
        }
        lines.push(String::new());
        if let Some(rp) = &result.repo_path {
            lines.push(format!(
                "Use: zbrain export --restore-only --repo \"{rp}\""
            ));
        }
    }

    if !result.warnings.is_empty() {
        lines.push(String::new());
        lines.push("Warnings:".to_string());
        lines.push("---------".to_string());
        for warning in &result.warnings {
            lines.push(format!("  ! {warning}"));
        }
    }

    lines.push(String::new());
    lines.push("Configuration:".to_string());
    lines.push("--------------".to_string());
    lines.push("DB tracked directories:".to_string());
    if let Some(c) = &result.config {
        for dir in &c.db_tracked {
            lines.push(format!("  - {dir}"));
        }
        lines.push(String::new());
        lines.push("DB-only directories:".to_string());
        for dir in &c.db_only {
            lines.push(format!("  - {dir}"));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    // ── parse_storage_yaml ──

    #[test]
    fn parses_storage_section_lists() {
        let yaml = "storage:\n  db_tracked:\n    - notes/\n  db_only:\n    - archive/\n";
        let raw = parse_storage_yaml(yaml).expect("has storage");
        assert_eq!(raw.db_tracked, vec!["notes/".to_string()]);
        assert_eq!(raw.db_only, vec!["archive/".to_string()]);
    }

    #[test]
    fn no_storage_section_is_none() {
        let yaml = "sync:\n  repo_path: /tmp/x\n";
        assert!(parse_storage_yaml(yaml).is_none());
    }

    #[test]
    fn inline_empty_list_ok() {
        let yaml = "storage:\n  db_tracked: []\n  db_only:\n    - a/\n";
        let raw = parse_storage_yaml(yaml).expect("has storage");
        assert!(raw.db_tracked.is_empty());
        assert_eq!(raw.db_only, vec!["a/".to_string()]);
    }

    #[test]
    fn comments_are_stripped() {
        let yaml = "# top comment\nstorage:\n  db_tracked:\n    - notes/  # trailing\n  # mid comment\n  db_only:\n    - archive/\n";
        let raw = parse_storage_yaml(yaml).expect("has storage");
        assert_eq!(raw.db_tracked, vec!["notes/".to_string()]);
        assert_eq!(raw.db_only, vec!["archive/".to_string()]);
    }

    #[test]
    fn deprecated_keys_parsed() {
        let yaml = "storage:\n  git_tracked:\n    - notes/\n  supabase_only:\n    - archive/\n";
        let raw = parse_storage_yaml(yaml).expect("has storage");
        assert_eq!(raw.git_tracked, vec!["notes/".to_string()]);
        assert_eq!(raw.supabase_only, vec!["archive/".to_string()]);
    }

    // ── normalize_storage_config ──

    #[test]
    fn canonical_wins_over_deprecated() {
        let raw = RawStorage {
            db_tracked: vec!["notes/".into()],
            db_only: vec!["archive/".into()],
            git_tracked: vec!["old/".into()],
            supabase_only: vec!["olddb/".into()],
        };
        let (cfg, adv) = normalize_storage_config(&raw);
        assert_eq!(cfg.db_tracked, vec!["notes/".to_string()]);
        assert_eq!(cfg.db_only, vec!["archive/".to_string()]);
        assert!(adv.iter().any(|a| a.contains("deprecated and ignored")));
    }

    #[test]
    fn deprecated_mapped_when_no_canonical() {
        let raw = RawStorage {
            git_tracked: vec!["notes/".into()],
            supabase_only: vec!["archive/".into()],
            ..Default::default()
        };
        let (cfg, adv) = normalize_storage_config(&raw);
        assert_eq!(cfg.db_tracked, vec!["notes/".to_string()]);
        assert_eq!(cfg.db_only, vec!["archive/".to_string()]);
        assert!(adv.iter().any(|a| a.contains("Rename to db_tracked")));
    }

    // ── validate_storage_config ──

    #[test]
    fn overlap_and_missing_slash_warnings() {
        let cfg = StorageConfig {
            db_tracked: vec!["notes/".into(), "notes".into()],
            db_only: vec!["notes".into(), "archive".into()],
        };
        let w = validate_storage_config(&cfg);
        assert!(w.iter().any(|x| x.contains("both db_tracked and db_only")));
        assert!(w.iter().any(|x| x.contains("should end with")));
    }

    #[test]
    fn clean_config_no_warnings() {
        let cfg = StorageConfig {
            db_tracked: vec!["notes/".into()],
            db_only: vec!["archive/".into()],
        };
        assert!(validate_storage_config(&cfg).is_empty());
    }

    // ── normalize_and_validate ──

    #[test]
    fn overlap_throws() {
        let cfg = StorageConfig {
            db_tracked: vec!["same/".into()],
            db_only: vec!["same/".into()],
        };
        assert!(normalize_and_validate_storage_config(cfg).is_err());
    }

    #[test]
    fn auto_adds_trailing_slash() {
        let cfg = StorageConfig {
            db_tracked: vec!["notes".into()],
            db_only: vec!["archive/".into()],
        };
        let out = normalize_and_validate_storage_config(cfg).unwrap();
        assert_eq!(out.db_tracked, vec!["notes/".to_string()]);
    }

    // ── matches_tier_dir / get_storage_tier ──

    #[test]
    fn path_segment_exact_prefix() {
        assert!(matches_tier_dir("media/x/foo", "media/x/"));
        assert!(!matches_tier_dir("media/xerox/foo", "media/x/"));
        assert!(!matches_tier_dir("media/x", "media/x/"));
        assert!(!matches_tier_dir("media/x/foo", "media/x")); // no trailing /
    }

    #[test]
    fn tier_resolution() {
        let cfg = StorageConfig {
            db_tracked: vec!["notes/".into()],
            db_only: vec!["archive/".into()],
        };
        assert_eq!(get_storage_tier("notes/a", &cfg), StorageTier::DbTracked);
        assert_eq!(get_storage_tier("archive/b", &cfg), StorageTier::DbOnly);
        assert_eq!(get_storage_tier("other/c", &cfg), StorageTier::Unspecified);
    }

    // ── walk_brain_repo ──

    #[test]
    fn walks_md_files_into_slugs() {
        let dir = tempfile::Builder::new().prefix("zbs_").tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("people")).unwrap();
        std::fs::write(root.join("people/alice.md"), "hello").unwrap();
        std::fs::write(root.join("people/bob.md"), "world!!").unwrap();
        std::fs::write(root.join("notes.txt"), "skip me").unwrap(); // not .md
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "x").unwrap(); // dot-dir skipped

        let map = walk_brain_repo(root.to_str().unwrap());
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("people/alice"));
        assert!(map.contains_key("people/bob"));
        assert_eq!(map.get("people/alice").unwrap().size, 5);
    }

    // ── get_storage_status ──

    async fn seed(engine: &InMemoryEngine, slug: &str) {
        engine
            .put_page(
                slug,
                None,
                &crate::engine::PageInput {
                    title: slug.to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn no_repo_all_unspecified() {
        let engine = InMemoryEngine::new();
        seed(&engine, "a").await;
        seed(&engine, "b").await;
        let res = get_storage_status(&engine, None).await.unwrap();
        assert_eq!(res.total_pages, 2);
        assert_eq!(res.pages_by_tier.unspecified, 2);
        assert!(res.config.is_none());
    }

    #[tokio::test]
    async fn tiers_and_missing_files() {
        let engine = InMemoryEngine::new();
        seed(&engine, "notes/kept").await; // db_tracked, file present
        seed(&engine, "archive/lost").await; // db_only, file MISSING
        seed(&engine, "other/free").await; // unspecified

        let dir = tempfile::Builder::new().prefix("zbs_").tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/kept.md"), "data").unwrap();
        std::fs::write(root.join("zbrain.yml"), "storage:\n  db_tracked:\n    - notes/\n  db_only:\n    - archive/\n").unwrap();

        let res = get_storage_status(&engine, Some(root.to_str().unwrap().to_string()))
            .await
            .unwrap();
        assert_eq!(res.total_pages, 3);
        assert_eq!(res.pages_by_tier.db_tracked, 1);
        assert_eq!(res.pages_by_tier.db_only, 1);
        assert_eq!(res.pages_by_tier.unspecified, 1);
        assert_eq!(res.disk_usage_by_tier.db_tracked, 4); // "data" = 4 bytes
        assert_eq!(res.missing_files.len(), 1);
        assert_eq!(res.missing_files[0].slug, "archive/lost");
    }

    #[tokio::test]
    async fn overlap_config_errors() {
        let engine = InMemoryEngine::new();
        seed(&engine, "a").await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("zbrain.yml"),
            "storage:\n  db_tracked:\n    - same/\n  db_only:\n    - same/\n",
        )
        .unwrap();
        assert!(get_storage_status(&engine, Some(root.to_str().unwrap().to_string()))
            .await
            .is_err());
    }

    // ── formatters ──

    #[test]
    fn json_roundtrip_has_tiers() {
        let res = StorageStatusResult {
            config: Some(StorageConfig {
                db_tracked: vec!["notes/".into()],
                db_only: vec!["archive/".into()],
            }),
            repo_path: Some("/repo".into()),
            total_pages: 3,
            pages_by_tier: PageCountsByTier {
                db_tracked: 1,
                db_only: 1,
                unspecified: 1,
            },
            missing_files: vec![],
            disk_usage_by_tier: DiskUsageByTier::default(),
            warnings: vec![],
        };
        let json = format_storage_status_json(&res);
        assert!(json.contains("\"db_tracked\": 1"));
        assert!(json.contains("\"repo_path\": \"/repo\""));
    }

    #[test]
    fn human_shows_tiers_and_config() {
        let res = StorageStatusResult {
            config: Some(StorageConfig {
                db_tracked: vec!["notes/".into()],
                db_only: vec!["archive/".into()],
            }),
            repo_path: Some("/repo".into()),
            total_pages: 2,
            pages_by_tier: PageCountsByTier {
                db_tracked: 1,
                db_only: 1,
                unspecified: 0,
            },
            missing_files: vec![],
            disk_usage_by_tier: DiskUsageByTier::default(),
            warnings: vec![],
        };
        let h = format_storage_status_human(&res);
        assert!(h.contains("Storage Status"));
        assert!(h.contains("DB tracked:     1 pages"));
        assert!(h.contains("DB tracked directories:"));
        assert!(h.contains("  - notes/"));
    }

    #[test]
    fn human_no_config_message() {
        let res = StorageStatusResult {
            config: None,
            repo_path: None,
            total_pages: 5,
            pages_by_tier: PageCountsByTier::default(),
            missing_files: vec![],
            disk_usage_by_tier: DiskUsageByTier::default(),
            warnings: vec![],
        };
        let h = format_storage_status_human(&res);
        assert!(h.contains("No zbrain.yml configuration found."));
        assert!(h.contains("Total pages: 5"));
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512.0 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
    }
}
