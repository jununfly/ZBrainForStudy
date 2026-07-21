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
use serde_json::Value;

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

/// Minimal duplicate-page reference returned by [`BrainEngine::find_duplicate_page`].
///
/// Mirrors the TS return shape `{ slug: string; id: number } | null` from
/// `BrainEngine.findDuplicatePage`. Duplicate detection intentionally returns
/// only the row identity needed by import deduplication, not a full [`Page`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicatePage {
    pub slug: String,
    pub id: u64,
}

/// `(slug, source_id)` pair returned by [`BrainEngine::list_all_page_refs`]
/// and consumed by [`BrainEngine::get_effective_dates`] /
/// [`BrainEngine::get_salience_scores`] as the canonical addressing form.
///
/// Equivalent to the TS shape `{ slug: string; sourceId: string }` returned
/// by `pglite-engine.ts:2577`. Ordering convention: `(source_id, slug)`
/// ascending, matching the TS `ORDER BY` clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, Default, serde::Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanPage {
    pub slug: String,
    pub title: String,
    pub domain: Option<String>,
}

/// File metadata row returned by [`BrainEngine::get_file`] and
/// [`BrainEngine::list_files_for_page`]. Mirrors TS `FileRow` in
/// `src/core/engine.ts`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub id: u64,
    pub source_id: String,
    pub page_slug: Option<String>,
    pub page_id: Option<u64>,
    pub filename: String,
    pub storage_path: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub content_hash: String,
    pub metadata: Value,
    pub created_at: String,
}

/// File metadata write spec for [`BrainEngine::upsert_file`]. Mirrors TS
/// `FileSpec` in `src/core/engine.ts`. File bytes never enter the DB;
/// `storage_path` points to repo/external storage.
#[derive(Debug, Clone, PartialEq)]
pub struct FileSpec {
    pub source_id: Option<String>,
    pub page_slug: Option<String>,
    pub page_id: Option<u64>,
    pub filename: String,
    pub storage_path: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub content_hash: String,
    pub metadata: Option<Value>,
}

/// Result of [`BrainEngine::upsert_file`]. Mirrors TS
/// `Promise<{ id: number; created: boolean }>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpsertFileResult {
    pub id: u64,
    pub created: bool,
}

// ── 1-6-7-5: ingestion + files read-side types ──────────────────────────────

/// A single content chunk read by [`BrainEngine::get_chunks`]. Mirrors TS
/// `Chunk` (src/core/types.ts). `embedding` is always omitted on read
/// (the TS `getChunks` path sets `includeEmbedding=false`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    pub page_id: i64,
    pub chunk_index: i64,
    pub chunk_text: String,
    pub chunk_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_symbol_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name_qualified: Option<String>,
    pub created_at: String,
}

/// One row from the `ingest_log` table, returned by
/// [`BrainEngine::get_ingest_log`]. Mirrors TS `IngestLogEntry`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestLogEntry {
    pub id: i64,
    pub source_id: String,
    pub source_type: String,
    pub source_ref: String,
    pub pages_updated: Vec<String>,
    pub summary: String,
    pub created_at: String,
}

/// Input for [`BrainEngine::log_ingest`]. Mirrors TS `logIngest` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestLogInput {
    pub source_id: String,
    pub source_type: String,
    pub source_ref: String,
    pub pages_updated: Vec<String>,
    pub summary: String,
}

/// A file metadata row as returned by [`BrainEngine::list_files`] — the 8
/// columns selected by the TS `file_list` op (id, page_slug, filename,
/// storage_path, mime_type, size_bytes, content_hash, created_at).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileListRow {
    pub id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_slug: Option<String>,
    pub filename: String,
    pub storage_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    pub content_hash: String,
    pub created_at: String,
}

/// A recent transcript entry returned by the `get_recent_transcripts` op.
/// Mirrors TS `RecentTranscript` (src/core/transcripts.ts).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentTranscript {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub mtime: String,
    pub length: i64,
    pub summary: String,
}

/// Raw sidecar data returned by [`BrainEngine::get_raw_data`]. Mirrors TS
/// `RawData` in `src/core/engine.ts`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawData {
    pub source: String,
    pub data: serde_json::Value,
    pub fetched_at: String,
}

