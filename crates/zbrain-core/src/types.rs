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

// ─── Slice 6a S6 helper types ───────────────────────────────────────────────
//
// These 5 structs are inputs/outputs for the 13 new `BrainEngine` methods
// landing in slice 6a S6 (see `docs/plans/20260526/13-slice-6a-gap-checklist.md`
// §13.1 + §13.3). They live in `types.rs` (not `engine.rs`) because they are
// pure value shapes — no behaviour, no trait dependency.

/// Query options for [`BrainEngine::find_duplicate_page`].
///
/// Mirrors `FindDuplicatePageOpts` in `src/core/pglite-engine.ts:815`.
/// `content_hash` is required (the primary dedup key);
/// `frontmatter_id` is optional and matched via `OR` so the page is
/// considered a duplicate if **either** identifier collides.
#[derive(Debug, Clone)]
pub struct FindDuplicatePageOpts {
    pub content_hash: String,
    pub frontmatter_id: Option<String>,
}

/// `(slug, source_id)` pair returned by [`BrainEngine::list_all_page_refs`]
/// and consumed by [`BrainEngine::get_effective_dates`] /
/// [`BrainEngine::get_salience_scores`] as the canonical addressing form.
///
/// Equivalent to the TS shape `{ slug: string; sourceId: string }` returned
/// by `pglite-engine.ts:2577`. Ordering convention: `(source_id, slug)`
/// ascending, matching the TS `ORDER BY` clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageRef {
    pub slug: String,
    pub source_id: String,
}

/// Result of [`BrainEngine::purge_deleted_pages`].
///
/// Mirrors the TS return `{ slugs: string[]; count: number }` at
/// `pglite-engine.ts:933`. Both are returned (vs just one) because the TS
/// callers consume both — `slugs` for cascade-cleanup notifications, `count`
/// for the audit log.
#[derive(Debug, Clone, Default)]
pub struct PurgeResult {
    pub slugs: Vec<String>,
    pub count: u64,
}

/// Aggregated args for [`BrainEngine::refresh_page_body`].
///
/// Mirrors the positional args of `pglite-engine.ts:948` (5 inputs:
/// `slug`, `sourceId`, `compiledTruth`, `timeline`, `contentHash`).
/// We use a struct rather than a 5-arg method because the rust-lang style
/// guide caps positional args at 4 for readability.
///
/// `timeline` is `serde_json::Value` because the TS source type is `any[]`
/// (event timeline objects with heterogeneous shapes per event source).
#[derive(Debug, Clone)]
pub struct RefreshPageBodyArgs {
    pub slug: String,
    pub source_id: String,
    pub compiled_truth: String,
    pub timeline: serde_json::Value,
    pub content_hash: String,
}

/// Row shape returned by [`BrainEngine::find_orphan_pages`].
///
/// Mirrors the TS return at `pglite-engine.ts:2619`: `{ slug, title, domain }`
/// where `title` falls back to `slug` via `COALESCE` and `domain` is
/// extracted from `frontmatter->>'domain'` (so it can be `NULL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanPage {
    pub slug: String,
    pub title: String,
    pub domain: Option<String>,
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
        assert_eq!(
            serde_json::to_string(&PageKind::Markdown).unwrap(),
            "\"markdown\""
        );
        assert_eq!(serde_json::to_string(&PageKind::Code).unwrap(), "\"code\"");
        assert_eq!(
            serde_json::to_string(&PageKind::Image).unwrap(),
            "\"image\""
        );
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
        assert!(
            bad.is_err(),
            "kebab-case must NOT parse — TS uses snake_case"
        );
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
