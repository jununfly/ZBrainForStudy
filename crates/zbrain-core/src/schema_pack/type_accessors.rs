//! Pure type-accessor helpers over a [`SchemaPackManifest`].
//!
//! Port of the small accessor modules under `src/core/schema-pack/`:
//! `expert-types.ts`, `extractable.ts`, `enrichable.ts`, `link-inference.ts`.
//!
//! Every function here is pure (no DB, no engine) and operates on a manifest
//! subset, so it is trivially unit-testable and reusable by both the CLI and
//! the inference pipeline.

use std::collections::HashSet;

use crate::schema_pack::manifest::SchemaPackManifest;
use crate::schema_pack::redos_guard::{PageRegexBudget, RedosOutcome};

/// Types whose `expert_routing` flag is set, in pack declaration order.
pub fn expert_types_from_pack(pack: &SchemaPackManifest) -> Vec<String> {
    pack.page_types
        .iter()
        .filter(|pt| pt.expert_routing)
        .map(|pt| pt.name.clone())
        .collect()
}

/// Like [`expert_types_from_pack`] but errors if no expert-routed type exists.
pub fn expert_types_from_pack_or_throw(pack: &SchemaPackManifest) -> crate::Result<Vec<String>> {
    let types = expert_types_from_pack(pack);
    if types.is_empty() {
        return Err(crate::Error::new(
            "SchemaPack",
            "no_expert_types",
            format!("pack '{}' declares no expert-routing page types", pack.name),
        ));
    }
    Ok(types)
}

/// Extractable type names as a set.
pub fn extractable_types_from_pack(pack: &SchemaPackManifest) -> HashSet<String> {
    pack.page_types
        .iter()
        .filter(|pt| pt.extractable)
        .map(|pt| pt.name.clone())
        .collect()
}

/// True if `type_name` is extractable in `pack`.
pub fn is_extractable_type(pack: &SchemaPackManifest, type_name: &str) -> bool {
    pack.page_types
        .iter()
        .any(|pt| pt.name == type_name && pt.extractable)
}

/// Enrichable type names as a set (from `enrichable_types`, not `page_types`).
pub fn enrichable_types_from_pack(pack: &SchemaPackManifest) -> HashSet<String> {
    pack.enrichable_types
        .iter()
        .map(|e| e.type_name.clone())
        .collect()
}

/// Rubric name declared for `type_name` in `pack.enrichable_types`, if any.
pub fn rubric_name_for_type(pack: &SchemaPackManifest, type_name: &str) -> Option<String> {
    pack.enrichable_types
        .iter()
        .find(|e| e.type_name == type_name)
        .and_then(|e| e.rubric.clone())
}

/// Infer a link type for `page_type` given `context` text, under `budget`.
///
/// Pass 1 (deterministic): a link type whose `inference.page_type` equals
/// `page_type` wins immediately.
/// Pass 2 (regex): each link type with an `inference.regex` is tried in
/// **lexicographic** order (deterministic degrade order — callers must
/// pre-sort the pack's link types) under the ReDoS budget; first match wins.
/// Pass 3: if nothing matches, returns `None` (the caller falls through to a
/// legacy default such as `mentions`).
pub fn infer_link_type_from_pack(
    pack: &SchemaPackManifest,
    page_type: &str,
    context: &str,
    budget: &mut PageRegexBudget,
) -> Option<String> {
    // Pass 1: explicit page_type match.
    for lt in &pack.link_types {
        if let Some(inf) = &lt.inference {
            if inf.page_type.as_deref() == Some(page_type) {
                return Some(lt.name.clone());
            }
        }
    }

    // Pass 2: regex matchers, lexicographic order for determinism.
    let mut regex_link_types: Vec<&crate::schema_pack::manifest::LinkTypeDefinition> = pack
        .link_types
        .iter()
        .filter(|lt| {
            lt.inference
                .as_ref()
                .and_then(|i| i.regex.as_deref())
                .is_some()
        })
        .collect();
    regex_link_types.sort_by(|a, b| a.name.cmp(&b.name));

    for lt in regex_link_types {
        let pattern = lt
            .inference
            .as_ref()
            .and_then(|i| i.regex.clone())
            .unwrap_or_default();
        match budget.run_bounded(&lt.name, &pattern, context) {
            RedosOutcome::Exhausted => return None,
            RedosOutcome::Matched(_) => return Some(lt.name.clone()),
            RedosOutcome::NoMatch => continue,
        }
    }

    None
}