/// Page version snapshot returned by [`BrainEngine::get_versions`] and
/// [`BrainEngine::create_version`]. Mirrors TS `PageVersion` in
/// `src/core/engine.ts`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageVersion {
    pub id: u64,
    pub page_id: u64,
    pub compiled_truth: String,
    pub frontmatter: Value,
    pub snapshot_at: String,
}

/// A single take record as stored in the `takes` table (read shape).
/// Mirrors TS `ParsedTake` in `src/core/takes-fence.ts`.
///
/// Unlike the TS fence model where `active` starts true and strikethrough
/// makes it false, the DB `active` column defaults to TRUE and is set FALSE
/// on supersede or manual deactivation.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Take {
    pub id: u64,
    pub page_id: u64,
    pub row_num: i32,
    pub claim: String,
    pub kind: String,
    pub holder: String,
    pub weight: f64,
    pub since_date: Option<String>,
    pub until_date: Option<String>,
    pub source: Option<String>,
    pub superseded_by: Option<i32>,
    pub active: bool,
    pub resolved_at: Option<String>,
    pub resolved_quality: Option<String>,
    pub resolved_outcome: Option<bool>,
    pub resolved_evidence: Option<String>,
    pub resolved_value: Option<f64>,
    pub resolved_unit: Option<String>,
    pub resolved_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Write spec for creating/upserting a single take. Mirrors the INSERT
/// columns used in TS postgres-engine.ts:3365.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeInput {
    pub page_id: u64,
    pub row_num: Option<i32>,
    pub claim: String,
    pub kind: String,
    pub holder: String,
    pub weight: f64,
    pub since_date: Option<String>,
    pub until_date: Option<String>,
    pub source: Option<String>,
    pub superseded_by: Option<i32>,
    pub active: Option<bool>,
}

/// Write spec for resolving a take (v0.30+ quality/outcome fields).
#[derive(Debug, Clone, PartialEq)]
pub struct TakeResolution {
    pub page_id: u64,
    pub row_num: i32,
    pub quality: Option<String>,
    pub outcome: Option<bool>,
    pub evidence: Option<String>,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub by: Option<String>,
}

/// Options for [`crate::engine::BrainEngine::list_takes`].
///
/// Mirrors TS `TakesListOpts`. The `takes_holders_allow_list` field is the
/// server-side filter backing the v0.28+ per-token visibility model: when
/// set, the engine applies `WHERE holder = ANY($allow_list)` on top of the
/// other predicates. `None` disables the filter (trusted local callers).
#[derive(Debug, Clone, Default)]
pub struct TakesListOpts {
    pub page_id: Option<u64>,
    pub holder: Option<String>,
    pub kind: Option<String>,
    pub active: Option<bool>,
    pub resolved: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub takes_holders_allow_list: Option<Vec<String>>,
}

/// Options for [`crate::engine::BrainEngine::search_takes`].
#[derive(Debug, Clone, Default)]
pub struct SearchTakesOpts {
    pub limit: Option<u32>,
    /// Per-token allow-list for the `holder` field (v0.28). When set, the
    /// engine applies `WHERE holder = ANY($allow_list)`.
    pub takes_holders_allow_list: Option<Vec<String>>,
}

/// A single search hit from `search_takes` (claim text match + score).
/// Mirrors TS `TakeHit`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeHit {
    pub take_id: u64,
    pub page_id: u64,
    pub page_slug: String,
    pub row_num: i32,
    pub claim: String,
    pub kind: String,
    pub holder: String,
    pub weight: f64,
    pub score: f64,
}

/// Canonical take kinds seeded from gbrain-base. Mirrors TS `TakeKind`
/// before v0.38 opened it to `string`. The DB CHECK constraint
/// `takes_kind_valid` enforces this closed set; the Rust layer accepts
/// any `String` to stay forward-compat with schema-pack extensions.
pub const SEED_TAKE_KINDS: &[&str] = &["fact", "take", "bet", "hunch"];

/// Result of a batch takes upsert operation.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertTakesResult {
    pub upserted: usize,
    pub weight_clamped: usize,
}

// ─── Link / Graph types (Phase 7B) ────────────────────────────────────────

