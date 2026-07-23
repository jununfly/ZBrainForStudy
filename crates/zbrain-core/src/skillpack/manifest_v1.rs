//! skillpack v1 manifest validator.
//!
//! Validates the skillpack.json manifest for third-party skillpacks.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Current manifest API version.
pub const SKILLPACK_API_VERSION: &str = "zbrain-skillpack-v1";
/// Current runbook schema version.
pub const RUNBOOK_SCHEMA_VERSION: u32 = 1;
/// Current eval schema version.
pub const EVAL_SCHEMA_VERSION: u32 = 1;

/// Error codes for manifest validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillpackManifestErrorCode {
    /// Manifest file not found.
    ManifestNotFound,
    /// Manifest is not valid JSON.
    ManifestMalformedJson,
    /// Manifest is missing a required field.
    ManifestMissingField,
    /// Manifest field has an invalid value.
    ManifestInvalidField,
    /// API version is unknown.
    ManifestUnknownApiVersion,
    /// Schema version is not supported.
    ManifestUnsupportedSchemaVersion,
    /// No skills listed in the manifest.
    ManifestSkillNotFound,
}

impl std::fmt::Display for SkillpackManifestErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SkillpackManifestErrorCode::ManifestNotFound => "manifest_not_found",
            SkillpackManifestErrorCode::ManifestMalformedJson => "manifest_malformed_json",
            SkillpackManifestErrorCode::ManifestMissingField => "manifest_missing_field",
            SkillpackManifestErrorCode::ManifestInvalidField => "manifest_invalid_field",
            SkillpackManifestErrorCode::ManifestUnknownApiVersion => "manifest_unknown_api_version",
            SkillpackManifestErrorCode::ManifestUnsupportedSchemaVersion => {
                "manifest_unsupported_schema_version"
            }
            SkillpackManifestErrorCode::ManifestSkillNotFound => "manifest_skill_not_found",
        };
        f.write_str(s)
    }
}

/// Error for manifest validation.
#[derive(Debug)]
pub struct SkillpackManifestError {
    code: SkillpackManifestErrorCode,
    message: String,
    detail: Option<ManifestErrorDetail>,
}

#[derive(Debug)]
struct ManifestErrorDetail {
    field: Option<String>,
    expected: Option<String>,
    actual: Option<String>,
}

impl SkillpackManifestError {
    pub fn new(code: SkillpackManifestErrorCode, message: String) -> Self {
        Self {
            code,
            message,
            detail: None,
        }
    }

    pub fn with_detail(
        code: SkillpackManifestErrorCode,
        message: impl Into<String>,
        field: Option<&str>,
        expected: Option<&str>,
        actual: Option<&str>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            detail: Some(ManifestErrorDetail {
                field: field.map(String::from),
                expected: expected.map(String::from),
                actual: actual.map(String::from),
            }),
        }
    }
}

impl std::fmt::Display for SkillpackManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl From<crate::error::StructuredError> for SkillpackManifestError {
    fn from(e: crate::error::StructuredError) -> Self {
        Self::new(SkillpackManifestErrorCode::ManifestMalformedJson, e.to_string())
    }
}

impl std::error::Error for SkillpackManifestError {}

impl From<SkillpackManifestError> for Error {
    fn from(e: SkillpackManifestError) -> Self {
        crate::error::from_std_error(&e)
    }
}

/// Runbook paths displayed after scaffolding.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillpackRunbook {
    /// Path to bootstrap.md (per-step checklist printed after scaffold).
    pub bootstrap: Option<String>,
}

