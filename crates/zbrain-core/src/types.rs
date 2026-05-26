//! Core type primitives shared across the engine.
//!
//! Slice 2 scope: pure enums + constants only. The DB-backed entities (`Page`,
//! `PageInput`, `Chunk`, ...) land in slice 4 alongside the storage abstraction
//! so we can co-design the sqlx mapping in one go. See
//! `docs/plans/20260526/04-plan.md`.
//!
//! Wire shape rules (preserved from `src/core/types.ts`):
//!
//! * `PageType` is open — it serializes as a plain string. We expose
//!   [`ALL_PAGE_TYPES`] as the seed list `gbrain-base` declares; runtime
//!   schema-pack validation owns the closed set per the v0.38 contract.
//! * `PageKind`, [`CRMode`], [`EffectiveDateSource`] are closed enums and
//!   serialize as kebab-/snake-case strings matching the TS values.

use serde::{Deserialize, Serialize};

/// Open page-type alias. Pre-v0.38 this was a closed union of 23 strings;
/// v0.38 schema packs took validation runtime-side, so the type system here
/// just reflects "any string". Use [`is_base_page_type`] to check membership
/// in the gbrain-base seed list.
pub type PageType = String;

/// Seed list of types declared by the built-in `gbrain-base` schema pack.
///
/// **NOT** exhaustive — schema packs can add their own types via manifest.
/// Ordering matches the TS `ALL_PAGE_TYPES` array byte-for-byte so codegen
/// referencing this list stays cross-rewrite stable.
pub const ALL_PAGE_TYPES: &[&str] = &[
    "person",
    "company",
    "deal",
    "yc",
    "civic",
    "project",
    "concept",
    "source",
    "media",
    "writing",
    "analysis",
    "guide",
    "hardware",
    "architecture",
    "meeting",
    "note",
    "email",
    "slack",
    "calendar-event",
    // v0.41.11+
    "conversation",
    "atom",
    "code",
    "image",
    "synthesis",
];

/// Whether `value` is one of the base seed page types declared by `gbrain-base`.
#[must_use]
pub fn is_base_page_type(value: &str) -> bool {
    ALL_PAGE_TYPES.contains(&value)
}

/// Multimodal ingestion path classifier (parallel to markdown + code).
///
/// Wire values: `"markdown"`, `"code"`, `"image"`. Closed enum on purpose —
/// the embedding pipeline branches on these three only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageKind {
    Markdown,
    Code,
    Image,
}

/// Contextual-retrieval tier ladder per `search.mode` (v0.40.3.0).
///
/// Wire values match TS `CR_MODES` exactly:
/// `"none"`, `"title"`, `"per_chunk_synopsis"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CRMode {
    /// No wrapper applied at embed time (conservative).
    None,
    /// `<context>{title}</context>\n{chunk}` (balanced).
    Title,
    /// Per-chunk Haiku synopsis prepended (tokenmax).
    PerChunkSynopsis,
}

/// Which precedence step won when computing a page's effective date (v0.29.1).
///
/// Wire values: `"event_date"`, `"date"`, `"published"`, `"filename"`,
/// `"fallback"` — same as TS `EffectiveDateSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveDateSource {
    EventDate,
    Date,
    Published,
    Filename,
    Fallback,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ALL_PAGE_TYPES ----------------------------------------------------

    #[test]
    fn all_page_types_count_matches_ts() {
        // 19 pre-v0.41.11 + 2 (conversation, atom) + 3 (code, image, synthesis)
        // = 24 entries. Pinning the count guards against accidental drift.
        assert_eq!(ALL_PAGE_TYPES.len(), 24);
    }

    #[test]
    fn all_page_types_first_and_last_anchor() {
        // First and last entries — cheap smoke for ordering.
        assert_eq!(ALL_PAGE_TYPES.first(), Some(&"person"));
        assert_eq!(ALL_PAGE_TYPES.last(), Some(&"synthesis"));
    }

    #[test]
    fn all_page_types_includes_v041_11_additions() {
        assert!(is_base_page_type("conversation"));
        assert!(is_base_page_type("atom"));
    }

    #[test]
    fn is_base_page_type_rejects_unknown() {
        assert!(!is_base_page_type(""));
        assert!(!is_base_page_type("apple-note")); // organic non-base type
        assert!(!is_base_page_type("PERSON")); // case-sensitive
    }

    // --- PageKind ---------------------------------------------------------

    #[test]
    fn page_kind_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&PageKind::Markdown).unwrap(), "\"markdown\"");
        assert_eq!(serde_json::to_string(&PageKind::Code).unwrap(), "\"code\"");
        assert_eq!(serde_json::to_string(&PageKind::Image).unwrap(), "\"image\"");
    }

    #[test]
    fn page_kind_roundtrip() {
        for k in [PageKind::Markdown, PageKind::Code, PageKind::Image] {
            let s = serde_json::to_string(&k).unwrap();
            let back: PageKind = serde_json::from_str(&s).unwrap();
            assert_eq!(k, back);
        }
    }

    // --- CRMode -----------------------------------------------------------

    #[test]
    fn cr_mode_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&CRMode::None).unwrap(), "\"none\"");
        assert_eq!(serde_json::to_string(&CRMode::Title).unwrap(), "\"title\"");
        assert_eq!(
            serde_json::to_string(&CRMode::PerChunkSynopsis).unwrap(),
            "\"per_chunk_synopsis\""
        );
    }

    #[test]
    fn cr_mode_rejects_unknown() {
        let bad: serde_json::Result<CRMode> = serde_json::from_str("\"per-chunk-synopsis\"");
        assert!(bad.is_err(), "kebab-case must NOT parse — TS uses snake_case");
    }

    // --- EffectiveDateSource ----------------------------------------------

    #[test]
    fn effective_date_source_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&EffectiveDateSource::EventDate).unwrap(),
            "\"event_date\""
        );
        assert_eq!(
            serde_json::to_string(&EffectiveDateSource::Fallback).unwrap(),
            "\"fallback\""
        );
    }

    #[test]
    fn effective_date_source_full_roundtrip() {
        for s in [
            EffectiveDateSource::EventDate,
            EffectiveDateSource::Date,
            EffectiveDateSource::Published,
            EffectiveDateSource::Filename,
            EffectiveDateSource::Fallback,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: EffectiveDateSource = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }
}
