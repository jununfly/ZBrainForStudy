use std::collections::HashMap;
use std::sync::RwLock;

use crate::LanguageId;

/// Per-language configuration for the tree-sitter chunker.
#[derive(Debug, Clone)]
pub struct LanguageEntry {
    /// AST node-type names that should be emitted as top-level chunks.
    pub top_level_types: Vec<String>,
    /// Configuration for nested child emission (e.g. methods inside a class).
    pub nested_emit: Option<NestedEmitConfig>,
}

/// Controls which parent AST nodes should emit their children as separate chunks.
#[derive(Debug, Clone)]
pub struct NestedEmitConfig {
    /// AST node-type names whose children should be individually chunked.
    pub parent_types: Vec<String>,
    /// AST node-type names of children to emit as sub-chunks.
    pub child_types: Vec<String>,
}

/// Runtime-mutable registry of language chunking configurations.
pub struct LanguageManifest {
    entries: RwLock<HashMap<LanguageId, LanguageEntry>>,
}

impl LanguageManifest {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) a language configuration.
    pub fn register(&self, lang: LanguageId, entry: LanguageEntry) {
        self.entries.write().unwrap().insert(lang, entry);
    }

    /// Remove a language from the manifest.
    pub fn unregister(&self, lang: LanguageId) {
        self.entries.write().unwrap().remove(&lang);
    }

    /// Look up a language entry.
    pub fn get(&self, lang: LanguageId) -> Option<LanguageEntry> {
        self.entries.read().unwrap().get(&lang).cloned()
    }
}

/// Build the default language manifest with registrations for all enabled language features.
///
/// Each language's entry is only registered when the corresponding Cargo feature flag is active.
pub fn default_manifest() -> LanguageManifest {
    let m = LanguageManifest::new();

    #[cfg(feature = "ts")]
    {
        m.register(
            LanguageId::TypeScript,
            LanguageEntry {
                top_level_types: vec![
                    "function_declaration".into(),
                    "class_declaration".into(),
                    "abstract_class_declaration".into(),
                    "interface_declaration".into(),
                    "type_alias_declaration".into(),
                    "enum_declaration".into(),
                    "lexical_declaration".into(),
                    "variable_declaration".into(),
                    "export_statement".into(),
                ],
                nested_emit: Some(NestedEmitConfig {
                    parent_types: vec![
                        "class_declaration".into(),
                        "abstract_class_declaration".into(),
                        "interface_declaration".into(),
                    ],
                    child_types: vec![
                        "method_definition".into(),
                        "method_signature".into(),
                        "public_field_definition".into(),
                    ],
                }),
            },
        );
        m.register(
            LanguageId::Tsx,
            LanguageEntry {
                top_level_types: vec![
                    "function_declaration".into(),
                    "class_declaration".into(),
                    "interface_declaration".into(),
                    "type_alias_declaration".into(),
                    "enum_declaration".into(),
                    "lexical_declaration".into(),
                    "variable_declaration".into(),
                    "export_statement".into(),
                ],
                nested_emit: Some(NestedEmitConfig {
                    parent_types: vec!["class_declaration".into(), "interface_declaration".into()],
                    child_types: vec![
                        "method_definition".into(),
                        "method_signature".into(),
                        "public_field_definition".into(),
                    ],
                }),
            },
        );
        m.register(
            LanguageId::JavaScript,
            LanguageEntry {
                top_level_types: vec![
                    "function_declaration".into(),
                    "class_declaration".into(),
                    "lexical_declaration".into(),
                    "variable_declaration".into(),
                    "export_statement".into(),
                ],
                nested_emit: Some(NestedEmitConfig {
                    parent_types: vec!["class_declaration".into()],
                    child_types: vec!["method_definition".into(), "field_definition".into()],
                }),
            },
        );
    }

    #[cfg(feature = "python")]
    {
        m.register(
            LanguageId::Python,
            LanguageEntry {
                top_level_types: vec![
                    "function_definition".into(),
                    "class_definition".into(),
                    "import_statement".into(),
                    "import_from_statement".into(),
                    "assignment".into(),
                ],
                nested_emit: Some(NestedEmitConfig {
                    parent_types: vec!["class_definition".into()],
                    child_types: vec!["function_definition".into()],
                }),
            },
        );
    }

    #[cfg(feature = "go")]
    {
        m.register(
            LanguageId::Go,
            LanguageEntry {
                top_level_types: vec![
                    "function_declaration".into(),
                    "method_declaration".into(),
                    "type_declaration".into(),
                    "const_declaration".into(),
                    "var_declaration".into(),
                    "import_declaration".into(),
                ],
                nested_emit: None,
            },
        );
    }

    #[cfg(feature = "rust")]
    {
        m.register(
            LanguageId::Rust,
            LanguageEntry {
                top_level_types: vec![
                    "function_item".into(),
                    "impl_item".into(),
                    "struct_item".into(),
                    "enum_item".into(),
                    "trait_item".into(),
                    "mod_item".into(),
                    "type_item".into(),
                    "const_item".into(),
                    "static_item".into(),
                    "use_declaration".into(),
                ],
                nested_emit: Some(NestedEmitConfig {
                    parent_types: vec!["impl_item".into(), "trait_item".into()],
                    child_types: vec!["function_item".into()],
                }),
            },
        );
    }

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_query_language() {
        let m = LanguageManifest::new();
        m.register(
            LanguageId::TypeScript,
            LanguageEntry {
                top_level_types: vec!["function_declaration".into()],
                nested_emit: None,
            },
        );
        let entry = m.get(LanguageId::TypeScript).unwrap();
        assert_eq!(entry.top_level_types, vec!["function_declaration"]);
    }

    #[test]
    fn unregister_language() {
        let m = LanguageManifest::new();
        m.register(LanguageId::TypeScript, LanguageEntry {
            top_level_types: vec!["function_declaration".into()],
            nested_emit: None,
        });
        m.unregister(LanguageId::TypeScript);
        assert!(m.get(LanguageId::TypeScript).is_none());
    }

    #[test]
    fn default_manifest_has_core_languages() {
        let m = default_manifest();
        // All four core languages should be registered when all features are enabled.
        assert!(m.get(LanguageId::TypeScript).is_some(), "TypeScript not registered");
        assert!(m.get(LanguageId::Tsx).is_some(), "TSX not registered");
        assert!(m.get(LanguageId::JavaScript).is_some(), "JavaScript not registered");
        assert!(m.get(LanguageId::Python).is_some(), "Python not registered");
        assert!(m.get(LanguageId::Go).is_some(), "Go not registered");
        assert!(m.get(LanguageId::Rust).is_some(), "Rust not registered");
    }

    #[test]
    fn typescript_has_nested_emit_config() {
        let m = default_manifest();
        let ts = m.get(LanguageId::TypeScript).unwrap();
        let nested = ts.nested_emit.unwrap();
        assert!(nested.parent_types.contains(&"class_declaration".into()));
        assert!(nested.child_types.contains(&"method_definition".into()));
    }

    #[test]
    fn rust_has_impl_item_in_nested() {
        let m = default_manifest();
        let rs = m.get(LanguageId::Rust).unwrap();
        let nested = rs.nested_emit.unwrap();
        assert!(nested.parent_types.contains(&"impl_item".into()));
        assert!(nested.child_types.contains(&"function_item".into()));
    }
}
