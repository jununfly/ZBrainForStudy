//! SchemaPackManifest v1 data model + validation + identity.
//!
//! Ported from TS `src/core/schema-pack/manifest-v1.ts`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The supported API version string. All manifests MUST declare this.
pub const SCHEMA_PACK_API_VERSION: &str = "zbrain-schema-pack-v1";

// ---------------------------------------------------------------------------
// PackPrimitive — closed enum of five composable primitives
// ---------------------------------------------------------------------------

/// Five composable primitives. Closed enum — packs cannot add primitives,
/// only types that extend one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackPrimitive {
    Entity,
    Media,
    Temporal,
    Annotation,
    #[default]
    Concept,
}

/// The five primitive string constants (matches TS `PACK_PRIMITIVES`).
pub const PACK_PRIMITIVES: &[PackPrimitive] = &[
    PackPrimitive::Entity,
    PackPrimitive::Media,
    PackPrimitive::Temporal,
    PackPrimitive::Annotation,
    PackPrimitive::Concept,
];

// ---------------------------------------------------------------------------
// AggregatorKind — closed registry of calibration aggregator algorithms
// ---------------------------------------------------------------------------

/// v0.41 T3 — closed enum of calibration aggregator algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregatorKind {
    /// Standard Brier score over resolved binary takes.
    ScalarBrier,
    /// Brier weighted by take.confidence.
    WeightedBrier,
    /// Simple accuracy ratio (correct / resolved).
    CountBased,
    /// Descriptive rollup (tier counts, dominant topics, time span).
    ClusterSummary,
}

// ---------------------------------------------------------------------------
// Sub-structs
// ---------------------------------------------------------------------------

/// Link inference rule — regex + page/type hints.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkInference {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
}

/// A link type definition (e.g. "works_at", "mentions").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkTypeDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inverse: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference: Option<LinkInference>,
}

/// A page type definition (e.g. "person", "note", "company").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageTypeDefinition {
    pub name: String,
    pub primitive: PackPrimitive,
    /// Path-prefix patterns; first match wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_prefixes: Vec<String>,
    /// E8: explicit alias declarations drive query closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Whether eligible for facts extraction.
    #[serde(default)]
    pub extractable: bool,
    /// Whether this type is an "expert" for find_experts / whoknows.
    #[serde(default)]
    pub expert_routing: bool,
}

/// Frontmatter link mapping — field → link type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontmatterLinkDefinition {
    pub page_type: String,
    pub fields: Vec<String>,
    pub link_type: String,
}

/// Enrichable type with optional rubric slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrichableType {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rubric: Option<String>,
}

/// Filing rule for auto-capture directory routing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingRule {
    pub kind: String,
    pub directory: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `borrow_from` entry — selective borrow of types/link_types from another pack.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorrowFromEntry {
    pub pack: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_types: Option<Vec<String>>,
}

/// v0.41 T3 — per-pack calibration domain declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationDomain {
    /// Open string label visible in scorecards.
    pub name: String,
    /// Closed-enum algorithm to compute the scorecard.
    pub aggregator: AggregatorKind,
    /// Page types whose takes feed this domain.
    pub page_types: Vec<String>,
}

// ---------------------------------------------------------------------------
// SchemaPackManifest — top-level pack manifest
// ---------------------------------------------------------------------------

/// SchemaPackManifest v1 — the parsed + validated pack file shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaPackManifest {
    pub api_version: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Minimum zbrain version required to load this pack.
    #[serde(default = "default_zbrain_min_version")]
    pub zbrain_min_version: String,
    /// Parent pack name (None = full override, no parent).
    /// Defaults to Some("zbrain-base") when field is missing.
    #[serde(default = "default_extends")]
    pub extends: Option<String>,
    /// Selective borrow of types/link_types from another pack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub borrow_from: Vec<BorrowFromEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_types: Vec<PageTypeDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_types: Vec<LinkTypeDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frontmatter_links: Vec<FrontmatterLinkDefinition>,
    #[serde(default = "default_takes_kinds")]
    pub takes_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enrichable_types: Vec<EnrichableType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filing_rules: Vec<FilingRule>,
    /// v0.41 — phase participation declaration (additive, not subtractive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<String>>,
    /// v0.41 — per-pack calibration domain declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration_domains: Option<Vec<CalibrationDomain>>,
}

impl Default for SchemaPackManifest {
    fn default() -> Self {
        Self {
            api_version: SCHEMA_PACK_API_VERSION.to_string(),
            name: String::new(),
            version: String::new(),
            description: String::new(),
            author: None,
            license: None,
            homepage: None,
            zbrain_min_version: default_zbrain_min_version(),
            extends: default_extends(),
            borrow_from: Vec::new(),
            page_types: Vec::new(),
            link_types: Vec::new(),
            frontmatter_links: Vec::new(),
            takes_kinds: default_takes_kinds(),
            enrichable_types: Vec::new(),
            filing_rules: Vec::new(),
            phases: None,
            calibration_domains: None,
        }
    }
}