/// A single link (directed edge) between two pages. Mirrors TS `Link` in
/// `src/core/types.ts`. Returned by `get_links` / `get_backlinks` — carries
/// slug references (not page IDs) so callers don't need a second lookup.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub from_slug: String,
    pub to_slug: String,
    pub link_type: String,
    pub context: String,
    pub link_source: Option<String>,
    pub origin_slug: Option<String>,
    pub origin_field: Option<String>,
}

/// Write spec for creating a single link. Mirrors TS `LinkBatchInput` in
/// `src/core/engine.ts`. All optional fields default per the TS convention:
/// `link_type` → `""`, `link_source` → `"markdown"` on new writes.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkBatchInput {
    pub from_slug: String,
    pub to_slug: String,
    pub link_type: Option<String>,
    pub context: Option<String>,
    pub link_source: Option<String>,
    pub origin_slug: Option<String>,
    pub origin_field: Option<String>,
    pub from_source_id: Option<String>,
    pub to_source_id: Option<String>,
    pub origin_source_id: Option<String>,
}

/// A page in a graph traversal with its outgoing edges. Mirrors TS
/// `GraphNode` in `src/core/types.ts`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeLink {
    pub to_slug: String,
    pub link_type: String,
}

/// A page node in a graph traversal result. Mirrors TS `GraphNode` in
/// `src/core/types.ts`. Field `rtype` serializes as `type` to match the TS
/// wire contract while avoiding the Rust keyword conflict.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub slug: String,
    pub title: String,
    #[serde(rename = "type")]
    pub page_type: String,
    pub depth: u32,
    pub links: Vec<GraphNodeLink>,
}

/// A single edge in a graph path traversal. Mirrors TS `GraphPath` in
/// `src/core/types.ts`. Carries both endpoint slugs, edge type, context,
/// and the depth of `to_slug` from the traversal root.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphPath {
    pub from_slug: String,
    pub to_slug: String,
    pub link_type: String,
    pub context: String,
    pub depth: u32,
}

/// Salience query result. Returned by `BrainEngine::get_recent_salience`.
/// Mirrors TS `SalienceResult` in `src/core/types.ts` (v0.29.1).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SalienceResult {
    pub slug: String,
    pub source_id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub page_type: PageType,
    pub updated_at: String,
    pub emotional_weight: f64,
    pub take_count: u32,
    pub take_avg_weight: f64,
    pub score: f64,
}

/// Adjacency aggregates for a single page within a subgraph induced by
/// an input set. Returned by `BrainEngine::getAdjacencyBoosts`. Mirrors
/// TS `AdjacencyRow` in `src/core/types.ts` (v0.40.4).
///
/// Cross-source semantics (mirrors TS JSDoc D15=A):
/// - `hits`: distinct from_page_id count, restricted to the input set.
/// - `cross_source_hits`: distinct OTHER source_id count, restricted to
///   the input set, EXCLUDING the target page's own source. A page in
///   source A linked from 2 pages in source A reports 0. Linked from 1
///   in source B + 1 in source C reports 2.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdjacencyRow {
    pub hits: u32,
    pub cross_source_hits: u32,
}

// ---------------------------------------------------------------------------
// Facts domain types (Phase 7B engine layer)
// ---------------------------------------------------------------------------
// Mirrors TS types in src/core/engine.ts:
//   FactKind    (L399), FactVisibility (L406), FactInsertStatus (L409),
//   FactRow     (L412), NewFact        (L442), FactsHealth     (L492),
//   FactListOpts(L477)

/// Fact claim kind. Mirrors TS `FactKind` union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactKind {
    Event,
    Preference,
    Commitment,
    Belief,
    Fact,
}

impl std::fmt::Display for FactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FactKind::Event => write!(f, "event"),
            FactKind::Preference => write!(f, "preference"),
            FactKind::Commitment => write!(f, "commitment"),
            FactKind::Belief => write!(f, "belief"),
            FactKind::Fact => write!(f, "fact"),
        }
    }
}

/// Fact visibility level. Mirrors TS `FactVisibility` union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactVisibility {
    Private,
    World,
}

impl std::fmt::Display for FactVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FactVisibility::Private => write!(f, "private"),
            FactVisibility::World => write!(f, "world"),
        }
    }
}