/// Resolve a frontmatter-link type for `field_name` on `page_type`, if the
/// pack declares one. First matching entry wins.
pub fn frontmatter_link_type_from_pack(
    pack: &SchemaPackManifest,
    page_type: &str,
    field_name: &str,
) -> Option<String> {
    pack.frontmatter_links
        .iter()
        .find(|fl| fl.page_type == page_type && fl.fields.iter().any(|f| f == field_name))
        .map(|fl| fl.link_type.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_pack::manifest::{
        EnrichableType, FrontmatterLinkDefinition, LinkInference, LinkTypeDefinition,
        PageTypeDefinition, PackPrimitive,
    };

    fn pt(name: &str, extractable: bool, expert: bool) -> PageTypeDefinition {
        PageTypeDefinition {
            name: name.to_string(),
            primitive: PackPrimitive::Entity,
            path_prefixes: vec![],
            aliases: vec![],
            extractable,
            expert_routing: expert,
        }
    }

    fn manifest_with(types: Vec<PageTypeDefinition>) -> SchemaPackManifest {
        SchemaPackManifest {
            page_types: types,
            ..Default::default()
        }
    }

    #[test]
    fn expert_types_filter() {
        let m = manifest_with(vec![
            pt("person", false, true),
            pt("note", true, false),
            pt("company", false, true),
        ]);
        let got = expert_types_from_pack(&m);
        assert_eq!(got, vec!["person", "company"]);
    }

    #[test]
    fn expert_types_or_throw() {
        let m = manifest_with(vec![pt("note", true, false)]);
        assert!(expert_types_from_pack_or_throw(&m).is_err());
        let m2 = manifest_with(vec![pt("person", false, true)]);
        assert_eq!(expert_types_from_pack_or_throw(&m2).unwrap(), vec!["person"]);
    }

    #[test]
    fn extractable_set_and_check() {
        let m = manifest_with(vec![pt("note", true, false), pt("person", false, true)]);
        let set = extractable_types_from_pack(&m);
        assert!(set.contains("note"));
        assert!(!set.contains("person"));
        assert!(is_extractable_type(&m, "note"));
        assert!(!is_extractable_type(&m, "person"));
    }

    #[test]
    fn enrichable_and_rubric() {
        let m = SchemaPackManifest {
            enrichable_types: vec![
                EnrichableType {
                    type_name: "person".to_string(),
                    rubric: Some("people-rubric".to_string()),
                },
                EnrichableType {
                    type_name: "company".to_string(),
                    rubric: None,
                },
            ],
            ..Default::default()
        };
        let set = enrichable_types_from_pack(&m);
        assert!(set.contains("person"));
        assert!(set.contains("company"));
        assert_eq!(rubric_name_for_type(&m, "person"), Some("people-rubric".to_string()));
        assert_eq!(rubric_name_for_type(&m, "company"), None);
        assert_eq!(rubric_name_for_type(&m, "missing"), None);
    }

    #[test]
    fn infer_pass1_page_type() {
        let m = SchemaPackManifest {
            link_types: vec![LinkTypeDefinition {
                name: "authored".to_string(),
                inverse: None,
                inference: Some(LinkInference {
                    regex: None,
                    page_type: Some("person".to_string()),
                    target_type: None,
                }),
            }],
            ..Default::default()
        };
        let mut b = PageRegexBudget::new();
        assert_eq!(infer_link_type_from_pack(&m, "person", "irrelevant", &mut b), Some("authored".to_string()));
    }

    #[test]
    fn infer_pass2_regex_sorted() {
        let mut m = SchemaPackManifest {
            // Deliberately out of order to verify lexicographic degrade.
            link_types: vec![
                LinkTypeDefinition {
                    name: "zeta".to_string(),
                    inverse: None,
                    inference: Some(LinkInference {
                        regex: Some(r"Z.+".to_string()),
                        page_type: None,
                        target_type: None,
                    }),
                },
                LinkTypeDefinition {
                    name: "alpha".to_string(),
                    inverse: None,
                    inference: Some(LinkInference {
                        regex: Some(r"A.+".to_string()),
                        page_type: None,
                        target_type: None,
                    }),
                },
            ],
            ..Default::default()
        };
        let mut b = PageRegexBudget::new();
        // context "Apple" matches both A.+ and Z.+? No: "Apple" matches A.+ not Z.+
        // Use context that matches only alpha's pattern.
        assert_eq!(infer_link_type_from_pack(&m, "x", "Apple", &mut b), Some("alpha".to_string()));
        let _ = &mut m;
    }

    #[test]
    fn infer_none_when_no_match() {
        let m = SchemaPackManifest {
            link_types: vec![LinkTypeDefinition {
                name: "alpha".to_string(),
                inverse: None,
                inference: Some(LinkInference {
                    regex: Some(r"A.+".to_string()),
                    page_type: None,
                    target_type: None,
                }),
            }],
            ..Default::default()
        };
        let mut b = PageRegexBudget::new();
        assert_eq!(infer_link_type_from_pack(&m, "x", "zzz", &mut b), None);
    }

    #[test]
    fn frontmatter_link_lookup() {
        let m = SchemaPackManifest {
            frontmatter_links: vec![FrontmatterLinkDefinition {
                page_type: "person".to_string(),
                fields: vec!["employer".to_string(), "investor".to_string()],
                link_type: "works_at".to_string(),
            }],
            ..Default::default()
        };
        assert_eq!(
            frontmatter_link_type_from_pack(&m, "person", "employer"),
            Some("works_at".to_string())
        );
        assert_eq!(
            frontmatter_link_type_from_pack(&m, "person", "investor"),
            Some("works_at".to_string())
        );
        assert_eq!(frontmatter_link_type_from_pack(&m, "person", "spouse"), None);
        assert_eq!(frontmatter_link_type_from_pack(&m, "company", "employer"), None);
    }
}
