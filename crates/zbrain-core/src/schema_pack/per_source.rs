//! Per-source bindings + SQL CTE builder for federated reads.
//!
//! Ported from TS `src/core/schema-pack/per-source.ts`.
//!
//! When a query spans multiple sources, each source may have a different
//! active schema pack. This module expands the alias closure per-source
//! and builds a SQL CTE that filters pages by (source_id, type) pairs.

use std::collections::HashMap;

use super::closure::{self, AliasCycleError};
use super::manifest::SchemaPackManifest;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One source's closure-expanded type set for a query type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceClosureBinding {
    pub source_id: String,
    pub types: Vec<String>,
}

/// Error from per-source binding construction.
#[derive(Debug)]
pub enum PerSourceError {
    AliasCycle(AliasCycleError),
}

impl std::fmt::Display for PerSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AliasCycle(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PerSourceError {}

impl From<AliasCycleError> for PerSourceError {
    fn from(e: AliasCycleError) -> Self {
        Self::AliasCycle(e)
    }
}

// ---------------------------------------------------------------------------
// build_per_source_bindings
// ---------------------------------------------------------------------------

/// For each source's pack, build the alias closure of `query_type`.
/// Results are sorted by source_id for deterministic output (cache key stability).
pub fn build_per_source_bindings(
    query_type: &str,
    source_packs: &HashMap<String, SchemaPackManifest>,
) -> Result<Vec<SourceClosureBinding>, PerSourceError> {
    let mut bindings: Vec<SourceClosureBinding> = Vec::new();

    for (source_id, manifest) in source_packs {
        let graph = closure::build_alias_graph(manifest)?;
        let mut opts = closure::ExpandClosureOpts::default();
        let types = closure::expand_closure(query_type, &graph, &mut opts);
        bindings.push(SourceClosureBinding {
            source_id: source_id.clone(),
            types,
        });
    }

    // Sort by source_id for deterministic output (codex F4).
    bindings.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    Ok(bindings)
}

// ---------------------------------------------------------------------------
// build_source_closure_cte
// ---------------------------------------------------------------------------

/// Build a SQL CTE that produces (source_id, type) pairs for filtering.
///
/// Returns `None` if bindings is empty or all bindings have empty types.
/// The CTE uses PostgreSQL `$N` parameter placeholders for source_ids and
/// `escape_sql_literal` for type names.
///
/// Example output:
/// ```sql
/// SELECT $1::text AS source_id, unnest(ARRAY['person','researcher']::text[]) AS type
///   UNION ALL
/// SELECT $2::text AS source_id, unnest(ARRAY['family-member']::text[]) AS type
/// ```
pub fn build_source_closure_cte(
    bindings: &[SourceClosureBinding],
) -> Option<(String, Vec<String>)> {
    if bindings.is_empty() {
        return None;
    }

    // Defensive sort (even if caller already sorted, CTE shape is cache key).
    let mut sorted = bindings.to_vec();
    sorted.sort_by(|a, b| a.source_id.cmp(&b.source_id));

    let mut params: Vec<String> = Vec::new();
    let mut branches: Vec<String> = Vec::new();

    for binding in &sorted {
        if binding.types.is_empty() {
            continue;
        }

        let source_param_idx = params.len() + 1; // 1-based
        params.push(binding.source_id.clone());

        let escaped_types: Vec<String> = binding.types.iter().map(|t| escape_sql_literal(t)).collect();
        let types_array = escaped_types.join(",");

        branches.push(format!(
            "SELECT ${source_param_idx}::text AS source_id, unnest(ARRAY[{types_array}]::text[]) AS type"
        ));
    }

    if branches.is_empty() {
        return None;
    }

    let cte = branches.join("\n  UNION ALL\n  ");
    Some((cte, params))
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// PostgreSQL string literal escaping: single-quote doubling.
fn escape_sql_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_pack::manifest::{
        PageTypeDefinition, PackPrimitive, SchemaPackManifest,
    };

    fn make_pack(name: &str, page_types: Vec<PageTypeDefinition>) -> SchemaPackManifest {
        SchemaPackManifest {
            name: name.into(),
            version: "1.0.0".into(),
            page_types,
            ..Default::default()
        }
    }

    // ---- build_per_source_bindings --------------------------------------

    #[test]
    fn empty_source_packs() {
        let packs = HashMap::new();
        let bindings = build_per_source_bindings("person", &packs).unwrap();
        assert!(bindings.is_empty());
    }

    #[test]
    fn single_source_closure() {
        let mut packs = HashMap::new();
        packs.insert(
            "source-1".into(),
            make_pack("p1", vec![
                PageTypeDefinition {
                    name: "person".into(),
                    primitive: PackPrimitive::Entity,
                    aliases: vec!["researcher".into()],
                    ..Default::default()
                },
                PageTypeDefinition {
                    name: "researcher".into(),
                    primitive: PackPrimitive::Entity,
                    ..Default::default()
                },
            ]),
        );
        let bindings = build_per_source_bindings("person", &packs).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].source_id, "source-1");
        assert!(bindings[0].types.contains(&"person".to_string()));
        assert!(bindings[0].types.contains(&"researcher".to_string()));
    }

    #[test]
    fn multiple_sources_sorted() {
        let mut packs = HashMap::new();
        packs.insert(
            "z-source".into(),
            make_pack("p1", vec![PageTypeDefinition {
                name: "note".into(),
                primitive: PackPrimitive::Concept,
                ..Default::default()
            }]),
        );
        packs.insert(
            "a-source".into(),
            make_pack("p2", vec![PageTypeDefinition {
                name: "note".into(),
                primitive: PackPrimitive::Concept,
                ..Default::default()
            }]),
        );
        let bindings = build_per_source_bindings("note", &packs).unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].source_id, "a-source");
        assert_eq!(bindings[1].source_id, "z-source");
    }

    #[test]
    fn query_type_not_in_pack_returns_just_self() {
        let mut packs = HashMap::new();
        packs.insert(
            "s1".into(),
            make_pack("p1", vec![PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                ..Default::default()
            }]),
        );
        // "company" doesn't exist in the pack — closure is just ["company"]
        let bindings = build_per_source_bindings("company", &packs).unwrap();
        assert_eq!(bindings[0].types, vec!["company"]);
    }

    // ---- build_source_closure_cte ---------------------------------------

    #[test]
    fn empty_bindings_returns_none() {
        assert!(build_source_closure_cte(&[]).is_none());
    }

    #[test]
    fn all_empty_types_returns_none() {
        let bindings = vec![SourceClosureBinding {
            source_id: "s1".into(),
            types: vec![],
        }];
        assert!(build_source_closure_cte(&bindings).is_none());
    }

    #[test]
    fn single_binding_cte() {
        let bindings = vec![SourceClosureBinding {
            source_id: "s1".into(),
            types: vec!["person".into(), "researcher".into()],
        }];
        let (cte, params) = build_source_closure_cte(&bindings).unwrap();
        assert_eq!(params, vec!["s1"]);
        assert!(cte.contains("$1::text AS source_id"));
        assert!(cte.contains("'person'"));
        assert!(cte.contains("'researcher'"));
        assert!(cte.contains("unnest(ARRAY["));
    }

    #[test]
    fn multiple_bindings_union_all() {
        let bindings = vec![
            SourceClosureBinding {
                source_id: "s2".into(),
                types: vec!["note".into()],
            },
            SourceClosureBinding {
                source_id: "s1".into(),
                types: vec!["person".into(), "researcher".into()],
            },
        ];
        let (cte, params) = build_source_closure_cte(&bindings).unwrap();
        // Sorted by source_id
        assert_eq!(params, vec!["s1", "s2"]);
        assert!(cte.contains("UNION ALL"));
        assert!(cte.contains("$1::text"));
        assert!(cte.contains("$2::text"));
    }

    #[test]
    fn skip_empty_type_bindings() {
        let bindings = vec![
            SourceClosureBinding {
                source_id: "s1".into(),
                types: vec![],
            },
            SourceClosureBinding {
                source_id: "s2".into(),
                types: vec!["note".into()],
            },
        ];
        let (cte, params) = build_source_closure_cte(&bindings).unwrap();
        // Only s2 has types
        assert_eq!(params, vec!["s2"]);
        assert!(!cte.contains("s1"));
    }

    // ---- escape_sql_literal ---------------------------------------------

    #[test]
    fn escape_simple_string() {
        let result = escape_sql_literal("person");
        assert_eq!(result, "'person'");
    }

    #[test]
    fn escape_single_quotes() {
        let result = escape_sql_literal("it's");
        assert_eq!(result, "'it''s'");
    }

    #[test]
    fn escape_empty_string() {
        let result = escape_sql_literal("");
        assert_eq!(result, "''");
    }
}