/// Result of an insertFact call. Mirrors TS `FactInsertStatus`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactInsertStatus {
    Inserted,
    Duplicate,
    Superseded,
}

/// A single row read from the `facts` table. Mirrors TS `FactRow` interface.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FactRow {
    pub id: i64,
    pub source_id: String,
    pub entity_slug: Option<String>,
    pub fact: String,
    pub kind: FactKind,
    pub visibility: FactVisibility,
    pub notability: String,
    pub context: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub expired_at: Option<String>,
    pub superseded_by: Option<i64>,
    pub consolidated_at: Option<String>,
    pub consolidated_into: Option<i64>,
    pub source: String,
    pub source_session: Option<String>,
    pub confidence: f64,
    pub created_at: Option<String>,
}

/// Input for `insertFact`. Mirrors TS `NewFact` interface.
/// All optional fields have sensible defaults applied by the implementation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFact {
    pub fact: String,
    pub kind: Option<FactKind>,
    pub entity_slug: Option<String>,
    pub visibility: Option<FactVisibility>,
    pub context: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub source: String,
    pub source_session: Option<String>,
    pub confidence: Option<f64>,
    pub notability: Option<String>,
    pub claim_metric: Option<String>,
    pub claim_value: Option<f64>,
    pub claim_unit: Option<String>,
    pub claim_period: Option<String>,
    pub event_type: Option<String>,
}

/// Operational health snapshot for the facts domain.
/// Mirrors TS `FactsHealth` interface.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FactsHealth {
    pub source_id: String,
    pub total_active: i64,
    pub total_today: i64,
    pub total_week: i64,
    pub total_expired: i64,
    pub total_consolidated: i64,
    pub top_entities: Vec<EntityCount>,
}

/// An entity slug + fact count pair used in `FactsHealth.top_entities`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityCount {
    pub entity_slug: String,
    pub count: i64,
}

/// Query filter options for listing facts. Mirrors TS `FactListOpts`.
#[derive(Debug, Clone, Default)]
pub struct FactListOpts {
    pub active_only: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub kinds: Option<Vec<FactKind>>,
    pub visibility: Option<Vec<FactVisibility>>,
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

    // --- Link types (Phase 7B) ---------------------------------------------

    #[test]
    fn link_serializes_camel_case() {
        let link = Link {
            from_slug: "people/alice".into(),
            to_slug: "companies/acme".into(),
            link_type: "works_at".into(),
            context: "Alice works at Acme".into(),
            link_source: Some("frontmatter".into()),
            origin_slug: Some("people/alice".into()),
            origin_field: Some("company".into()),
        };
        let json = serde_json::to_string(&link).unwrap();
        assert!(json.contains("\"fromSlug\""));
        assert!(json.contains("\"toSlug\""));
        assert!(json.contains("\"linkType\""));
        assert!(json.contains("\"linkSource\""));
        assert!(json.contains("\"originSlug\""));
        assert!(json.contains("\"originField\""));
    }

    #[test]
    fn graph_node_serializes_type_field() {
        let node = GraphNode {
            slug: "companies/acme".into(),
            title: "Acme Corp".into(),
            page_type: "company".into(),
            depth: 1,
            links: vec![GraphNodeLink {
                to_slug: "people/alice".into(),
                link_type: "works_at".into(),
            }],
        };
        let json = serde_json::to_string(&node).unwrap();
        // $type is the Rust keyword workaround — wire must say "type"
        assert!(json.contains("\"type\":\"company\""));
        assert!(json.contains("\"depth\":1"));
        assert!(json.contains("\"toSlug\":\"people/alice\""));
    }

    #[test]
    fn graph_path_serializes_camel_case() {
        let path = GraphPath {
            from_slug: "people/alice".into(),
            to_slug: "companies/acme".into(),
            link_type: "works_at".into(),
            context: "Alice works at Acme Corp".into(),
            depth: 2,
        };
        let json = serde_json::to_string(&path).unwrap();
        assert!(json.contains("\"fromSlug\""));
        assert!(json.contains("\"toSlug\""));
        assert!(json.contains("\"linkType\""));
        assert!(json.contains("\"depth\":2"));
    }
}