fn default_zbrain_min_version() -> String {
    "0.38.0".to_string()
}

fn default_extends() -> Option<String> {
    Some("zbrain-base".to_string())
}

fn default_takes_kinds() -> Vec<String> {
    vec![
        "fact".to_string(),
        "take".to_string(),
        "bet".to_string(),
        "hunch".to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Validation error code — mirrors TS `SchemaPackManifestError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaPackManifestErrorCode {
    InvalidApiVersion,
    InvalidShape,
    InvalidVersion,
}

/// Validation error envelope.
#[derive(Debug, Clone)]
pub struct SchemaPackManifestError {
    pub code: SchemaPackManifestErrorCode,
    pub message: String,
    pub path: Option<String>,
}

impl std::fmt::Display for SchemaPackManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SchemaPackManifestError({:?}): {}", self.code, self.message)?;
        if let Some(ref p) = self.path {
            write!(f, " (path: {p})")?;
        }
        Ok(())
    }
}

impl std::error::Error for SchemaPackManifestError {}

// ---------------------------------------------------------------------------
// Parse + validate
// ---------------------------------------------------------------------------

/// Parse + validate a manifest from a serde_json::Value (deserialized from
/// JSON or YAML). Throws `SchemaPackManifestError` on shape/version issues.
pub fn parse_schema_pack_manifest(
    raw: &serde_json::Value,
) -> Result<SchemaPackManifest, SchemaPackManifestError> {
    if !raw.is_object() {
        return Err(SchemaPackManifestError {
            code: SchemaPackManifestErrorCode::InvalidShape,
            message: "manifest must be a JSON/YAML object at the top level".into(),
            path: None,
        });
    }

    // Check api_version up front (before full deserialization for a
    // better error message).
    let api_version = raw
        .get("api_version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if api_version != SCHEMA_PACK_API_VERSION {
        return Err(SchemaPackManifestError {
            code: SchemaPackManifestErrorCode::InvalidApiVersion,
            message: format!(
                "unsupported api_version: {api_version:?}; expected {SCHEMA_PACK_API_VERSION}"
            ),
            path: None,
        });
    }

    serde_json::from_value::<SchemaPackManifest>(raw.clone()).map_err(|e| {
        SchemaPackManifestError {
            code: SchemaPackManifestErrorCode::InvalidShape,
            message: format!("manifest validation failed: {e}"),
            path: None,
        }
    })
}

// ---------------------------------------------------------------------------
// Identity: `<name>@<version>+<sha8>`
// ---------------------------------------------------------------------------

/// Pack identity — used as cache key, replay record, registry id.
/// Format: `<name>@<version>+<sha8>`.
pub fn pack_identity(manifest: &SchemaPackManifest, sha8: &str) -> String {
    format!("{}@{}+{}", manifest.name, manifest.version, sha8)
}

/// Compute the manifest's content hash (first 8 hex chars of SHA-256).
///
/// Uses canonical JSON (sorted keys) for determinism — matches the TS
/// `computeManifestSha8` behaviour.
pub fn compute_manifest_sha8(manifest: &SchemaPackManifest) -> String {
    let canonical = serde_json::to_string(manifest).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let result = hasher.finalize();
    // First 4 bytes → 8 hex chars
    hex::encode(&result[..4])
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- PackPrimitive ---------------------------------------------------

    #[test]
    fn pack_primitive_deserialize_json() {
        let p: PackPrimitive = serde_json::from_str("\"entity\"").unwrap();
        assert_eq!(p, PackPrimitive::Entity);
        let p: PackPrimitive = serde_json::from_str("\"media\"").unwrap();
        assert_eq!(p, PackPrimitive::Media);
        let p: PackPrimitive = serde_json::from_str("\"temporal\"").unwrap();
        assert_eq!(p, PackPrimitive::Temporal);
        let p: PackPrimitive = serde_json::from_str("\"annotation\"").unwrap();
        assert_eq!(p, PackPrimitive::Annotation);
        let p: PackPrimitive = serde_json::from_str("\"concept\"").unwrap();
        assert_eq!(p, PackPrimitive::Concept);
    }

    #[test]
    fn pack_primitive_invalid_value() {
        let err = serde_json::from_str::<PackPrimitive>("\"unknown\"").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown"),
            "error should mention the invalid value: {msg}"
        );
    }

    // ---- AggregatorKind --------------------------------------------------

    #[test]
    fn aggregator_kind_deserialize() {
        assert_eq!(
            serde_json::from_str::<AggregatorKind>("\"scalar_brier\"").unwrap(),
            AggregatorKind::ScalarBrier,
        );
        assert_eq!(
            serde_json::from_str::<AggregatorKind>("\"weighted_brier\"").unwrap(),
            AggregatorKind::WeightedBrier,
        );
        assert_eq!(
            serde_json::from_str::<AggregatorKind>("\"count_based\"").unwrap(),
            AggregatorKind::CountBased,
        );
        assert_eq!(
            serde_json::from_str::<AggregatorKind>("\"cluster_summary\"").unwrap(),
            AggregatorKind::ClusterSummary,
        );
    }

    // ---- Minimal valid manifest ------------------------------------------

    #[test]
    fn parse_minimal_valid_manifest() {
        let json = serde_json::json!({
            "api_version": "zbrain-schema-pack-v1",
            "name": "test-pack",
            "version": "1.0.0",
        });
        let m = parse_schema_pack_manifest(&json).unwrap();
        assert_eq!(m.api_version, SCHEMA_PACK_API_VERSION);
        assert_eq!(m.name, "test-pack");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.description, "");
        assert!(m.page_types.is_empty());
        assert!(m.link_types.is_empty());
        assert_eq!(m.extends, Some("zbrain-base".to_string()));
    }

    #[test]
    fn parse_manifest_with_page_types() {
        let json = serde_json::json!({
            "api_version": "zbrain-schema-pack-v1",
            "name": "test-pack",
            "version": "1.0.0",
            "page_types": [
                {
                    "name": "person",
                    "primitive": "entity",
                    "path_prefixes": ["people/"],
                    "aliases": [],
                    "extractable": true,
                    "expert_routing": true
                }
            ]
        });
        let r = parse_schema_pack_manifest(&json);
        let m = r.unwrap();
        assert_eq!(m.page_types.len(), 1);
        let pt = &m.page_types[0];
        assert_eq!(pt.name, "person");
        assert_eq!(pt.primitive, PackPrimitive::Entity);
        assert_eq!(pt.path_prefixes, vec!["people/"]);
        assert!(pt.extractable);
        assert!(pt.expert_routing);
    }

    #[test]
    fn parse_manifest_invalid_api_version() {
        let json = serde_json::json!({
            "api_version": "wrong-version",
            "name": "test-pack",
            "version": "1.0.0",
        });
        let err = parse_schema_pack_manifest(&json).unwrap_err();
        assert!(matches!(
            err.code,
            SchemaPackManifestErrorCode::InvalidApiVersion
        ));
        assert!(
            err.message.contains("wrong-version"),
            "should mention the bad version: {}",
            err.message
        );
    }

    #[test]
    fn parse_manifest_not_an_object() {
        let json = serde_json::json!(["not", "an", "object"]);
        let err = parse_schema_pack_manifest(&json).unwrap_err();
        assert!(matches!(
            err.code,
            SchemaPackManifestErrorCode::InvalidShape
        ));
    }

    #[test]
    fn parse_manifest_missing_required_field() {
        let json = serde_json::json!({
            "api_version": "zbrain-schema-pack-v1",
            // missing required "version"
            "name": "test-pack",
        });
        let err = parse_schema_pack_manifest(&json).unwrap_err();
        assert!(matches!(
            err.code,
            SchemaPackManifestErrorCode::InvalidShape
        ));
    }

    #[test]
    fn parse_manifest_with_all_defaults() {
        let json = serde_json::json!({
            "api_version": "zbrain-schema-pack-v1",
            "name": "test-pack",
            "version": "1.0.0",
        });
        let m = parse_schema_pack_manifest(&json).unwrap();
        assert_eq!(m.description, "");
        assert!(m.author.is_none());
        assert!(m.license.is_none());
        assert!(m.homepage.is_none());
        assert_eq!(m.zbrain_min_version, "0.38.0");
        assert_eq!(m.extends, Some("zbrain-base".to_string()));
        assert!(m.borrow_from.is_empty());
        assert_eq!(m.takes_kinds, vec!["fact", "take", "bet", "hunch"]);
        assert!(m.phases.is_none());
        assert!(m.calibration_domains.is_none());
    }

    #[test]
    fn pack_identity_format() {
        let manifest = SchemaPackManifest {
            name: "my-pack".into(),
            version: "2.1.3".into(),
            ..Default::default()
        };
        let sha8 = "abcd1234".to_string();
        let id = pack_identity(&manifest, &sha8);
        assert_eq!(id, "my-pack@2.1.3+abcd1234");
    }

    #[test]
    fn compute_manifest_sha8_deterministic() {
        let m = SchemaPackManifest {
            name: "test-pack".into(),
            version: "1.0.0".into(),
            page_types: vec![PageTypeDefinition {
                name: "note".into(),
                primitive: PackPrimitive::Concept,
                ..Default::default()
            }],
            ..Default::default()
        };
        let h1 = compute_manifest_sha8(&m);
        let h2 = compute_manifest_sha8(&m);
        assert_eq!(h1, h2, "sha8 must be deterministic");
        assert_eq!(h1.len(), 8, "sha8 must be 8 hex chars");
    }

    #[test]
    fn compute_manifest_sha8_empty_changes() {
        let m1 = SchemaPackManifest::default();
        let m2 = SchemaPackManifest {
            name: "other".into(),
            ..Default::default()
        };
        let h1 = compute_manifest_sha8(&m1);
        let h2 = compute_manifest_sha8(&m2);
        assert_ne!(h1, h2, "different manifests must have different sha8");
    }
}
