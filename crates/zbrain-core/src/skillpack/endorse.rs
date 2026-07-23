/**
 * skillpack/endorse.rs — Garry-only endorsement workflow for the registry.
 *
 * Operator runs `zbrain skillpack endorse <name> [--tier T]` inside a clone
 * of `garrytan/zbrain-skillpack-registry`. The CLI:
 * 1. Validates `<name>` exists in registry.json
 * 2. Reads + schema-validates endorsements.json
 * 3. Sets/clears the tier entry (community → endorsed is the common path)
 * 4. Writes back with stable key ordering so diffs are clean
 * 5. Stages + creates a one-line commit `endorse: <name> -> <tier>`
 * 6. Optionally pushes (--push)
 *
 * Pure-data shape lives here; the CLI wrapper in src/commands/skillpack.rs
 * handles user-facing argv parsing + git invocations.
 */

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::skillpack::registry_schema::{
    validate_registry_catalog, validate_endorsements_file, EndorsementsFile, RegistryTier,
    RegistrySchemaError, ENDORSEMENTS_SCHEMA_VERSION, REGISTRY_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndorseOptions {
    /// Absolute path to a clone of the registry repo.
    pub registry_repo_root: PathBuf,
    /// Pack name to endorse / change tier on.
    pub pack_name: String,
    /// Target tier. Defaults to 'endorsed' since that's the common move.
    pub tier: Option<RegistryTier>,
    /// Optional human note recorded alongside the endorsement.
    pub note: Option<String>,
    /// Dry-run: report what would change without writing or committing.
    #[serde(default)]
    pub dry_run: bool,
    /// Push the commit to origin/main after writing.
    #[serde(default)]
    pub push: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndorseResult {
    pub schema_version: &'static str,
    pub pack_name: String,
    pub prior_tier: Option<RegistryTier>,
    pub new_tier: RegistryTier,
    pub endorsements_path: PathBuf,
    pub commit_sha: Option<String>,
    pub pushed: bool,
    pub dry_run: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EndorseError {
    #[error("{0} does not look like a skillpack-registry repo (no registry.json at root)")]
    NotARegistryRepo(String),

    #[error("{0}/registry.json is malformed: {1}")]
    InvalidRegistrySchema(String, String),

    #[error("pack \"{0}\" is not in registry.json — endorse requires a catalog entry first")]
    PackNotInCatalog(String),

    #[error("git commit failed: {0}")]
    GitCommitFailed(String),

    #[error("git push failed: {0}")]
    GitPushFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<RegistrySchemaError> for EndorseError {
    fn from(err: RegistrySchemaError) -> Self {
        EndorseError::InvalidRegistrySchema("registry.json".to_string(), err.to_string())
    }
}

/// Verify the directory looks like a skillpack-registry repo.
fn assert_registry_repo(root: &Path) -> Result<(), EndorseError> {
    let reg = root.join("registry.json");
    if !reg.exists() {
        return Err(EndorseError::NotARegistryRepo(root.to_string_lossy().into_owned()));
    }

    let raw = fs::read_to_string(&reg)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    validate_registry_catalog(value)?;

    Ok(())
}

/// Stable JSON stringify with sorted keys at every depth.
fn stable_stringify<T: Serialize>(value: &T, indent: usize) -> Result<String, serde_json::Error> {
    // serde_json doesn't have built-in sorted keys, we need to do a recursive sort.
    let mut value = serde_json::to_value(value)?;
    sort_json_value(&mut value);
    let mut json = serde_json::to_string_pretty(&value)?;
    if indent == 2 {
        // serde_json uses 2 spaces for pretty, just add the newline at the end.
        json.push('\n');
    }
    Ok(json)
}

/// Recursively sort object keys.
fn sort_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<_> = map.iter_mut().collect();
            sorted.sort_by_key(|(k, _)| *k);
            for (_, v) in &mut sorted {
                sort_json_value(v);
            }
            *map = sorted.into_iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        Value::Array(arr) => {
            for v in arr {
                sort_json_value(v);
            }
        }
        _ => {}
    }
}

/// Pure-fn that mutates the parsed endorsements object.
pub fn apply_endorsement(
    mut current: EndorsementsFile,
    pack_name: &str,
    tier: RegistryTier,
    note: Option<&str>,
) -> (EndorsementsFile, Option<RegistryTier>) {
    let prior_tier = current.endorsements.get(pack_name).map(|e| e.tier);

    let endorsement = crate::skillpack::registry_schema::EndorsementEntry {
        tier,
        endorsed_at: Utc::now().to_rfc3339(),
        note: note.map(|s| s.to_string()),
    };

    current.endorsements.insert(pack_name.to_string(), endorsement);

    (current, prior_tier)
}

/// Run the full endorse flow: validate -> mutate -> write atomically ->
/// git stage + commit -> optionally push. Returns a structured result the
/// CLI formats.
pub fn run_endorse(opts: EndorseOptions) -> Result<EndorseResult, EndorseError> {
    assert_registry_repo(&opts.registry_repo_root)?;

    // Catalog membership check.
    let catalog_path = opts.registry_repo_root.join("registry.json");
    let catalog_raw = fs::read_to_string(&catalog_path)?;
    let catalog_value: Value = serde_json::from_str(&catalog_raw)?;
    let catalog = validate_registry_catalog(catalog_value)?;

    if !catalog.skillpacks.iter().any(|e| e.name == opts.pack_name) {
        return Err(EndorseError::PackNotInCatalog(opts.pack_name));
    }

    // Endorsements file (may be missing on a fresh registry).
    let end_path = opts.registry_repo_root.join("endorsements.json");
    let current = if end_path.exists() {
        let raw = fs::read_to_string(&end_path)?;
        let value: Value = serde_json::from_str(&raw)?;
        validate_endorsements_file(value)?
    } else {
        EndorsementsFile {
            schema_version: ENDORSEMENTS_SCHEMA_VERSION.to_string(),
            endorsements: std::collections::HashMap::new(),
        }
    };

    let tier = opts.tier.unwrap_or(RegistryTier::Endorsed);
    let (next, prior_tier) = apply_endorsement(current, &opts.pack_name, tier, opts.note.as_deref());

    if opts.dry_run {
        return Ok(EndorseResult {
            schema_version: "skillpack-endorse-v1",
            pack_name: opts.pack_name,
            prior_tier,
            new_tier: tier,
            endorsements_path: end_path,
            commit_sha: None,
            pushed: false,
            dry_run: true,
        });
    }

    // Atomic write via .tmp + rename.
    let tmp = end_path.with_extension("tmp");
    let json = stable_stringify(&next, 2)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &end_path)?;

    // git stage + commit.
    let mut commit_sha = None;
    let output = Command::new("git")
        .arg("-C")
        .arg(&opts.registry_repo_root)
        .arg("add")
        .arg("endorsements.json")
        .output()?;

    if !output.status.success() {
        return Err(EndorseError::GitCommitFailed(String::from_utf8_lossy(&output.stderr).into_owned()));
    }

    let commit_msg = format!("endorse: {} -> {}", opts.pack_name, tier);
    let output = Command::new("git")
        .arg("-C")
        .arg(&opts.registry_repo_root)
        .arg("commit")
        .arg("-m")
        .arg(commit_msg)
        .output()?;

    if !output.status.success() {
        return Err(EndorseError::GitCommitFailed(String::from_utf8_lossy(&output.stderr).into_owned()));
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(&opts.registry_repo_root)
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .output()?;

    if output.status.success() {
        commit_sha = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    let mut pushed = false;
    if opts.push {
        let output = Command::new("git")
            .arg("-C")
            .arg(&opts.registry_repo_root)
            .arg("push")
            .arg("origin")
            .arg("HEAD")
            .output()?;

        if !output.status.success() {
            return Err(EndorseError::GitPushFailed(String::from_utf8_lossy(&output.stderr).into_owned()));
        }
        pushed = true;
    }

    Ok(EndorseResult {
        schema_version: "skillpack-endorse-v1",
        pack_name: opts.pack_name,
        prior_tier,
        new_tier: tier,
        endorsements_path: end_path,
        commit_sha,
        pushed,
        dry_run: false,
    })
}
