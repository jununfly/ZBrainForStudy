/**
 * skillpack/registry_schema.rs — runtime validators for the
 * `garrytan/zbrain-skillpack-registry` catalog files.
 *
 * Two files at the registry repo root:
 *   - registry.json     — catalog of all listed skillpacks
 *   - endorsements.json — Garry-controlled tier overrides (codex G3
 *     separation: contributors PR catalog entries; only Garry edits
 *     endorsements)
 *
 * Pure validators — no I/O. Used by the registry-client (fetch path)
 * and by the publish-gate when validating PR submissions.
 */

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use lazy_static::lazy_static;

pub const REGISTRY_SCHEMA_VERSION: &str = "zbrain-registry-v1";
pub const ENDORSEMENTS_SCHEMA_VERSION: &str = "zbrain-endorsements-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryTier {
    Endorsed,
    Community,
    Experimental,
    Dead,
}

impl std::fmt::Display for RegistryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryTier::Endorsed => write!(f, "endorsed"),
            RegistryTier::Community => write!(f, "community"),
            RegistryTier::Experimental => write!(f, "experimental"),
            RegistryTier::Dead => write!(f, "dead"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySource {
    /// Source kind — git is the v1 primary path; tarball-only is v1.5+.
    pub kind: String, // currently "git" only
    /// https:// URL to clone (must match the SSRF allowlist in git-remote.ts).
    pub url: String,
    /// Pinned commit SHA at PR-merge time. Required for endorsed/community.
    pub pinned_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Pack name (must match skillpack.json `name`. Unique in the catalog.
    pub name: String,
    /// One-line description for `zbrain skillpack search` output.
    pub description: String,
    /// Author display name (display only; account binding is via author_handle).
    pub author: String,
    /// Account handle on the source host (e.g. "garrytan" for github.com/garrytan).
    pub author_handle: String,
    /// Homepage URL (canonical pack repo).
    pub homepage: String,
    /// Source metadata.
    pub source: RegistrySource,
    /// SHA-256 of the published tarball at validation time. Used by the
    /// durability path: if source repo disappears, the registry-hosted
    /// tarballs/<name>-<version>.tgz mirror matches this hash.
    pub tarball_sha256: String,
    /// Minimum zbrain version the pack supports.
    pub zbrain_min_version: String,
    /// Default tier — may be overridden by endorsements.json. Catalog PRs
    /// always land at `community`. Endorsement happens via the separate
    /// `zbrain skillpack endorse` command writing endorsements.json.
    pub default_tier: RegistryTier,
    /// Searchable tags (lowercase kebab strings).
    pub tags: Vec<String>,
    /// ISO 8601 timestamp of the most-recent successful publish-gate validation.
    pub validated_at: String,
    /// Reference to the immutable validation log under registry/validation-runs/.
    pub validation_run_id: String,
    /// Cached count of skills in this pack (informational; from skillpack.json).
    pub skills_count: u32,
    /// Cached list of skill slugs (informational).
    pub skills: Vec<String>,
    /// Pack version when validated.
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryBundles {
    /// Named bundles — `zbrain skillpack scaffold starter-pack` walks the list.
    #[serde(flatten)]
    pub bundles: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryCatalog {
    pub schema_version: String,
    /// Catalog last-updated timestamp (informational).
    pub updated_at: String,
    pub skillpacks: Vec<RegistryEntry>,
    /// Optional named bundles.
    pub bundles: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndorsementEntry {
    /// Tier this pack should resolve to (overriding default_tier).
    pub tier: RegistryTier,
    /// When the endorsement was set.
    pub endorsed_at: String,
    /// Optional human note (e.g. "promoted after 30 days of clean use).
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndorsementsFile {
    pub schema_version: String,
    /// Map from pack name → endorsement record. Missing entries inherit default_tier.
    pub endorsements: HashMap<String, EndorsementEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistrySchemaError {
    #[error("registry.json top-level must be an object")]
    MalformedJson,

    #[error("registry.json schema_version is {0}; expected {REGISTRY_SCHEMA_VERSION}")]
    UnknownSchema(String),

    #[error("registry entry is missing required field: {field}")]
    MissingField { field: String, entry_name: Option<String> },

    #[error("invalid field: {field}")]
    InvalidField { field: String, entry_name: Option<String> },
}

lazy_static! {
    static ref NAME_RE: Regex = Regex::new(r"^[a-z][a-z0-9-]{1,63}$").unwrap();
}

fn is_valid_tier(tier: &str) -> bool {
    matches!(tier, "endorsed" | "community" | "experimental" | "dead")
}

fn is_valid_default_tier(tier: &str) -> bool {
    matches!(tier, "community" | "experimental" | "dead")
}

/// Validate a parsed JSON value as a RegistryCatalog. Throws on every gap.
pub fn validate_registry_catalog(
    value: serde_json::Value,
) -> Result<RegistryCatalog, RegistrySchemaError> {
    let obj = match value {
        serde_json::Value::Object(obj) => obj,
        _ => return Err(RegistrySchemaError::MalformedJson),
    };

    let schema_version = obj.get("schema_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "schema_version".to_string(), entry_name: None })?;

    if schema_version != REGISTRY_SCHEMA_VERSION {
        return Err(RegistrySchemaError::UnknownSchema(schema_version.to_string()));
    }

    let updated_at = obj.get("updated_at")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "updated_at".to_string(), entry_name: None })?
        .to_string();

    let skillpacks = obj.get("skillpacks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "skillpacks".to_string(), entry_name: None })?;

    let mut validated_entries = Vec::new();
    for entry in skillpacks {
        validated_entries.push(validate_registry_entry(entry)?);
    }

    let bundles = if let Some(bundles_val) = obj.get("bundles") {
        match bundles_val {
            serde_json::Value::Object(bundles_map) => {
                let mut map = HashMap::new();
                for (name, arr) in bundles_map {
                    let names = arr.as_array()
                        .ok_or_else(|| RegistrySchemaError::InvalidField {
                            field: format!("bundles.{name}"), entry_name: None })?;
                    let string_names: Result<Vec<String>, _> = names
                        .iter()
                        .map(|v| v.as_str()
                            .map(|s| s.to_string())
                            .ok_or_else(|| RegistrySchemaError::InvalidField {
                                field: format!("bundles.{name}[i]"), entry_name: None })
                        )
                        .collect();
                    map.insert(name.to_string(), string_names?);
                }
                Some(map)
            }
            _ => return Err(RegistrySchemaError::InvalidField {
                field: "bundles".to_string(), entry_name: None }),
        }
    } else {
        None
    };

    Ok(RegistryCatalog {
        schema_version: schema_version.to_string(),
        updated_at,
        skillpacks: validated_entries,
        bundles,
    })
}

/// Validate one entry inside the skillpacks array.
pub fn validate_registry_entry(
    value: &serde_json::Value,
) -> Result<RegistryEntry, RegistrySchemaError> {
    let obj = match value {
        serde_json::Value::Object(obj) => obj,
        _ => return Err(RegistrySchemaError::MalformedJson),
    };

    let entry_name = obj.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());

    let required = &[
        "name", "description", "author", "author_handle", "homepage",
        "source", "tarball_sha256", "zbrain_min_version", "default_tier",
        "tags", "validated_at", "validation_run_id", "skills_count",
        "skills", "version",
    ];

    for &field in required {
        if !obj.contains_key(field) {
            return Err(RegistrySchemaError::MissingField {
                field: field.to_string(),
                entry_name: entry_name.clone(),
            });
        }
    }

    let name = obj.get("name").and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "name".to_string(),
            entry_name: entry_name.clone() })?;

    if !NAME_RE.is_match(name) {
        return Err(RegistrySchemaError::InvalidField {
            field: "name".to_string(),
            entry_name: entry_name.clone(),
        });
    }

    let name = name.to_string();

    for &field in &[
        "description", "author", "author_handle", "homepage",
        "tarball_sha256", "zbrain_min_version", "validated_at",
        "validation_run_id", "version",
    ] {
        let s = obj.get(field).and_then(|v| v.as_str());
        if s.is_none() || s.unwrap().is_empty() {
            return Err(RegistrySchemaError::InvalidField {
                field: field.to_string(),
                entry_name: Some(name.clone()),
            });
        }
    }

    let skills_count = obj.get("skills_count")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "skills_count".to_string(),
            entry_name: Some(name.clone()),
        })?;

    let default_tier_str = obj.get("default_tier").and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "default_tier".to_string(),
            entry_name: Some(name.clone()),
        })?;

    if !is_valid_default_tier(default_tier_str) {
        return Err(RegistrySchemaError::InvalidField {
            field: "default_tier".to_string(),
            entry_name: Some(name.clone()),
        });
    }

    let default_tier = match default_tier_str {
        "community" => RegistryTier::Community,
        "experimental" => RegistryTier::Experimental,
        "dead" => RegistryTier::Dead,
        _ => unreachable!(), // checked above
    };

    let tags = obj.get("tags")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "tags".to_string(),
            entry_name: Some(name.clone()),
        })?;

    let tags: Result<Vec<String>, _> = tags
        .iter()
        .map(|v| v.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| RegistrySchemaError::InvalidField {
                field: format!("tags[i]"),
                entry_name: Some(name.clone()),
            })
        )
        .collect();

    let skills = obj.get("skills")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "skills".to_string(),
            entry_name: Some(name.clone()),
        })?;

    let skills: Result<Vec<String>, _> = skills
        .iter()
        .map(|v| v.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| RegistrySchemaError::InvalidField {
                field: format!("skills[i]"),
                entry_name: Some(name.clone()),
            })
        )
        .collect();

    // Source object.
    let source_val = obj.get("source")
        .and_then(|v| v.as_object())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "source".to_string(),
            entry_name: Some(name.clone()),
        })?;

    let kind = source_val.get("kind").and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "source.kind".to_string(),
            entry_name: Some(name.clone()),
        })?;

    if kind != "git" {
        return Err(RegistrySchemaError::InvalidField {
            field: "source.kind".to_string(),
            entry_name: Some(name.clone()),
        });
    }

    let url = source_val.get("url").and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "source.url".to_string(),
            entry_name: Some(name.clone()),
        })?;

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(RegistrySchemaError::InvalidField {
            field: "source.url".to_string(),
            entry_name: Some(name.clone()),
        });
    }

    let pinned_commit = source_val.get("pinned_commit").and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "source.pinned_commit".to_string(),
            entry_name: Some(name.clone()),
        })?;

    lazy_static::lazy_static! {
        static ref PINNED_COMMIT_RE: Regex = Regex::new(r"^[a-f0-9]{7,40}$").unwrap();
    }
    if !PINNED_COMMIT_RE.is_match(pinned_commit) {
        return Err(RegistrySchemaError::InvalidField {
            field: "source.pinned_commit".to_string(),
            entry_name: Some(name.clone()),
        });
    }

    Ok(RegistryEntry {
        name,
        description: obj.get("description").unwrap().as_str().unwrap().to_string(),
        author: obj.get("author").unwrap().as_str().unwrap().to_string(),
        author_handle: obj.get("author_handle").unwrap().as_str().unwrap().to_string(),
        homepage: obj.get("homepage").unwrap().as_str().unwrap().to_string(),
        source: RegistrySource {
            kind: kind.to_string(),
            url: url.to_string(),
            pinned_commit: pinned_commit.to_string(),
        },
        tarball_sha256: obj.get("tarball_sha256").unwrap().as_str().unwrap().to_string(),
        zbrain_min_version: obj.get("zbrain_min_version").unwrap().as_str().unwrap().to_string(),
        default_tier,
        tags: tags?,
        validated_at: obj.get("validated_at").unwrap().as_str().unwrap().to_string(),
        validation_run_id: obj.get("validation_run_id").unwrap().as_str().unwrap().to_string(),
        skills_count: skills_count as u32,
        skills: skills?,
        version: obj.get("version").unwrap().as_str().unwrap().to_string(),
    })
}

