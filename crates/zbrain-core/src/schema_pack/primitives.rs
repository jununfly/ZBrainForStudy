//! Five composable primitive defaults.
//!
//! A primitive is a named bundle of (default link verbs, default frontmatter
//! fields, expert-routing flag, enrichment rubric slot). Pack types extend
//! one primitive by name; the primitive's defaults flow through unless the
//! type overrides specific fields.
//!
//! Ported from TS `src/core/schema-pack/primitives.ts`.

use super::manifest::PackPrimitive;

/// Default values associated with a pack primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimitiveDefaults {
    /// Default link verbs the primitive emits in inferLinkType heuristics.
    pub default_link_verbs: Vec<&'static str>,
    /// Default frontmatter fields the inference layer recognizes.
    pub default_frontmatter_fields: Vec<&'static str>,
    /// Whether types under this primitive are expert-routing candidates by default.
    pub default_expert_routing: bool,
    /// Default enrichment rubric slot (consulted by enrichment-service).
    pub default_rubric: Option<&'static str>,
    /// Whether types under this primitive are facts-eligible by default.
    pub default_extractable: bool,
}

/// Lookup defaults for a primitive. Exhaustive over the closed enum.
pub fn get_primitive_defaults(p: PackPrimitive) -> PrimitiveDefaults {
    match p {
        PackPrimitive::Entity => PrimitiveDefaults {
            default_link_verbs: vec![
                "works_at", "founded", "mentions", "invested_in", "advises", "attended",
            ],
            default_frontmatter_fields: vec!["aliases", "email", "location", "role"],
            default_expert_routing: true,
            default_rubric: Some("entity-default"),
            default_extractable: true,
        },
        PackPrimitive::Media => PrimitiveDefaults {
            default_link_verbs: vec!["cites", "references", "authored_by"],
            default_frontmatter_fields: vec!["url", "source", "author", "date"],
            default_expert_routing: false,
            default_rubric: Some("media-default"),
            default_extractable: false,
        },
        PackPrimitive::Temporal => PrimitiveDefaults {
            default_link_verbs: vec!["attended", "occurred_at"],
            default_frontmatter_fields: vec!["date", "attendees", "duration", "location"],
            default_expert_routing: false,
            default_rubric: Some("temporal-default"),
            default_extractable: true,
        },
        PackPrimitive::Annotation => PrimitiveDefaults {
            default_link_verbs: vec!["claims", "sources_from"],
            default_frontmatter_fields: vec!["confidence", "valid_from", "source"],
            default_expert_routing: false,
            default_rubric: Some("annotation-default"),
            default_extractable: false,
        },
        PackPrimitive::Concept => PrimitiveDefaults {
            default_link_verbs: vec!["relates_to", "supersedes", "mentions"],
            default_frontmatter_fields: vec!["tags"],
            default_expert_routing: false,
            default_rubric: Some("concept-default"),
            default_extractable: false,
        },
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_defaults() {
        let d = get_primitive_defaults(PackPrimitive::Entity);
        assert_eq!(
            d.default_link_verbs,
            vec!["works_at", "founded", "mentions", "invested_in", "advises", "attended"]
        );
        assert!(d.default_expert_routing);
        assert!(d.default_extractable);
        assert_eq!(d.default_rubric, Some("entity-default"));
    }

    #[test]
    fn media_defaults() {
        let d = get_primitive_defaults(PackPrimitive::Media);
        assert!(!d.default_expert_routing);
        assert!(!d.default_extractable);
        assert_eq!(d.default_rubric, Some("media-default"));
    }

    #[test]
    fn temporal_defaults() {
        let d = get_primitive_defaults(PackPrimitive::Temporal);
        assert!(!d.default_expert_routing);
        assert!(d.default_extractable);
        assert_eq!(d.default_rubric, Some("temporal-default"));
    }

    #[test]
    fn annotation_defaults() {
        let d = get_primitive_defaults(PackPrimitive::Annotation);
        assert!(!d.default_expert_routing);
        assert!(!d.default_extractable);
        assert_eq!(d.default_rubric, Some("annotation-default"));
    }

    #[test]
    fn concept_defaults() {
        let d = get_primitive_defaults(PackPrimitive::Concept);
        assert!(!d.default_expert_routing);
        assert!(!d.default_extractable);
        assert_eq!(d.default_rubric, Some("concept-default"));
    }

    /// Every known primitive must return a non-empty link_verbs set and a
    /// rubric. This is a contract test — if a new primitive is added to the
    /// enum, this test forces the author to update get_primitive_defaults.
    #[test]
    fn all_primitives_exhaustive() {
        for &p in super::super::manifest::PACK_PRIMITIVES {
            let d = get_primitive_defaults(p);
            assert!(
                !d.default_link_verbs.is_empty(),
                "primitive {p:?} must have default link verbs"
            );
            assert!(
                d.default_rubric.is_some(),
                "primitive {p:?} must have a default rubric"
            );
        }
    }
}
