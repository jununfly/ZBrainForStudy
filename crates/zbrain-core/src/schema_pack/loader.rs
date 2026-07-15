//! Schema pack loader — YAML/JSON sniffing + normalization.
//!
//! Pack authors choose YAML or JSON. The loader sniffs by file extension
//! (`.yaml` / `.yml` / `.json`), parses, and normalizes to a
//! `SchemaPackManifest` before validation.
//!
//! Unlike the TS hand-rolled YAML mini-parser (which avoids a js-yaml
//! dependency), the Rust implementation uses `serde_yaml` (already a
//! workspace dependency for the capture pipeline). For Schema Pack
//! manifests, full YAML support is acceptable — users may want anchors,
//! references, etc.
//!
//! Ported from TS `src/core/schema-pack/loader.ts`.

use std::path::Path;

use super::manifest::{parse_schema_pack_manifest, SchemaPackManifest, SchemaPackManifestError};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Loader error codes — mirrors TS `SchemaPackLoaderError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderErrorCode {
    ParseError,
    FileNotFound,
    UnsupportedExtension,
}

/// Loader error envelope.
#[derive(Debug, Clone)]
pub struct LoaderError {
    pub code: LoaderErrorCode,
    pub message: String,
    pub path: String,
}

impl std::fmt::Display for LoaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SchemaPackLoaderError({:?}): {} (path: {})",
            self.code, self.message, self.path
        )
    }
}

impl std::error::Error for LoaderError {}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load + parse + validate a pack from disk. Returns the validated manifest.
pub fn load_pack_from_file(path: &Path) -> Result<SchemaPackManifest, LoaderError> {
    let content =
        std::fs::read_to_string(path).map_err(|e| LoaderError {
            code: LoaderErrorCode::FileNotFound,
            message: format!("cannot read pack file: {e}"),
            path: path.display().to_string(),
        })?;
    load_pack_from_string(&content, &path.display().to_string())
}

/// Parse a manifest from a raw string. Extension-driven: `.json` uses
/// serde_json, anything else uses serde_yaml (YAML). Test seam.
pub fn load_pack_from_string(
    content: &str,
    hint: &str,
) -> Result<SchemaPackManifest, LoaderError> {
    let ext = Path::new(hint)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let raw: serde_json::Value = if ext == "json" {
        serde_json::from_str(content).map_err(|e| LoaderError {
            code: LoaderErrorCode::ParseError,
            message: format!("JSON parse error: {e}"),
            path: hint.to_string(),
        })?
    } else {
        // Default to YAML for .yaml, .yml, and unknown extensions.
        serde_yaml::from_str(content).map_err(|e| LoaderError {
            code: LoaderErrorCode::ParseError,
            message: format!("YAML parse error: {e}"),
            path: hint.to_string(),
        })?
    };

    parse_schema_pack_manifest(&raw).map_err(|e| LoaderError {
        code: {
            use super::manifest::SchemaPackManifestErrorCode;
            match e.code {
                SchemaPackManifestErrorCode::InvalidApiVersion
                | SchemaPackManifestErrorCode::InvalidVersion => {
                    LoaderErrorCode::ParseError
                }
                SchemaPackManifestErrorCode::InvalidShape => {
                    LoaderErrorCode::ParseError
                }
            }
        },
        message: e.message,
        path: e.path.unwrap_or_else(|| hint.to_string()),
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_minimal_json_manifest() {
        let json = r#"{
            "api_version": "zbrain-schema-pack-v1",
            "name": "my-pack",
            "version": "0.1.0"
        }"#;
        let m = load_pack_from_string(json, "manifest.json").unwrap();
        assert_eq!(m.name, "my-pack");
        assert_eq!(m.version, "0.1.0");
    }

    #[test]
    fn load_minimal_yaml_manifest() {
        let yaml = r#"
api_version: zbrain-schema-pack-v1
name: my-pack
version: 0.1.0
"#;
        let m = load_pack_from_string(yaml, "manifest.yaml").unwrap();
        assert_eq!(m.name, "my-pack");
        assert_eq!(m.version, "0.1.0");
    }

    #[test]
    fn load_yml_extension_as_yaml() {
        let yaml = r#"
api_version: zbrain-schema-pack-v1
name: my-pack
version: 0.1.0
"#;
        let m = load_pack_from_string(yaml, "manifest.yml").unwrap();
        assert_eq!(m.name, "my-pack");
    }

    #[test]
    fn unknown_extension_defaults_to_yaml() {
        let yaml = r#"
api_version: zbrain-schema-pack-v1
name: my-pack
version: 0.1.0
"#;
        let m = load_pack_from_string(yaml, "manifest.txt").unwrap();
        assert_eq!(m.name, "my-pack");
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let json = "{ bad json }";
        let err = load_pack_from_string(json, "manifest.json").unwrap_err();
        assert!(matches!(err.code, LoaderErrorCode::ParseError));
        assert!(err.message.contains("JSON"), "should mention JSON: {}", err.message);
    }

    #[test]
    fn invalid_yaml_returns_parse_error() {
        let yaml = "\tinvalid: indentation";
        let err = load_pack_from_string(yaml, "manifest.yaml").unwrap_err();
        assert!(matches!(err.code, LoaderErrorCode::ParseError));
    }

    #[test]
    fn missing_api_version_returns_error() {
        let json = r#"{"name": "test", "version": "1.0.0"}"#;
        let err = load_pack_from_string(json, "manifest.json").unwrap_err();
        assert!(matches!(err.code, LoaderErrorCode::ParseError));
    }

    #[test]
    fn yaml_with_page_types() {
        let yaml = r#"
api_version: zbrain-schema-pack-v1
name: my-pack
version: 0.1.0
page_types:
  - name: person
    primitive: entity
    path_prefixes:
      - people/
    extractable: true
    expert_routing: true
  - name: note
    primitive: concept
    extractable: false
"#;
        let m = load_pack_from_string(yaml, "manifest.yaml").unwrap();
        assert_eq!(m.page_types.len(), 2);
        assert_eq!(m.page_types[0].name, "person");
        assert_eq!(m.page_types[0].primitive, crate::schema_pack::manifest::PackPrimitive::Entity);
        assert_eq!(m.page_types[1].name, "note");
    }

    #[test]
    fn yaml_with_link_types() {
        let yaml = r#"
api_version: zbrain-schema-pack-v1
name: my-pack
version: 0.1.0
page_types:
  - name: person
    primitive: entity
link_types:
  - name: works_at
    inverse: employs
  - name: founded
"#;
        let m = load_pack_from_string(yaml, "manifest.yaml").unwrap();
        assert_eq!(m.link_types.len(), 2);
        assert_eq!(m.link_types[0].name, "works_at");
        assert_eq!(m.link_types[0].inverse, Some("employs".into()));
        assert_eq!(m.link_types[1].name, "founded");
        assert!(m.link_types[1].inverse.is_none());
    }
}