/// Validate a parsed JSON value as an EndorsementsFile.
pub fn validate_endorsements_file(
    value: serde_json::Value,
) -> Result<EndorsementsFile, RegistrySchemaError> {
    let obj = match value {
        serde_json::Value::Object(obj) => obj,
        _ => return Err(RegistrySchemaError::MalformedJson),
    };

    let schema_version = obj.get("schema_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RegistrySchemaError::InvalidField {
            field: "schema_version".to_string(), entry_name: None })?;

    if schema_version != ENDORSEMENTS_SCHEMA_VERSION {
        return Err(RegistrySchemaError::UnknownSchema(schema_version.to_string()));
    }

    let endorsements_val = obj.get("endorsements");
    let endorsements_obj = match endorsements_val {
        Some(serde_json::Value::Object(map)) => map,
        _ => return Err(RegistrySchemaError::InvalidField {
            field: "endorsements".to_string(),
            entry_name: None,
        }),
    };

    let mut endorsements = HashMap::new();

    for (name, record_val) in endorsements_obj {
        let record_obj = match record_val {
            serde_json::Value::Object(obj) => obj,
        _ => return Err(RegistrySchemaError::InvalidField {
            field: format!("endorsements.{name}"),
            entry_name: None,
        }),
    };

        let tier_str = record_obj.get("tier").and_then(|v| v.as_str())
            .ok_or_else(|| RegistrySchemaError::InvalidField {
                field: format!("endorsements.{name}.tier"),
                entry_name: None,
            })?;

        if !is_valid_tier(tier_str) {
            return Err(RegistrySchemaError::InvalidField {
                field: format!("endorsements.{name}.tier"),
                entry_name: None,
            });
        }

        let tier = match tier_str {
            "endorsed" => RegistryTier::Endorsed,
            "community" => RegistryTier::Community,
            "experimental" => RegistryTier::Experimental,
            "dead" => RegistryTier::Dead,
            _ => unreachable!(),
        };

        let endorsed_at = record_obj.get("endorsed_at").and_then(|v| v.as_str())
            .ok_or_else(|| RegistrySchemaError::InvalidField {
                field: format!("endorsements.{name}.endorsed_at"),
                entry_name: None,
            })?
            .to_string();

        let note = record_obj.get("note").and_then(|v| v.as_str()).map(|s| s.to_string());

        endorsements.insert(name.to_string(), EndorsementEntry {
            tier,
            endorsed_at,
            note,
        });
    }

    Ok(EndorsementsFile {
        schema_version: schema_version.to_string(),
        endorsements,
    })
}

/// Project a registry entry through the endorsements overlay to produce the
/// effective tier shown to the user. If endorsements.json has a record for
/// this pack, it wins; otherwise default_tier from the catalog applies.
pub fn effective_tier(
    entry: &RegistryEntry,
    endorsements: Option<&EndorsementsFile>,
) -> RegistryTier {
    if let Some(end) = endorsements {
        if let Some(r#override) = end.endorsements.get(&entry.name) {
            return r#override.tier;
        }
    }
    entry.default_tier
}