/// Third-party skillpack manifest (zbrain-skillpack-v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillpackManifest {
    /// API version (must be exactly SKILLPACK_API_VERSION).
    pub api_version: String,
    /// Package name (must match repo directory; unique in registry namespace).
    pub name: String,
    /// Semver-ish version string (Keep-a-Changelog compatible).
    pub version: String,
    /// One-line description, shown by `zbrain skillpack info`.
    pub description: String,
    /// Author name (display name optionally with email; not parsed).
    pub author: String,
    /// SPDX license id (e.g. "MIT").
    pub license: String,
    /// Homepage URL (canonical source repo).
    pub homepage: String,
    /// Minimum zbrain version this pack requires (semver).
    pub zbrain_min_version: String,
    /// Runbook format schema version (default: 1).
    #[serde(default)]
    pub runbook_schema_version: Option<u32>,
    /// Eval format schema version (default: 1).
    #[serde(default)]
    pub eval_schema_version: Option<u32>,
    /// Skill directories relative to pack root (e.g. ["skills/judge-submission"]).
    pub skills: Vec<String>,
    /// Shared dependencies (files/dirs every skill in the pack depends on).
    #[serde(default)]
    pub shared_deps: Option<Vec<String>>,
    /// Skills bundled but not installed by default.
    #[serde(default)]
    pub excluded_from_install: Option<Vec<String>>,
    /// Globs for unit tests run by `doctor --full` and publish-gate.
    #[serde(default)]
    pub unit_tests: Option<Vec<String>>,
    /// Globs for E2E tests (skipped when no DATABASE_URL).
    #[serde(default)]
    pub e2e_tests: Option<Vec<String>>,
    /// Globs for LLM-judge eval configs (cross-modal-eval shape).
    #[serde(default)]
    pub llm_evals: Option<Vec<String>>,
    /// Globs for routing-eval.jsonl files.
    #[serde(default)]
    pub routing_evals: Option<Vec<String>>,
    /// Runbook paths displayed after scaffolding.
    #[serde(default)]
    pub runbooks: Option<SkillpackRunbook>,
    /// Path to CHANGELOG.md.
    pub changelog: Option<String>,
}

/// Validate a skillpack manifest from raw JSON text.
pub fn parse_validate_manifest(json: &str) -> Result<SkillpackManifest> {
    let manifest: SkillpackManifest = serde_json::from_str(json)
        .map_err(|e| {
            SkillpackManifestError::new(
                SkillpackManifestErrorCode::ManifestMalformedJson,
                format!("Invalid JSON: {e}"),
            )
        })?;

    // Validate required fields
    if manifest.api_version != SKILLPACK_API_VERSION {
        return Err(SkillpackManifestError::with_detail(
            SkillpackManifestErrorCode::ManifestUnknownApiVersion,
            "Invalid API version",
            Some("api_version"),
            Some(SKILLPACK_API_VERSION),
            Some(manifest.api_version.as_str()),
        ).into());
    }

    if manifest.name.is_empty() {
        return Err(SkillpackManifestError::with_detail(
            SkillpackManifestErrorCode::ManifestMissingField,
            "name cannot be empty",
            Some("name"),
            None,
            None,
        ).into());
    }

    if manifest.version.is_empty() {
        return Err(SkillpackManifestError::with_detail(
            SkillpackManifestErrorCode::ManifestMissingField,
            "version cannot be empty",
            Some("version"),
            None,
            None,
        ).into());
    }

    if manifest.description.is_empty() {
        return Err(SkillpackManifestError::with_detail(
            SkillpackManifestErrorCode::ManifestMissingField,
            "description cannot be empty",
            Some("description"),
            None,
            None,
        ).into());
    }

    if manifest.author.is_empty() {
        return Err(SkillpackManifestError::with_detail(
            SkillpackManifestErrorCode::ManifestMissingField,
            "author cannot be empty",
            Some("author"),
            None,
            None,
        ).into());
    }

    if manifest.license.is_empty() {
        return Err(SkillpackManifestError::with_detail(
            SkillpackManifestErrorCode::ManifestMissingField,
            "license cannot be empty",
            Some("license"),
            None,
            None,
        ).into());
    }

    if manifest.homepage.is_empty() {
        return Err(SkillpackManifestError::with_detail(
            SkillpackManifestErrorCode::ManifestMissingField,
            "homepage cannot be empty",
            Some("homepage"),
            None,
            None,
        ).into());
    }

    if manifest.skills.is_empty() {
        return Err(SkillpackManifestError::new(
            SkillpackManifestErrorCode::ManifestSkillNotFound,
            "skills array cannot be empty — at least one skill must be listed".to_string(),
        ).into());
    }

    Ok(manifest)
}
