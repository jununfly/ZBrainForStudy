//! Slice 3 — `BrainEngine` trait skeleton + in-memory mock.
//!
//! Slice 6a S2 expanded `Page` / `PageInput` / `PageFilters` to mirror the
//! full TS shape (`src/core/types.ts:73|199|277`) so that the libsql and
//! postgres engines have a place to land every column the 0002 schema exposes.
//!
//! Wider method groups (chunks, links, takes, facts, timeline, config,
//! migrations, eval, emotional) are intentionally deferred to later slices
//! so this trait boundary stays reviewable in a single PR.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use erased_serde::Serialize;
use serde_json::{json, Map, Value};

use crate::{
    calibration_queries::{aggregate_calibration_curve, aggregate_scorecard, CalibrationBucket,
        CalibrationCurveQuery, CalibrationProfileRow, CalibrationQueries, CalibrationRow,
        CalibrationWaveQueries, PatternDetail, ScorecardQuery, ScorecardRow, TakeSummary,
        TakesScorecard},
    oauth_queries::{ExchangeTokens, OAuthClientInfo, OAuthQueries, RegisterClientRequest,
        RegisterClientResponse, RevokeClientResponse, UpdateClientTtlResponse},
    time::current_utc_iso8601, types::PageVersion, types::RawData, types::Take,
    types::TakeHit, types::TakeInput, types::TakeResolution, types::TakesListOpts,
    types::SearchTakesOpts, types::UpsertTakesResult, CRMode, DuplicatePage,
    EffectiveDateSource, Error, EntityCount, FactInsertStatus, FactKind, FactListOpts, FactRow,
    FactVisibility, FactsHealth, FileRow, FileSpec, FindDuplicatePageOpts, GraphNode, GraphPath,
    AdjacencyRow, Link, LinkBatchInput, NewFact, OrphanPage, PageKind, PageRef, PageType, PurgeResult,
    RefreshPageBodyArgs, UpsertFileResult, Chunk, FileListRow, IngestLogEntry, IngestLogInput,
};

// ─── Value types ─────────────────────────────────────────────────────────────

/// Input row for [`BrainEngine::add_synthesis_evidence`].
/// Mirrors TS `SynthesisEvidenceInput` (think/index.ts:186).
#[derive(Debug, Clone)]
pub struct SynthesisEvidenceInput {
    /// Page id of the synthesis page (type='synthesis').
    pub synthesis_page_id: i64,
    /// Page id of the cited take page.
    pub take_page_id: i64,
    /// Row number within the take page; `None` for page-level citations
    /// (which are NOT persisted - synthesis_evidence is a take->synthesis FK).
    pub take_row_num: Option<i32>,
    /// Ordinal index of the citation within the answer.
    pub citation_index: i32,
}

/// Discriminates engine backend. Lets migrations / diagnostics branch without
/// `instanceof` / dynamic dispatch tricks. Mirrors the TS `'postgres'|'pglite'`
/// literal union; `InMemory` is Rust-only (test double).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Postgres,
    Libsql,
    /// In-process mock used in unit tests.
    InMemory,
}

/// Map an [`EngineKind`] to the stable string the thin-client banner and
/// `get_brain_identity` packet expose. Mirrors the TS `'postgres' | 'pglite'`
/// union; the Rust-only `InMemory` test double renders as `inmemory`.
pub fn engine_kind_str(k: EngineKind) -> &'static str {
    match k {
        EngineKind::Postgres => "postgres",
        EngineKind::Libsql => "pglite",
        EngineKind::InMemory => "inmemory",
    }
}

/// Thin-client banner identity packet (mirrors TS `get_brain_identity`).
///
/// Read-scope, banner-only: lets a thin client learn which engine backs its
/// brain and roughly how much content lives there, without any content access.
/// `page_count` / `chunk_count` default to 0 and are populated only by backends
/// that expose admin stats (Libsql); `last_sync_iso` is always `None` in this
/// port (the TS source never populated it either).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainIdentity {
    pub version: String,
    pub engine: String,
    pub page_count: i64,
    pub chunk_count: i64,
    pub last_sync_iso: Option<String>,
}

/// Startup configuration passed to [`BrainEngine::connect`].
/// Mirrors `EngineConfig` in `src/core/types.ts:1285`.
#[derive(Debug, Default, Clone)]
pub struct EngineConfig {
    pub database_url: Option<String>,
    pub database_path: Option<String>,
}

/// A persisted brain page. Mirrors `Page` in `src/core/types.ts:73`.
///
/// Slice 6a S2 expanded the struct to carry the 0002 projection (24 fields).
/// Slice 6a S5 added the last five columns — `last_retrieved_at`,
/// `generation`, `embedding`, `chunker_version`, `source_path` — bringing
/// the total to **29 fields**, matching the full TS shape after the 0003
/// migration. TS `Date` fields are `String` (ISO-8601) to avoid depending
/// on `chrono` in this slice; `frontmatter` is `serde_json::Value`
/// (TEXT-stored JSON in `SQLite`, JSONB in Postgres).
///
/// `Eq` is intentionally dropped because `emotional_weight: Option<f64>` and
/// `frontmatter: Value` do not implement `Eq`.
///
/// `Deserialize` is derived (symmetric with `Serialize`) so the CLI
/// `--explain` path can round-trip a `QueryOutput` back out of the
/// `run_operation` `serde_json::Value` — see `operation::QueryResultItem`.
/// All fields are deserializable (the three nested enums already derive it).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    // ── identity (always present) ────────────────────────────────────────
    pub id: u64,
    pub slug: String,
    pub page_type: PageType,
    pub page_kind: PageKind,
    pub title: String,
    pub compiled_truth: String,
    pub timeline: String,

    // ── content metadata ─────────────────────────────────────────────────
    /// Free-form JSON frontmatter stored as TEXT (`SQLite`) / JSONB (Postgres).
    pub frontmatter: Value,
    /// Content hash (e.g. `sha256:…`). NULL until the importer computes it.
    pub content_hash: Option<String>,
    /// Deterministic 0..1 emotional-weight score. NULL until the recompute cycle.
    pub emotional_weight: Option<f64>,

    // ── timestamps ───────────────────────────────────────────────────────
    /// ISO-8601 `created_at`. Always set by the DB (`DEFAULT CURRENT_TIMESTAMP`).
    pub created_at: String,
    /// ISO-8601 `updated_at`. Always set by the DB.
    pub updated_at: String,
    /// Soft-delete marker. `None` = live; `Some(ts)` = pending purge.
    pub deleted_at: Option<String>,
    /// ISO-8601 timestamp of the last read access. Powers salience decay;
    /// updated by the retrieval pipeline (`brain.recordPageRead`) rather
    /// than the schema trigger. `None` until first read after import.
    pub last_retrieved_at: Option<String>,

    // ── effective-date chain ─────────────────────────────────────────────
    pub effective_date: Option<String>,
    pub effective_date_source: Option<EffectiveDateSource>,
    pub import_filename: Option<String>,

    // ── salience ─────────────────────────────────────────────────────────
    /// Bumped by `recompute_emotional_weight` so salient old pages surface.
    pub salience_touched_at: Option<String>,
    /// Persisted salience score (TS migration adds the column; structurally
    /// catch-up landed in S5 alongside `last_retrieved_at`). Intentionally
    /// excluded from the `bump_page_generation` allow-list — salience is a
    /// background recomputation and bumping `generation` on every salience
    /// touch would over-invalidate the query cache.
    pub salience_score: Option<f64>,

    // ── revision counter + embedding ────────────────────────────────────
    /// Monotonic per-page revision counter. Starts at `1`; the
    /// `bump_page_generation_fn` trigger (0002 + 0003) bumps it whenever a
    /// watched column changes. Consumers use this to invalidate caches.
    pub generation: i64,
    /// Vector embedding bytes (PG `BYTEA` / `SQLite` `BLOB`). `None` until
    /// the embedding worker writes it. Encoding format (f32 LE flat
    /// array vs f16 vs other) is deferred to slice 6e per C4; this slice
    /// only carries the column.
    pub embedding: Option<Vec<u8>>,

    // ── chunker + source path ───────────────────────────────────────────
    /// Chunker generation marker (`NOT NULL DEFAULT 1`). Bumped when the
    /// chunker contract changes so reindex jobs can detect stale chunks.
    pub chunker_version: i32,
    /// Original on-disk path of the imported page (TS migration 2698).
    /// `None` for pages that were never on disk (e.g. captured via API).
    pub source_path: Option<String>,

    // ── source / provenance ──────────────────────────────────────────────
    /// Required (`NOT NULL DEFAULT 'default'`). Which source owns this page.
    pub source_id: String,
    pub source_kind: Option<String>,
    pub source_uri: Option<String>,
    pub ingested_via: Option<String>,
    pub ingested_at: Option<String>,

    // ── contextual retrieval ─────────────────────────────────────────────
    pub contextual_retrieval_mode: Option<CRMode>,
    pub corpus_generation: Option<String>,
}

/// Write-side representation. Mirrors `PageInput` in `src/core/types.ts:199`.
///
/// Only `page_type`, `title`, `compiled_truth` are required; everything else
/// is `Option<…>` with safe `Default::default()` so existing callers compile
/// unchanged.
///
/// Slice 6a S5 added the last two write-side fields (`last_retrieved_at`,
/// `embedding`) so the importer / retrieval pipeline can persist them via
/// the engine without a side-channel. The remaining 14 fields landed in S2.
#[derive(Debug, Clone, Default)]
pub struct PageInput {
    pub page_type: PageType,
    pub title: String,
    pub compiled_truth: String,
    pub timeline: Option<String>,
    pub frontmatter: Option<Value>,
    pub content_hash: Option<String>,
    pub page_kind: Option<PageKind>,
    pub effective_date: Option<String>,
    pub effective_date_source: Option<EffectiveDateSource>,
    pub import_filename: Option<String>,
    pub chunker_version: Option<i32>,
    pub source_path: Option<String>,
    pub source_kind: Option<String>,
    pub source_uri: Option<String>,
    pub ingested_via: Option<String>,
    pub ingested_at: Option<String>,
    /// NEW in S5: ISO-8601 timestamp of the last read access. Lets callers
    /// (e.g. retrieval pipeline) push the value through `put_page` instead
    /// of touching the column out-of-band.
    pub last_retrieved_at: Option<String>,
    /// NEW in S5: vector embedding bytes. `None` leaves the column
    /// untouched on upsert (per C4, encoding deferred to slice 6e).
    pub embedding: Option<Vec<u8>>,
}

/// Sort ordering for [`PageFilters::sort`]. Mirrors the TS `PageFilters.sort`
/// literal union at `types.ts:305`. Each variant maps to a whitelisted SQL
/// fragment via [`page_sort_sql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageSort {
    #[default]
    UpdatedDesc,
    UpdatedAsc,
    CreatedDesc,
    Slug,
}

/// A single search result from `search_pages`.
///
/// Contains the matched page and a relevance score (0..1, higher = more relevant).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    /// The matched page
    pub page: Page,
    /// Final relevance score (0..1). After the fusion foundation this is the
    /// RRF-fused, normalized score; downstream boost/rerank stages (later
    /// sub-nodes) mutate this in place.
    pub score: f64,
    /// Pre-boost fused score captured once at pipeline entry, mirroring the TS
    /// attribution stamp (`src/core/search/hybrid.ts:289` — "capture base_score
    /// ONCE at entry"). Lets later rerank/`--explain` stages reconstruct the
    /// per-stage multiplier breakdown (base → boost → reranker_delta → final)
    /// without re-running fusion. Equal to `score` until a boost stage runs.
    pub base_score: f64,
    /// Keyword snippet extracted from content (for UI display)
    pub snippet: Option<String>,
    /// Cross-encoder reranker relevance score, stamped ONLY when the rerank
    /// post-processing stage actually reordered this row. `None` for the
    /// un-reranked tail and whenever the reranker is disabled or fails open.
    /// Mirrors the TS `SearchResult.rerank_score` stamp
    /// (`src/core/search/rerank.ts:119`). Does NOT overwrite `score` /
    /// `base_score` — RRF fusion stays the authoritative fused signal;
    /// `--explain` reads this as a separate stage multiplier.
    pub rerank_score: Option<f64>,
    /// Rank delta from the rerank stage: original RRF index minus the new
    /// head position (positive = moved closer to the top). Computed as a free
    /// by-product the moment the head is reordered, so a later `--explain`
    /// attribution stage need not re-derive it. `None` mirrors `rerank_score`
    /// (tail rows / reranker off / fail-open). Mirrors the TS
    /// `SearchResult.reranker_delta` stamp (`src/core/search/rerank.ts:123`).
    pub reranker_delta: Option<i64>,
    /// Salience boost multiplier stamped ONLY when the post-fusion salience
    /// stage actually multiplied this row's score (`1 + k*ln(1 + salience)`,
    /// k=0.15 'on' / 0.30 'strong'). `None` when the row had zero salience or
    /// was skipped by the floor-threshold gate, so `--explain` can render "no
    /// salience boost applied" honestly. Does NOT overwrite `base_score` (the
    /// pre-boost fused signal). Mirrors TS `SearchResult.salience_boost`
    /// (`src/core/search/hybrid.ts:169`).
    ///
    /// FUTURE(boost-metadata-axes): the sibling metadata-axis boosts —
    /// backlink_boost / recency_boost / graph_signal / source_boost — plus the
    /// lexical exact-match boost are NOT migrated yet; each blocks on a data
    /// layer that does not exist in Rust (backlink counts, graph edges, source
    /// weights, intent-weights). They will add their own stamp fields here when
    /// ported. registered in docs/plans/KNOWN-GAPS.md (G13).
    pub salience_boost: Option<f64>,
    /// Recency boost multiplier stamped ONLY when the post-fusion recency stage
    /// actually multiplied this row's score (`1 + strengthMul * coef * hl /
    /// (hl + days_old)`, strengthMul 1.0 'on' / 1.5 'strong'). `None` when the
    /// page had no effective-date entry, matched an evergreen prefix
    /// (`halflife_days == 0` or `coefficient == 0`), or was skipped by the
    /// floor-threshold gate — so `--explain` can render "no recency boost
    /// applied" honestly. Does NOT overwrite `base_score`. Mirrors TS
    /// `SearchResult.recency_boost` (`src/core/search/hybrid.ts:220`).
    pub recency_boost: Option<f64>,
}

/// Options for `search_pages`.
#[derive(Debug, Default, Clone)]
pub struct SearchOpts {
    /// Keywords to search for (case-insensitive substring match)
    pub keywords: Vec<String>,
    /// Maximum number of results to return
    pub limit: Option<usize>,
    /// Minimum score threshold (0..1)
    pub min_score: Option<f64>,
    /// Source scope (None = all sources)
    pub source_id: Option<String>,
    /// Query embedding for the vector-retrieval path (f32, same space as the
    /// stored `Page::embedding` f32-LE bytes). `None` disables the vector path
    /// so fusion degenerates to lexical-only. Injectable here (rather than
    /// computed internally) so the fusion pipeline is decoupled from a real
    /// embedding provider — provider wiring is a deferred sub-node. When the
    /// vector path is inactive, RRF fusion still runs over the single lexical
    /// list, so `base_score` is always populated.
    pub query_embedding: Option<Vec<f32>>,
    /// Floor-threshold ratio for the post-fusion metadata-axis boost gate. When
    /// `Some(r)` (0 < r <= 1), a boost stage SKIPS any result whose fused score
    /// is below `top_score * r`, so a weak-overlap tail page can't leapfrog the
    /// primary hit by accumulating metadata boost. `None` (default) disables
    /// the gate, preserving prior behavior. Mirrors TS `computeFloorThreshold`
    /// (`src/core/search/hybrid.ts:126`) — the threshold is computed ONCE at
    /// post-fusion entry from the pre-boost scores so stage order can't change
    /// which rows clear the gate.
    pub floor_ratio: Option<f64>,
    /// Per-prefix recency-decay map for the post-fusion recency boost stage.
    /// `None` (default) means the engine falls back to
    /// `recency_decay::DEFAULT_RECENCY_DECAY`. The caller resolves the effective
    /// map (defaults + zbrain.yml + `ZBRAIN_RECENCY_DECAY` env + overrides) via
    /// `recency_decay::resolve_recency_decay_map` and passes the already-merged
    /// result here, mirroring the TS `applyRecencyBoost(..., decayMap, ...)`
    /// parameter shape — the engine stays a pure scoring machine and never
    /// reads env itself.
    pub recency_decay: Option<crate::recency_decay::RecencyDecayMap>,
    /// Fallback decay config applied to slugs that match no prefix in
    /// `recency_decay`. `None` uses `recency_decay::DEFAULT_FALLBACK`. Mirrors
    /// the TS `applyRecencyBoost(..., fallback)` parameter.
    pub recency_fallback: Option<crate::recency_decay::RecencyDecayConfig>,
    /// Page-type whitelist. `None` (default) or empty = no filter (all live
    /// pages are candidates). `Some(list)` keeps only pages whose `page_type`
    /// is in the list. Mirrors TS `SearchOpts.types` (v0.33) which pushes the
    /// person/company filter to SQL level for `whoknows`. In Rust the
    /// candidate query already fetches ALL live pages before post-fusion
    /// truncation, so filtering here (rather than in each backend's SQL) is
    /// budget-identical and keeps the logic in one place — every engine
    /// inherits it via `fuse_and_boost`.
    pub types: Option<Vec<String>>,
    /// When `true`, skip the post-fusion salience boost stage so `score`
    /// equals the raw fused `base_score` on the salience axis. Default `false`
    /// preserves the always-on behavior (G13). Mirrors TS
    /// `SearchOpts.salience = 'off'`. `whoknows` sets this so it can apply its
    /// OWN salience formula on the raw relevance score without double-boosting.
    pub disable_salience_boost: bool,
    /// When `true`, skip the post-fusion recency boost stage. Default `false`
    /// preserves always-on behavior (G13). Mirrors TS `SearchOpts.recency =
    /// 'off'`. Paired with `disable_salience_boost` by `whoknows`, which owns
    /// its recency-decay formula.
    pub disable_recency_boost: bool,
}

/// Reciprocal Rank Fusion constant. Mirrors `RRF_K` at
/// `src/core/search/hybrid.ts:34`. Lower values weight top ranks more heavily.
pub const RRF_K: f64 = 60.0;

/// Decode a `Page::embedding` little-endian f32 byte blob into an f32 vector.
///
/// Mirrors the TS decode path (Voyage f32-LE base64 → `Float32Array` at
/// `src/core/ai/gateway.ts:864`). Returns `None` when the blob is empty or its
/// length is not a multiple of 4 (fail-loud on a malformed column rather than
/// silently truncating).
pub(crate) fn decode_embedding_le(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Cosine similarity of two equal-length f32 vectors. Mirrors
/// `cosineSimilarity` at `src/core/search/hybrid.ts:1344`. Returns `0.0` when
/// lengths differ or either magnitude is zero (matches the TS denom-zero
/// guard), so a dimension mismatch degrades gracefully instead of ranking on
/// garbage.
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut mag_a, mut mag_b) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..a.len() {
        let (x, y) = (f64::from(a[i]), f64::from(b[i]));
        dot += x * y;
        mag_a += x * x;
        mag_b += y * y;
    }
    let denom = mag_a.sqrt() * mag_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Merge ranked lists via Reciprocal Rank Fusion, keyed by page id.
///
/// Mirrors `rrfFusion` at `src/core/search/hybrid.ts:1251`: each list
/// contributes `1 / (K + rank)` per member, contributions accumulate across
/// lists, then the fused scores are normalized to 0..1 by the observed max.
/// Input is a slice of ranked page-id lists (rank = index); output maps page id
/// → normalized fused score. Boost/rerank stages are deliberately out of scope
/// (later sub-nodes) — this only produces the pre-boost `base_score`.
fn rrf_fuse(lists: &[Vec<u64>], k: f64) -> std::collections::HashMap<u64, f64> {
    use std::collections::HashMap;
    let mut acc: HashMap<u64, f64> = HashMap::new();
    for list in lists {
        for (rank, &id) in list.iter().enumerate() {
            *acc.entry(id).or_insert(0.0) += 1.0 / (k + rank as f64);
        }
    }
    let max = acc.values().copied().fold(0.0f64, f64::max);
    if max > 0.0 {
        for score in acc.values_mut() {
            *score /= max;
        }
    }
    acc
}

/// Salience-boost coefficient for strength `'on'`. Mirrors TS
/// `applySalienceBoost` k=0.15 (`src/core/search/hybrid.ts:159`). The
/// logarithmic form `1 + k*ln(1 + salience)` keeps the factor in a bounded
/// `[1.0, ~1.6]` range so a strong boost can't catastrophically flip rankings.
const SALIENCE_BOOST_COEF_ON: f64 = 0.15;

/// Parse an ISO-8601 timestamp (as stored on `Page::created_at` /
/// `updated_at` / `effective_date`) to Unix epoch milliseconds. Returns `None`
/// for an unparseable string so the recency stage simply skips that row (the
/// same "no date entry → no boost" branch as a missing key). Accepts RFC-3339
/// with an explicit offset (e.g. `2026-07-08T00:00:00Z`) and bare
/// `YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS` (assumed UTC), matching the shapes the
/// TS layer feeds `new Date(...)`.
pub(crate) fn iso8601_to_unix_ms(s: &str) -> Option<i64> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc).timestamp_millis());
    }
    // Bare datetime without timezone → assume UTC.
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(ndt.and_utc().timestamp_millis());
        }
    }
    // Date only → midnight UTC.
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    }
    None
}

/// Compute the absolute score floor below which post-fusion metadata boosts
/// skip a result. Mirrors TS `computeFloorThreshold`
/// (`src/core/search/hybrid.ts:126`).
///
/// Returns `f64::NEG_INFINITY` (no gate) when `floor_ratio` is `None`, out of
/// range (NaN / non-finite / <= 0 / > 1), or when no result has a positive
/// finite score. Otherwise returns `top_score * ratio`, where `top_score` is
/// the largest finite score. Computed ONCE before any boost mutates scores so
/// stage order can't change which rows clear the gate.
fn compute_floor_threshold(scores: &[f64], floor_ratio: Option<f64>) -> f64 {
    let Some(ratio) = floor_ratio else {
        return f64::NEG_INFINITY;
    };
    if !ratio.is_finite() || ratio <= 0.0 || ratio > 1.0 {
        return f64::NEG_INFINITY;
    }
    let top = scores
        .iter()
        .copied()
        .filter(|s| s.is_finite() && *s > 0.0)
        .fold(f64::NEG_INFINITY, f64::max);
    if top == f64::NEG_INFINITY {
        return f64::NEG_INFINITY;
    }
    top * ratio
}

/// Backend-agnostic search core: fuse two retrieval signals over a
/// pre-filtered candidate set, then apply the post-fusion metadata boosts.
///
/// This is the shared half of `BrainEngine::search_pages`. Each backend is
/// responsible only for the backend-*specific* half — materializing the
/// candidate `Page`s (live/non-deleted, optionally source-scoped) — then hands
/// them here so InMemory, libsql, and postgres all fuse, snippet, and boost
/// identically. That guarantees a single scoring truth across engines instead
/// of three drifting copies.
///
/// `candidates` is an owned slice (not a live store borrow) precisely so the
/// caller can drop any non-Send store guard before this async fn awaits the
/// salience / effective-date reads — a lock held across an await would make the
/// caller's future non-Send. The extra clone at the call site is the price of
/// that Send-safety and is negligible against the retrieval itself.
///
/// Pipeline (mirrors TS `src/core/search/hybrid.ts`):
/// 1. Lexical path — weighted substring match over title (0.4) /
///    compiled_truth (0.4) / frontmatter (0.2), rank-ordered by hit weight.
/// 2. Vector path — cosine similarity of `opts.query_embedding` against each
///    candidate's stored f32-LE embedding; skipped when no query embedding is
///    supplied, so fusion degenerates to lexical-only.
/// 3. RRF fusion → normalized `base_score`, snippet extraction, `min_score`
///    gate.
/// 4. Post-fusion boosts — floor-threshold computed ONCE from the pre-boost
///    scores, then salience (k=0.15 'on') and recency (per-prefix half-life)
///    stages gated by it. `engine` supplies `get_salience_scores` /
///    `get_effective_dates`, so the boost inputs come from whichever backend
///    called in.
/// 5. Sort by final score descending, then apply `opts.limit`.
pub(crate) async fn fuse_and_boost(
    engine: &dyn BrainEngine,
    candidates: &[Page],
    opts: &SearchOpts,
) -> crate::Result<Vec<SearchResult>> {
    let keywords_lower: Vec<String> = opts.keywords.iter().map(|k| k.to_lowercase()).collect();

    // Page-type whitelist (TS SearchOpts.types, v0.33). `None`/empty = no
    // filter. Applied here (not in each backend's SQL) because the candidate
    // query already materializes all live pages before post-fusion truncation,
    // so a whitelist filter costs the same and keeps the logic single-sourced.
    let type_filter: Option<&[String]> = match &opts.types {
        Some(t) if !t.is_empty() => Some(t.as_slice()),
        _ => None,
    };

    // Index the owned candidate slice by page id so the two retrieval paths and
    // the fusion step share one lookup (was a `HashMap<u64, &Page>` over the
    // live store; now over the caller-materialized slice).
    let candidates_by_id: std::collections::HashMap<u64, &Page> = candidates
        .iter()
        .filter(|p| type_filter.map_or(true, |types| types.iter().any(|t| t == &p.page_type)))
        .map(|p| (p.id, p))
        .collect();

    // ── Lexical path ────────────────────────────────────────────────────
    // Substring match over title / compiled_truth / frontmatter. Produces a
    // rank-ordered list of page ids (higher weighted-hit sum ranks first).
    let mut lexical: Vec<(u64, f64)> = Vec::new();
    for (&id, page) in &candidates_by_id {
        let title_lower = page.title.to_lowercase();
        let content_lower = page.compiled_truth.to_lowercase();
        let frontmatter_lower = page.frontmatter.to_string().to_lowercase();

        let mut weight = 0.0;
        let mut hit = false;
        for keyword in &keywords_lower {
            if title_lower.contains(keyword) {
                weight += 0.4; // Title matches count more
                hit = true;
            }
            if content_lower.contains(keyword) {
                weight += 0.4; // Content matches
                hit = true;
            }
            if frontmatter_lower.contains(keyword) {
                weight += 0.2; // Frontmatter matches
                hit = true;
            }
        }
        if hit {
            lexical.push((id, weight));
        }
    }
    lexical.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let lexical_ids: Vec<u64> = lexical.iter().map(|(id, _)| *id).collect();

    // ── Vector path ─────────────────────────────────────────────────────
    // Cosine similarity between the injected query embedding and each
    // candidate's stored f32-LE embedding. Skipped entirely when no query
    // embedding is supplied, so fusion degenerates to lexical-only.
    let mut vector_ids: Vec<u64> = Vec::new();
    if let Some(query_vec) = &opts.query_embedding {
        let mut scored: Vec<(u64, f64)> = Vec::new();
        for (&id, page) in &candidates_by_id {
            if let Some(bytes) = &page.embedding {
                if let Some(page_vec) = decode_embedding_le(bytes) {
                    let cos = cosine_similarity(query_vec, &page_vec);
                    if cos > 0.0 {
                        scored.push((id, cos));
                    }
                }
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        vector_ids = scored.iter().map(|(id, _)| *id).collect();
    }

    // ── Fusion ──────────────────────────────────────────────────────────
    // RRF-fuse the two ranked lists into a normalized 0..1 base_score. A
    // page appearing in both lists accumulates both contributions.
    let fused = rrf_fuse(&[lexical_ids, vector_ids], RRF_K);

    let mut results = Vec::new();
    for (id, base_score) in fused {
        let Some(page) = candidates_by_id.get(&id) else {
            continue;
        };
        if opts.min_score.map_or(false, |min| base_score < min) {
            continue;
        }

        // Snippet: 150-char window around the first keyword hit, else the
        // content head (vector-only matches have no keyword to anchor on).
        let content_lower = page.compiled_truth.to_lowercase();
        let snippet = if page.compiled_truth.is_empty() {
            None
        } else {
            let first_match = keywords_lower
                .iter()
                .find_map(|k| content_lower.find(k))
                .unwrap_or(0);
            let start = first_match.saturating_sub(50);
            let end = (start + 150).min(page.compiled_truth.len());
            // Clamp to a char boundary so the slice never panics on UTF-8.
            let mut s = start;
            while s > 0 && !page.compiled_truth.is_char_boundary(s) {
                s -= 1;
            }
            let mut e = end;
            while e < page.compiled_truth.len() && !page.compiled_truth.is_char_boundary(e) {
                e += 1;
            }
            Some(page.compiled_truth[s..e].to_string())
        };

        results.push(SearchResult {
            page: (*page).clone(),
            score: base_score,
            base_score,
            snippet,
            // Rerank is a query-pipeline post-processing stage layered on
            // top of the engine's fused results (see operation.rs query
            // path); the engine itself never reranks, so both stamps start
            // as None and are set later only for reordered head rows.
            rerank_score: None,
            reranker_delta: None,
            salience_boost: None,
            recency_boost: None,
        });
    }

    // ── Post-fusion boost stages ────────────────────────────────────────
    // Mirrors TS `runPostFusionStages` (src/core/search/hybrid.ts:282):
    // compute the floor-threshold ONCE from the pre-boost fused scores,
    // then apply each metadata-axis boost gated by it. Only the salience
    // stage is migrated so far; strength is hardcoded to 'on' (k=0.15)
    // because the search-mode system that resolves 'on'/'strong'/'off' is
    // not ported yet.
    //
    // FUTURE(salience-strength-by-mode): TS resolves salience strength from
    // the active search mode (ModeBundle). Rust has no mode system yet, so
    // this is pinned to 'on'. registered in docs/plans/KNOWN-GAPS.md (G13).
    if !results.is_empty() {
        let pre_boost: Vec<f64> = results.iter().map(|r| r.base_score).collect();
        let floor = compute_floor_threshold(&pre_boost, opts.floor_ratio);

        // Salience scores are keyed by "{source_id}::{slug}"; read them via
        // the same engine method the trait already exposes.
        let refs: Vec<crate::types::PageRef> = results
            .iter()
            .map(|r| crate::types::PageRef {
                slug: r.page.slug.clone(),
                source_id: r.page.source_id.clone(),
            })
            .collect();

        // Salience boost stage. Skipped entirely when the caller sets
        // `disable_salience_boost` (TS `salience: 'off'`) — `whoknows` does
        // this so it can apply its OWN salience formula on the raw fused
        // `base_score` without double-boosting. When skipped, we also avoid
        // the `get_salience_scores` round-trip.
        if !opts.disable_salience_boost {
            let salience = engine.get_salience_scores(&refs).await?;
            for r in &mut results {
                if !r.score.is_finite() || r.score < floor {
                    continue;
                }
                let key = format!("{}::{}", r.page.source_id, r.page.slug);
                let Some(&s) = salience.get(&key) else {
                    continue;
                };
                if s <= 0.0 {
                    continue;
                }
                let factor = 1.0 + SALIENCE_BOOST_COEF_ON * (1.0 + s).ln();
                r.score *= factor;
                r.salience_boost = Some(factor);
            }
        }

        // Recency stage (per-prefix half-life decay). Uses the same
        // once-computed floor as salience so a weak-overlap tail page can't
        // leapfrog the primary hit by stacking recency on top. The decay
        // map is caller-resolved config (defaults + zbrain.yml + env +
        // overrides), passed in via SearchOpts — the engine never reads env
        // itself, staying a pure scoring machine. Dates come from the
        // engine's own get_effective_dates; strength is pinned to 'on'
        // (search-mode system unported — see the salience note above / G13).
        //
        // Skipped entirely when the caller sets `disable_recency_boost` (TS
        // `recency: 'off'`), paired with `disable_salience_boost` by
        // `whoknows` which owns its recency-decay formula. When skipped we
        // also avoid the `get_effective_dates` round-trip.
        if !opts.disable_recency_boost {
            let date_strings = engine.get_effective_dates(&refs).await?;
            let dates_ms: std::collections::HashMap<String, i64> = date_strings
                .into_iter()
                .filter_map(|(k, v)| iso8601_to_unix_ms(&v).map(|ms| (k, ms)))
                .collect();
            let decay_map = opts
                .recency_decay
                .clone()
                .unwrap_or_else(crate::recency_decay::default_recency_decay);
            let fallback = opts
                .recency_fallback
                .unwrap_or(crate::recency_decay::DEFAULT_FALLBACK);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            let mut rows: Vec<crate::recency_decay::RecencyRow<'_>> = results
                .iter_mut()
                .map(|r| {
                    let key = format!("{}::{}", r.page.source_id, r.page.slug);
                    crate::recency_decay::RecencyRow {
                        slug: r.page.slug.as_str(),
                        key,
                        score: &mut r.score,
                        recency_boost: &mut r.recency_boost,
                    }
                })
                .collect();
            crate::recency_decay::apply_recency_boost(
                &mut rows,
                &dates_ms,
                // Pinned to 'on': Rust has no search-mode system yet to resolve
                // 'on'/'strong'/'off' from a ModeBundle (same gap as salience
                // strength above). registered in docs/plans/KNOWN-GAPS.md (G13).
                crate::recency_decay::RecencyStrength::On,
                &decay_map,
                fallback,
                now_ms,
                if floor.is_finite() { Some(floor) } else { None },
            );
        }

        // Backlink stage (v0.29.1). Mirrors TS `runPostFusionStages` order
        // (backlink → salience → recency). Fail-open: any error from
        // `get_backlink_counts` leaves scores untouched, preserving the
        // pre-v0.29.1 contract. TS `applyBacklinkBoost` multiplies `score`
        // in place (no separate stamped field — unlike salience/recency which
        // record a multiplier), so we do the same. Keyed by `slug` to match
        // TS `getBacklinkCounts(slugs)` + `applyBacklinkBoost` (which indexes
        // counts by `r.slug`, NOT `source_id::slug`).
        if let Ok(counts) = engine
            .get_backlink_counts(
                &results.iter().map(|r| r.page.slug.clone()).collect::<Vec<_>>(),
            )
            .await
        {
            let floor_gate = if floor.is_finite() { Some(floor) } else { None };
            for r in &mut results {
                if !r.score.is_finite() {
                    continue;
                }
                if let Some(f) = floor_gate {
                    if r.score < f {
                        continue;
                    }
                }
                let count = counts.get(&r.page.slug).copied().unwrap_or(0);
                if count > 0 {
                    r.score *=
                        1.0 + crate::search::fusion::BACKLINK_BOOST_COEF * (1.0_f64 + count as f64).ln();
                }
            }
        }
    }

    // Sort by score descending (boosts may have reordered the head).
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Apply limit if set
    if let Some(limit) = opts.limit {
        results.truncate(limit);
    }

    Ok(results)
}

/// Returns the whitelisted SQL `ORDER BY` fragment for the given sort mode.
/// Mirrors `PAGE_SORT_SQL` at `types.ts:332`. Engines splice this literal
/// into prepared statements — no SQL-injection risk because the enum is closed.
#[must_use]
pub fn page_sort_sql(sort: PageSort) -> &'static str {
    match sort {
        PageSort::UpdatedDesc => "p.updated_at DESC",
        PageSort::UpdatedAsc => "p.updated_at ASC",
        PageSort::CreatedDesc => "p.created_at DESC",
        PageSort::Slug => "p.slug ASC",
    }
}

/// Validate a source id against the canonical regex from TS
/// `src/core/source-id.ts`:
/// `^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`
///
/// Rules: 1-32 chars, lowercase alphanumeric, optional interior hyphens,
/// must start and end with alphanumeric.
#[must_use]
pub fn is_valid_source_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 32 {
        return false;
    }
    let bytes = id.as_bytes();
    // First char must be alphanumeric lowercase
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    // Last char must be alphanumeric lowercase
    if !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    // All chars must be [a-z0-9-]
    bytes
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Filter options for [`BrainEngine::list_pages`]. Mirrors `PageFilters`
/// at `types.ts:277`.
#[derive(Debug, Default, Clone)]
pub struct PageFilters {
    pub page_type: Option<PageType>,
    pub tag: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub updated_after: Option<String>,
    pub slug_prefix: Option<String>,
    pub include_deleted: bool,
    pub sort: Option<PageSort>,
    pub source_id: Option<String>,
    pub source_ids: Option<Vec<String>>,
}

/// Options for [`BrainEngine::get_page`]. Mirrors `GetPageOpts`.
#[derive(Debug, Default, Clone)]
pub struct GetPageOpts {
    /// Source scope for slug lookup. `None` performs an unscoped slug lookup,
    /// matching TS `getPage(slug)` semantics. Callers that need the default
    /// source only must pass `Some("default")` explicitly.
    pub source_id: Option<String>,
    pub include_deleted: bool,
}

/// Options for [`BrainEngine::resolve_slugs`]. Mirrors TS
/// `resolveSlugs(partial, { sourceId, sourceIds })`.
#[derive(Debug, Default, Clone)]
pub struct ResolveSlugsOpts {
    /// Scope to a single source when present.
    pub source_id: Option<String>,
    /// Scope to a federated set of sources when non-empty. This takes
    /// precedence over `source_id`, matching TS source scope precedence.
    pub source_ids: Option<Vec<String>>,
}

/// A row from the `sources` table. Mirrors the full TypeScript `SourceRow`
/// interface from `src/core/sources-load.ts` and the TS `src/schema.sql` DDL.
///
/// Fields beyond `id`/`name`/`config` are populated by the import/clone pipeline
/// (1-7-1-2), sync engine (1-7-1-5), and archive lifecycle (v0.26.5).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceRow {
    pub id: String,
    pub name: String,
    pub local_path: Option<String>,
    pub last_commit: Option<String>,
    pub last_sync_at: Option<String>,
    pub config: serde_json::Value,
    pub created_at: Option<String>,
    pub chunker_version: Option<String>,
    pub archived: bool,
    pub archived_at: Option<String>,
    pub archive_expires_at: Option<String>,
    pub contextual_retrieval_mode: Option<String>,
    pub trust_frontmatter_overrides: bool,
}

/// Input for `BrainEngine::create_source`. Mirrors the TS source init flow.
/// `id` must satisfy the source-id regex `^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateSourceInput {
    pub id: String,
    pub name: String,
    pub config: Option<serde_json::Value>,
}

/// Input for `BrainEngine::update_source`. All fields are optional — only
/// `Some(_)` values are applied. Mirrors TS `updateSourceConfig` + sync
/// field mutations.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct UpdateSourceInput {
    pub name: Option<String>,
    pub config: Option<serde_json::Value>,
    pub local_path: Option<String>,
    pub last_commit: Option<String>,
    pub last_sync_at: Option<String>,
    pub chunker_version: Option<String>,
    pub contextual_retrieval_mode: Option<String>,
    pub trust_frontmatter_overrides: Option<bool>,
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Core engine contract. Every storage backend (postgres, libsql, in-memory)
/// must implement this trait.
///
/// Object-safe by design — callers hold `Arc<dyn BrainEngine>` so that
/// operations / CLI layer can be storage-agnostic.
///
/// Slices 4-8 will extend this trait with additional method groups (chunks,
/// links, takes, facts, …) following the same append-only pattern.
/// Input for [`BrainEngine::add_take_proposal`]. Mirrors the columns written
/// by `propose-takes.ts` into the `take_proposals` queue. The idempotency key
/// is the composite unique index `(source_id, page_slug, content_hash,
/// prompt_version)` (see `src/schema.sql`).
#[derive(Debug, Clone)]
pub struct TakeProposalInput {
    pub source_id: String,
    pub page_slug: String,
    pub content_hash: String,
    pub prompt_version: String,
    pub proposal_run_id: String,
    pub claim_text: String,
    pub kind: String,
    pub holder: String,
    pub weight: f64,
    pub domain: Option<String>,
    /// JSON-encoded list of existing fence rows used for dedup context.
    pub dedup_against_fence_rows: Option<String>,
    pub model_id: String,
}

/// Input for [`BrainEngine::add_take_grade_cache`]. Mirrors the columns written
/// by `grade-takes.ts` into the `take_grade_cache` verdict cache. The idempotency
/// key is the composite primary key `(take_id, prompt_version, judge_model_id,
/// evidence_signature)` (see `src/schema.sql` / migration 0023).
#[derive(Debug, Clone)]
pub struct TakeGradeCacheInput {
    pub take_id: u64,
    pub prompt_version: String,
    pub judge_model_id: String,
    pub evidence_signature: String,
    /// Defaults to `v0.36.1.0` when not set.
    pub wave_version: String,
    pub verdict: String,
    pub confidence: f64,
    /// Whether this verdict was auto-applied to the canonical `takes` table via
    /// `resolve_take`. D17 default: `false` (review-queue posture).
    pub applied: bool,
    /// Estimated LLM cost in USD for this verdict (optional, telemetry only).
    pub cost_usd: Option<f64>,
}

/// Input for [`BrainEngine::put_dream_verdict`]. Mirrors the columns written by
/// `synthesize.ts` into the `dream_verdicts` significance cache (v0.23). The
/// idempotency key is the composite primary key `(file_path, content_hash)`
/// (see `src/schema.sql` / migration 0026).
#[derive(Debug, Clone)]
pub struct DreamVerdictInput {
    pub worth_processing: bool,
    /// Free-text justifications for the verdict (e.g. why a transcript is or
    /// isn't worth synthesizing). Empty when no reasons were recorded.
    pub reasons: Vec<String>,
}

/// Returned by [`BrainEngine::get_dream_verdict`]. Mirrors `DreamVerdict` in
/// `synthesize.ts`.
#[derive(Debug, Clone)]
pub struct DreamVerdict {
    pub worth_processing: bool,
    /// Free-text justifications (see [`DreamVerdictInput::reasons`]).
    pub reasons: Vec<String>,
    /// ISO-8601 timestamp (UTC). Stored as `TIMESTAMPTZ` in Postgres and `TEXT`
    /// in SQLite.
    pub judged_at: String,
}

/// A single take contributing to a page's emotional-weight computation.
/// Shared by [`BrainEngine::batch_load_emotional_inputs`] and
/// `compute_emotional_weight` (see `autopilot::phases::emotional_weight`).
#[derive(Debug, Clone)]
pub struct EmotionalWeightTake {
    pub holder: String,
    pub weight: f64,
    pub kind: String,
    pub active: bool,
}

/// Per-page inputs for emotional-weight recomputation, returned by
/// [`BrainEngine::batch_load_emotional_inputs`].
#[derive(Debug, Clone)]
pub struct EmotionalInput {
    pub slug: String,
    pub source_id: String,
    pub tags: Vec<String>,
    pub takes: Vec<EmotionalWeightTake>,
}

/// A single emotional-weight write (composite key slug + source_id), consumed
/// by [`BrainEngine::set_emotional_weight_batch`].
#[derive(Debug, Clone)]
pub struct EmotionalWeightWrite {
    pub slug: String,
    pub source_id: String,
    pub emotional_weight: f64,
}

// ── Eval candidates (G74 1-1-4: eval-export / eval-prune / eval-replay) ──
//
// Mirrors TS `eval_candidates` (src/schema.sql, v0.25.0 BrainBench-Real
// substrate), created by migration 0030 (dual dialect). Capture-side writes
// are not yet wired (honest gap), so the table starts empty until that lands.
// These types + the default trait methods below live at module level so all
// backends (incl. the InMemory test engine) compile unchanged; libsql and
// postgres override with real SQL.

/// Filter for `list_eval_candidates` (eval-export / eval-replay / eval-whoknows-L2).
#[derive(Debug, Clone, Default)]
pub struct EvalCandidateFilter {
    /// Restrict to a tool: `query` or `search`.
    pub tool_name: Option<String>,
    /// ISO-8601 lower bound on `created_at` (inclusive).
    pub since: Option<String>,
    /// Max rows to return (newest first).
    pub limit: Option<usize>,
}

/// A captured eval candidate row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalCandidate {
    pub id: i64,
    pub tool_name: String,
    pub query: String,
    #[serde(default)]
    pub retrieved_slugs: Vec<String>,
    #[serde(default)]
    pub retrieved_chunk_ids: Vec<i64>,
    #[serde(default)]
    pub source_ids: Vec<String>,
    pub expand_enabled: Option<bool>,
    pub detail: Option<String>,
    pub detail_resolved: Option<String>,
    pub vector_enabled: bool,
    pub expansion_applied: bool,
    pub latency_ms: i64,
    pub remote: bool,
    pub job_id: Option<i64>,
    pub subagent_id: Option<i64>,
    pub created_at: String,
    pub as_of_ts: Option<String>,
    pub salience_param: Option<String>,
    pub recency_param: Option<String>,
    pub salience_resolved: Option<String>,
    pub recency_resolved: Option<String>,
    pub salience_source: Option<String>,
    pub recency_source: Option<String>,
    pub embedding_column: Option<String>,
}

#[async_trait]
pub trait BrainEngine: Send + Sync + std::fmt::Debug {
    // ── Identity ──────────────────────────────────────────────────────────

    /// Returns the backend discriminator. Used for conditional logic in
    /// migrations and diagnostics without `downcast`.
    fn kind(&self) -> EngineKind;

    /// Whether this backend has a persistent database store. Maintenance
    /// phases that require DB access skip with `reason = "no_database"`
    /// when this returns `false` (mirrors the TS `engine === null`
    /// no-database guard). Defaults to `true`; a read-only / null backend
    /// overrides it to `false` (1-6-1-4).
    fn supports_database(&self) -> bool {
        true
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Open / authenticate the underlying connection pool.
    async fn connect(&self, config: &EngineConfig) -> crate::Result<()>;

    /// Gracefully drain the connection pool and release resources.
    async fn disconnect(&self) -> crate::Result<()>;

    /// Apply schema migrations up to the current version.
    async fn init_schema(&self) -> crate::Result<()>;

    // ── Source lookup ──────────────────────────────────────────────────────

    /// Look up a source by its `config->>'github_repo'` value.
    /// Used by POST /webhooks/github to find the matching source.
    /// Returns `None` if no source is configured for this GitHub repo.
    async fn get_source_by_github_repo(
        &self,
        github_repo: &str,
    ) -> crate::Result<Option<SourceRow>>;

    // ── Source CRUD (1-7-1-1) ──────────────────────────────────────────────

    /// List all sources, optionally including archived ones.
    /// Mirrors TS `loadAllSources({ includeArchived })` in `src/core/sources-load.ts`.
    async fn list_sources(&self, include_archived: bool) -> crate::Result<Vec<SourceRow>>;

    /// Fetch a single source by its `id`.
    /// Mirrors TS `fetchSource(id)` in `src/core/sources-load.ts`.
    async fn get_source(&self, id: &str) -> crate::Result<Option<SourceRow>>;

    /// Per-source sync statistics for the status report (roadmap 1-6-7-13-4).
    ///
    /// Returns one row per registered source with page / chunk / unembedded
    /// chunk counts. The `sync_enabled` flag is derived from
    /// `config.syncEnabled !== false` (default true). Unlike the TS
    /// `buildSyncStatusReport` (which used `engine.executeRaw`), this is a
    /// dedicated typed method — each backend encapsulates its own fixed SQL,
    /// keeping `zbrain-core` free of a raw-SQL escape hatch.
    async fn source_sync_stats(
        &self,
    ) -> crate::Result<Vec<crate::sync_status::SourceSyncStat>>;

    /// Create a new source row. The `id` must pass source-id validation
    /// (`^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`). Returns the created row
    /// (with `created_at` populated). Mirrors TS `createSource` in the CLI
    /// init flow.
    async fn create_source(&self, input: &CreateSourceInput) -> crate::Result<SourceRow>;

    /// Update mutable fields of an existing source (`name`, `config`,
    /// `local_path`, `last_commit`, `last_sync_at`, `chunker_version`,
    /// `contextual_retrieval_mode`, `trust_frontmatter_overrides`).
    /// Returns the updated row. Mirrors TS `updateSourceConfig` and sync
    /// field mutations.
    async fn update_source(&self, id: &str, input: &UpdateSourceInput) -> crate::Result<SourceRow>;

    /// Soft-delete (archive) a source: sets `archived = true`,
    /// `archived_at = now()`, `archive_expires_at = now() + 72h`.
    /// Returns `true` if a row was affected. Mirrors TS archive flow (v0.26.5).
    async fn delete_source(&self, id: &str) -> crate::Result<bool>;

    // ── Page CRUD (slice 3 subset) ────────────────────────────────────────

    /// Fetch a single page by `slug`.
    ///
    /// `opts.source_id = Some(_)` scopes the lookup to that source. `None`
    /// performs the TS-compatible unscoped slug lookup. Returns `None` if not
    /// found or soft-deleted (unless `opts.include_deleted` is true).
    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> crate::Result<Option<Page>>;

    /// Insert or update a page (upsert semantics — same `(source_id, slug)` →
    /// same `id`).
    ///
    /// `source_id = None` is normalised to `"default"` to mirror the TS
    /// `opts?.sourceId ?? 'default'` rule
    /// (`zbrain/src/core/pglite-engine.ts:838`). Pages in different sources
    /// with the same slug are independent rows.
    async fn put_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
        input: &PageInput,
    ) -> crate::Result<Page>;

    /// Hard-delete a page row by (`source_id`, `slug`).
    ///
    /// `source_id = None` is normalised to `"default"`, mirroring TS
    /// `deletePage(slug, opts)` where `opts?.sourceId ?? "default"`.
    async fn delete_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<()>;

    /// Return all pages matching `filters`, ordered by `filters.sort` or the
    /// default [`PageSort::UpdatedDesc`] when no explicit sort is supplied.
    ///
    /// Source filter precedence mirrors TS/PGLite: non-empty
    /// `filters.source_ids` wins over `filters.source_id`; an empty
    /// `filters.source_ids` falls back to `filters.source_id`, or remains
    /// unscoped when no single source is provided.
    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>>;

    /// Slug resolver with TS-compatible exact-first / fuzzy-fallback behavior.
    /// Exact slug matches win and suppress fuzzy candidates; otherwise fuzzy
    /// fallback returns live slugs containing `partial`. Source scoping mirrors
    /// TS `resolveSlugs(partial, opts)` where non-empty `source_ids` takes
    /// precedence over `source_id`.
    async fn resolve_slugs(
        &self,
        partial: &str,
        opts: &ResolveSlugsOpts,
    ) -> crate::Result<Vec<String>>;

    // ── Files (metadata rows; bytes live outside the DB) ──────────────────
    /// Insert or update a file metadata row. Mirrors TS `upsertFile`.
    ///
    /// Identity follows the current TS schema/backend implementation:
    /// `UNIQUE(storage_path)`, not `(source_id, storage_path)`.
    async fn upsert_file(&self, spec: &FileSpec) -> crate::Result<UpsertFileResult>;

    /// Fetch one file metadata row by source and storage path. Mirrors TS
    /// `getFile(sourceId, storagePath)`.
    async fn get_file(&self, source_id: &str, storage_path: &str)
        -> crate::Result<Option<FileRow>>;

    /// List file metadata rows linked to a page id. Mirrors TS
    /// `listFilesForPage(pageId)`.
    async fn list_files_for_page(&self, page_id: u64) -> crate::Result<Vec<FileRow>>;

    // ── 1-6-7-5: file listing + ingestion + chunks ──────────────────────

    /// List file metadata rows, optionally filtered by page slug. Mirrors TS
    /// `file_list` op (FILE_LIST_LIMIT = 100 enforced by the op).
    /// Default: returns `Err(`Unsupported`)`.
    async fn list_files(&self, slug: Option<&str>) -> crate::Result<Vec<FileListRow>> {
        let _ = slug;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_files not yet implemented for this engine",
        ))
    }

    /// Return content chunks for a page by slug. Mirrors TS `getChunks`.
    /// Default: returns `Err(`Unsupported`)`.
    async fn get_chunks(&self, slug: &str, source_id: &str) -> crate::Result<Vec<Chunk>> {
        let _ = (slug, source_id);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_chunks not yet implemented for this engine",
        ))
    }

    /// Append an ingestion-log entry. Mirrors TS `logIngest`.
    /// Default: returns `Err(`Unsupported`)`.
    async fn log_ingest(&self, input: &IngestLogInput) -> crate::Result<()> {
        let _ = input;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "log_ingest not yet implemented for this engine",
        ))
    }

    /// Return recent ingestion-log entries, newest first. Mirrors TS
    /// `getIngestLog`. Default: returns `Err(`Unsupported`)`.
    async fn get_ingest_log(&self, limit: u32) -> crate::Result<Vec<IngestLogEntry>> {
        let _ = limit;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_ingest_log not yet implemented for this engine",
        ))
    }

    // ── Calibration (1-6-7-5) ─────────────────────────────────────

    /// Read the active calibration profile for a holder. Mirrors TS
    /// `get_calibration_profile` (which delegates to `getLatestProfile`).
    /// Default: returns `Err(`Unsupported`)` — engines that implement
    /// `CalibrationQueries` override this to delegate to `get_latest_profile`.
    async fn get_calibration_profile(
        &self,
        _holder: &str,
        _source_id: Option<&str>,
        _source_ids: Option<&[String]>,
    ) -> crate::Result<Option<crate::calibration_queries::CalibrationProfileRow>> {
        let _ = (_holder, _source_id, _source_ids);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_calibration_profile not implemented for this engine",
        ))
    }

    /// Aggregated scorecard for a holder. Mirrors TS `getScorecard`. Default:
    /// `Err(`Unsupported`)` — engines that implement `CalibrationQueries`
    /// override this to delegate to `CalibrationQueries::get_scorecard`.
    ///
    /// This is the bridge the `takes_scorecard` operation calls; it lets the
    /// op stay on `&dyn BrainEngine` without downcasting to `CalibrationQueries`.
    async fn get_scorecard(
        &self,
        _query: &crate::calibration_queries::ScorecardQuery<'_>,
    ) -> crate::Result<crate::calibration_queries::TakesScorecard> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_scorecard not implemented for this engine",
        ))
    }

    /// Write a calibration profile row. Mirrors TS `runPhaseCalibrationProfile`.
    /// Default: `Err(`Unsupported`)` — engines that implement `CalibrationQueries`
    /// override this to delegate to `insert_calibration_profile`.
    async fn insert_calibration_profile(
        &self,
        _row: &crate::calibration_queries::CalibrationProfileInsert<'_>,
    ) -> crate::Result<i64> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "insert_calibration_profile not implemented for this engine",
        ))
    }

    /// Calibration curve (observed vs predicted per weight bucket). Mirrors TS
    /// `getCalibrationCurve`. Default: `Err(`Unsupported`)` — engines that
    /// implement `CalibrationQueries` override this to delegate to
    /// `CalibrationQueries::get_calibration_curve`.
    ///
    /// This is the bridge the `takes_calibration` operation calls; it lets the
    /// op stay on `&dyn BrainEngine` without downcasting to `CalibrationQueries`.
    async fn get_calibration_curve(
        &self,
        _query: &crate::calibration_queries::CalibrationCurveQuery<'_>,
    ) -> crate::Result<Vec<crate::calibration_queries::CalibrationBucket>> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_calibration_curve not implemented for this engine",
        ))
    }

    // ── undo-wave reversal bridge (1-3-3-2) ──────────────────────────────
    // Default = unsupported; each concrete engine overrides to delegate to
    // `CalibrationWaveQueries`. Lets `undo_wave` stay on `&dyn BrainEngine`.

    /// Step 1 of `undo_wave`. See [`crate::calibration_queries::CalibrationWaveQueries`].
    async fn revert_wave_resolutions(
        &self,
        _wave_version: &str,
        _resolved_by: &str,
        _dry_run: bool,
    ) -> crate::Result<u64> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "revert_wave_resolutions not implemented for this engine",
        ))
    }

    /// Step 1b of `undo_wave`. See [`crate::calibration_queries::CalibrationWaveQueries`].
    async fn unapply_wave_grade_cache(&self, _wave_version: &str, _dry_run: bool) -> crate::Result<u64> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "unapply_wave_grade_cache not implemented for this engine",
        ))
    }

    /// Step 2 of `undo_wave`. See [`crate::calibration_queries::CalibrationWaveQueries`].
    async fn delete_calibration_profiles_for_wave(
        &self,
        _wave_version: &str,
        _dry_run: bool,
    ) -> crate::Result<u64> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "delete_calibration_profiles_for_wave not implemented for this engine",
        ))
    }

    /// Step 3 of `undo_wave`. See [`crate::calibration_queries::CalibrationWaveQueries`].
    async fn purge_nudge_log_for_wave(&self, _wave_version: &str, _dry_run: bool) -> crate::Result<u64> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "purge_nudge_log_for_wave not implemented for this engine",
        ))
    }

    /// Execute a raw SQL query and return rows as JSON Values.
    ///
    /// This is needed for complex ad-hoc aggregations like calibration domain
    /// scorecard aggregation that require joining multiple tables and cannot
    /// be easily expressed through the typed query APIs.
    ///
    /// The caller is responsible for deserializing rows to the expected type.
    async fn execute_raw(
        &self,
        _sql: &str,
        _params: &[&(dyn erased_serde::Serialize + Sync)],
    ) -> crate::Result<Vec<serde_json::Value>> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "execute_raw not implemented for this engine",
        ))
    }

    // ── Chunks & Code Edges (slice #110) ──────────────────────────

    /// Upsert chunks for a page. Mirrors TS `upsertChunks`.
    /// Default: returns `Err(`Unsupported`)`.
    async fn upsert_chunks(
        &self,
        slug: &str,
        chunks: &[crate::import::ChunkInput],
    ) -> crate::Result<()> {
        let _ = (slug, chunks);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "upsert_chunks not yet implemented for this engine",
        ))
    }

    /// Delete all chunks for a page. Mirrors TS `deleteChunks`.
    /// Default: returns `Err(`Unsupported`)`.
    async fn delete_chunks(&self, slug: &str) -> crate::Result<()> {
        let _ = slug;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "delete_chunks not yet implemented for this engine",
        ))
    }

    /// Return all stored chunks for a page slug in chunk-index order.
    /// Default: returns `Err(`Unsupported`)`.
    async fn get_chunks_for_page(&self, slug: &str) -> crate::Result<Vec<crate::import::ChunkInput>> {
        let _ = slug;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_chunks_for_page not yet implemented for this engine",
        ))
    }

    /// Add code edges. Mirrors TS `addCodeEdges`.
    /// Default: returns `Err(`Unsupported`)`.
    async fn add_code_edges(
        &self,
        edges: &[crate::import::CodeEdgeInput],
    ) -> crate::Result<()> {
        let _ = edges;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "add_code_edges not yet implemented for this engine",
        ))
    }

    /// Delete code edges for given chunk IDs. Mirrors TS `deleteCodeEdgesForChunks`.
    /// Default: returns `Err(`Unsupported`)`.
    async fn delete_code_edges_for_chunks(
        &self,
        chunk_ids: &[i64],
    ) -> crate::Result<()> {
        let _ = chunk_ids;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "delete_code_edges_for_chunks not yet implemented for this engine",
        ))
    }

    /// Graph query: "who calls this symbol?" — UNION of `code_edges_chunk` +
    /// `code_edges_symbol` where `to_symbol_qualified = qualified_name`.
    /// Mirrors TS `getCallersOf`. Default: returns `Err(`Unsupported`)`.
    async fn get_callers_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
        let _ = (qualified_name, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_callers_of not yet implemented for this engine",
        ))
    }

    /// Graph query: "what does this symbol call?" — edges where
    /// `from_symbol_qualified = qualified_name`. Mirrors TS `getCalleesOf`.
    /// Default: returns `Err(`Unsupported`)`.
    async fn get_callees_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
        let _ = (qualified_name, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_callees_of not yet implemented for this engine",
        ))
    }

    /// All edges touching a chunk in the given direction. Mirrors TS
    /// `getEdgesByChunk`. Default: returns `Err(`Unsupported`)`.
    async fn get_edges_by_chunk(
        &self,
        chunk_id: i64,
        opts: &crate::import::CodeEdgeByChunkOpts,
    ) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
        let _ = (chunk_id, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_edges_by_chunk not yet implemented for this engine",
        ))
    }

    /// 1-6-7-10-3 符号查询：精确查找符号的**定义站点**。
    ///
    /// 对齐 TS `findCodeDef`（`src/commands/code-def.ts`）：`content_chunks.symbol_name`
    /// 精确匹配 + `symbol_type IN (DEF_TYPES)` + 所属页面 `page_kind = 'code'`，
    /// JOIN `pages` 取 `slug` 与 `frontmatter->>'file'`。
    async fn find_code_def(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeDefResult>> {
        let _ = (symbol, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "find_code_def not yet implemented for this engine",
        ))
    }

    /// 1-6-7-10-3 符号查询：模糊查找符号的**引用站点**（使用处）。
    ///
    /// 对齐 TS `findCodeRefs`（`src/commands/code-refs.ts`）：`content_chunks.chunk_text`
    /// `ILIKE '%symbol%'` + 所属页面 `page_kind = 'code'`，JOIN `pages` 取 `slug` 与
    /// `frontmatter->>'file'`。
    async fn find_code_refs(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeRefResult>> {
        let _ = (symbol, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "find_code_refs not yet implemented for this engine",
        ))
    }

    /// 1-6-7-10-4 符号消歧：把裸符号名解析为合格名（`symbol_name_qualified`），
    /// 供递归遍历（`runRecursiveWalk`）在跳数之前定位起点。
    ///
    /// 对齐 TS `disambiguateSymbol`（`src/core/code-intel/recursive-walk.ts:77`）：
    /// 先按 `symbol_name = bare OR symbol_name_qualified = bare` 取精确命中，
    /// 无命中时按 `symbol_name_qualified ILIKE '%bare%'` 取 `did_you_mean` 候选。
    /// 两阶段均限定 `pages.source_id = source_id` 且与 `symbol_name_qualified IS NOT NULL`。
    async fn disambiguate_symbol(
        &self,
        bare: &str,
        source_id: &str,
    ) -> crate::Result<crate::import::SymbolDisambiguation> {
        let _ = (bare, source_id);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "disambiguate_symbol not yet implemented for this engine",
        ))
    }

    /// 递归遍历代码调用图（BFS）：从起始合格符号出发，按方向逐跳扩展，
    /// 深度分组返回，处理循环截断、节点上限。对齐 TS `runRecursiveWalk`。
    ///
    /// 算法路径：
    /// 1. 若输入不是精确合格名，先用 `disambiguate_symbol` 解析起始符号；
    /// 2. 语言门：仅支持 `typescript/tsx/javascript/python`，其它语言返回 unsupported；
    /// 3. BFS 遍历：从起始符号出发，每跳调用 `get_callers_of`/`get_callees_of` 取下一跳，
    ///    用 visited set 去重+检测循环，遇到循环跳过并标记 `cycles_detected = true`；
    /// 4. 截断：命中 `depth_cap` 或 `max_nodes` 时标记对应 truncation 提前退出；
    /// 5. 结果按深度分组返回，置信度按公式 `clamp(1/(1+0.3*d), 0.05, 1.0)` 计算。
    async fn recursive_walk(
        &self,
        symbol: &str,
        opts: &crate::import::RecursiveWalkOpts,
    ) -> crate::Result<crate::import::RecursiveWalkResult> {
        let _ = (symbol, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "recursive_walk not yet implemented for this engine",
        ))
    }

    // ── Slice 6a S6 method group (15 required methods) ────────────────────
    //
    // Backends must implement the full Slice 6a S6 method group explicitly;
    // these methods have no default body, so leaving one unimplemented is a
    // compile error — exactly the safety net the C1 contract hardening plan
    // (2026-06-01) demands.
    //
    // Method ordering: §13.2 of `13-slice-6a-gap-checklist.md`.

    // — Duplicate detection (1) —
    /// Return the first live duplicate page identity within `source_id`.
    ///
    /// Mirrors TS `findDuplicatePage`, including its minimal return shape:
    /// `{ slug: string; id: number } | null`. Matching is by `content_hash` or,
    /// when supplied, `frontmatter.id`; soft-deleted rows are ignored.
    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> crate::Result<Option<DuplicatePage>>;

    // — Soft-delete lifecycle (3) —
    /// Soft-delete a page (set `deleted_at = CURRENT_TIMESTAMP`).
    /// Returns `Some(slug)` if a row was hit, `None` if the slug was already
    /// missing or already soft-deleted. Mirrors TS `softDeletePage` which
    /// returns `{ slug } | null`.
    async fn soft_delete_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Option<String>>;

    /// Restore a previously soft-deleted page. Returns `true` if a row was
    /// affected, `false` otherwise. Mirrors TS `restorePage`.
    async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<bool>;

    /// Hard-delete pages whose `deleted_at` is older than `older_than_hours`
    /// ago. Returns the cleared slugs plus the count. Mirrors TS
    /// `purgeDeletedPages`.
    async fn purge_deleted_pages(&self, older_than_hours: u32) -> crate::Result<PurgeResult>;

    // — Tag CRUD (3) —
    /// Attach `tag` to the page identified by (`slug`, `source_id`). Mirrors
    /// TS `addTag` which throws when the page is missing — Rust returns
    /// `Err(Error::page_not_found(..))` in that case. Idempotent on duplicate
    /// (tag, page) pairs.
    async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()>;

    /// Detach `tag` from the page identified by (`slug`, `source_id`). Mirrors
    /// TS `removeTag` whose sub-select silently no-ops when the page is
    /// missing — Rust preserves that asymmetry and returns `Ok(())`.
    async fn remove_tag(&self, slug: &str, tag: &str, source_id: Option<&str>)
        -> crate::Result<()>;

    /// List the tags currently attached to (`slug`, `source_id`), ordered by
    /// tag ascending. Mirrors TS `getTags` which returns `[]` for missing
    /// pages.
    async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> crate::Result<Vec<String>>;

    // — Search (1) —
    /// Keyword-based page search.
    ///
    /// Simple single-pass keyword matching against title, compiled_truth, and
    /// frontmatter. No vector search yet — that's for a later slice. Results
    /// are ordered by relevance score (descending).
    ///
    /// Default implementation returns an empty Vec — override with actual
    /// implementation per backend. InMemory + libsql override with real
    /// lexical+vector fusion via `fuse_and_boost`; Postgres still falls back to
    /// this empty default (a PG brain returns no query results).
    /// Postgres gap registered in docs/plans/KNOWN-GAPS.md (G23).
    async fn search_pages(&self, _opts: &SearchOpts) -> crate::Result<Vec<SearchResult>> {
        Ok(Vec::new())
    }

    /// Search for pages by embedding vector similarity (cosine distance).
    /// 
    /// Used by `search_by_image` op after embedding the query image to get
    /// visually similar candidate pages. Returns the top-N candidates sorted
    /// by descending similarity (best first).
    /// 
    /// Default implementation returns an empty Vec. Only InMemory and libsql
    /// need an implementation because their `content_chunks` table stores
    /// the embedding column for chunks when `search.unified_multimodal` is enabled.
    /// Postgres stores the embedding but vector search is not yet implemented.
    async fn search_pages_by_embedding(
        &self,
        _query_embedding: &[f32],
        _limit: usize,
        _source_id: Option<&str>,
    ) -> crate::Result<Vec<Page>> {
        Ok(Vec::new())
    }

    // — Image-search daily budget (1-6-7-11: search_by_image) —
    /// Accumulated image-search spend (cents) for a client since UTC
    /// midnight. Backs the per-client daily cap enforced by the
    /// `search_by_image` op. Default returns `0` (cap effectively off for
    /// backends that don't persist the `image_search_spend_log` table).
    async fn image_search_daily_spend_cents(&self, _client_id: &str) -> crate::Result<i64> {
        Ok(0)
    }

    /// Record one completed image-search API spend row (cents) for audit and
    /// daily-cap accounting. `provider`/`model` identify the embedding API
    /// used. Default is a no-op so backends without the table stay buildable.
    async fn record_image_search_spend(
        &self,
        _client_id: &str,
        _amount_cents: i64,
        _provider: &str,
        _model: &str,
    ) -> crate::Result<()> {
        Ok(())
    }

    // — Content refresh (2) —
    /// Update `compiled_truth`, `timeline`, `content_hash` for an existing
    /// page (typically after a re-importer pass). Mirrors TS
    /// `refreshPageBody`.
    async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> crate::Result<()>;

    /// Update the `contextual_retrieval_mode` + `corpus_generation` columns.
    /// `mode` is `&str` (not `CRMode`) in 6a so we can ship without
    /// re-validating every TS string literal; the S6-T2 review may upgrade
    /// the param to `CRMode` if the enum is found to be stable.
    async fn update_page_contextual_retrieval_state(
        &self,
        slug: &str,
        source_id: &str,
        mode: &str,
        corpus_generation: Option<&str>,
    ) -> crate::Result<()>;

    // — Raw data / versions / slug rewrite (7) —
    /// Upsert raw sidecar data for a page. Mirrors TS `putRawData`.
    async fn put_raw_data(
        &self,
        slug: &str,
        source: &str,
        data: &Value,
        source_id: Option<&str>,
    ) -> crate::Result<()>;

    /// Get raw sidecar data for a page. Mirrors TS `getRawData`.
    async fn get_raw_data(
        &self,
        slug: &str,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<RawData>>;

    /// Create a version snapshot for a page. Mirrors TS `createVersion`.
    async fn create_version(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<PageVersion>;

    /// Get all versions for a page, newest-first. Mirrors TS `getVersions`.
    async fn get_versions(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<PageVersion>>;

    /// Revert a page to a previous version. Mirrors TS `revertToVersion`.
    async fn revert_to_version(
        &self,
        slug: &str,
        version_id: u64,
        source_id: Option<&str>,
    ) -> crate::Result<()>;

    /// Update a page's slug. Mirrors TS `updateSlug`.
    async fn update_slug(
        &self,
        old_slug: &str,
        new_slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<()>;

    /// Rewrite links after a slug change. Mirrors TS `rewriteLinks`.
    /// Explicit no-op; links use integer page_id foreign keys.
    async fn rewrite_links(&self, _old_slug: &str, _new_slug: &str) -> crate::Result<()>;

    /// Migrate DB fact rows from a phantom (unprefixed) slug to its canonical
    /// (prefixed) slug. Active rows only (`expired_at IS NULL`) so the
    /// supersession audit trail is undisturbed. Returns the number of fact
    /// rows moved. Mirrors TS `migrateFactsToCanonical`.
    ///
    /// Default: no-op returning `Ok(0)` — engines without persistent facts
    /// (e.g. `InMemoryEngine`) have nothing to migrate; the in-cycle caller
    /// (phantom_redirect) treats `0` as "no facts needed moving" and proceeds.
    async fn migrate_facts_to_canonical(
        &self,
        _phantom_slug: &str,
        _canonical_slug: &str,
        _source_id: &str,
    ) -> crate::Result<i64> {
        Ok(0)
    }

    // — Bulk slug / ref enumeration (3) —
    /// Return the set of all live (non-soft-deleted) slugs, optionally
    /// scoped to `source_id`. Mirrors TS `getAllSlugs`.
    async fn get_all_slugs(
        &self,
        source_id: Option<&str>,
    ) -> crate::Result<std::collections::HashSet<String>>;

    /// Return every live `(slug, source_id)` pair, ordered by
    /// `(source_id, slug)` ascending. Mirrors TS `listAllPageRefs`.
    async fn list_all_page_refs(&self) -> crate::Result<Vec<PageRef>>;

    /// Return pages with zero inbound links from live pages. Mirrors TS
    /// `findOrphanPages` — discovered late in S6-T0 (was missing from the
    /// initial 12-method tally). Both sides of the join must filter out
    /// soft-deleted rows.
    async fn find_orphan_pages(&self) -> crate::Result<Vec<OrphanPage>>;

    /// Statistical anomalies in recent page activity, grouped by cohort
    /// (`tag` / `type`). Mirrors TS `engine.findAnomalies`:
    ///
    /// * Builds a densified per-cohort daily count baseline over
    ///   `[since - lookback_days, since)` and a target-day count snapshot.
    /// * Delegates the threshold math to [`crate::anomaly::compute_anomalies_from_buckets`].
    ///
    /// The three backends rewrite the SQL per dialect (Libsql recursive date
    /// CTE + `substr`/`json_group_array`; Postgres `generate_series`/`date_trunc`/
    /// `array_agg`; InMemory in-Rust) but share
    /// [`crate::anomaly::resolve_anomaly_windows`] for identical window semantics.
    ///
    /// Default impl returns `Unsupported` so a backend can defer the port
    /// (Postgres lands in a later slice); override to implement.
    async fn find_anomalies(
        &self,
        opts: crate::anomaly::AnomaliesOpts,
    ) -> crate::Result<Vec<crate::anomaly::AnomalyResult>> {
        let _ = opts;
        Err(crate::Error::unsupported(
            "find_anomalies not yet implemented for this backend",
        ))
    }

    // — Batch timestamps / scores (3) —
    /// Resolve `slug` → `COALESCE(updated_at, created_at)` for many slugs at
    /// once. Mirrors TS `getPageTimestamps`, including deleted-row visibility:
    /// the TS query does not filter `deleted_at`. Missing slugs are omitted
    /// from the returned map (caller must handle absence).
    ///
    /// Values are ISO-8601 strings, matching the rest of the core API (see
    /// `Page::created_at` / `Page::updated_at`). §13 originally specified
    /// `chrono::DateTime<Utc>`; we keep `String` to avoid pulling `chrono`
    /// into `zbrain-core` and to stay aligned with `Page`'s field types.
    /// Deviation logged in §13.6.
    async fn get_page_timestamps(
        &self,
        slugs: &[String],
    ) -> crate::Result<std::collections::HashMap<String, String>>;

    /// Resolve `(slug, source_id)` → `COALESCE(effective_date, updated_at,
    /// created_at)`. Key format: `"{source_id}::{slug}"` so the caller can
    /// disambiguate slugs that collide across sources. Mirrors TS
    /// `getEffectiveDates`.
    ///
    /// Values are ISO-8601 strings; see `get_page_timestamps` for rationale.
    async fn get_effective_dates(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, String>>;

    /// Compute the salience score for each ref. Formula (mirrors TS
    /// `getSalienceScores`):
    ///
    /// ```text
    /// score = COALESCE(emotional_weight, 0) * 5
    ///       + ln(1 + distinct_active_take_count)
    /// ```
    ///
    /// **Phase 7**: the `takes` table is now available via
    /// [`get_takes_for_page`]. Backends SHOULD compute the real
    /// `distinct_active_take_count` rather than hard-coding 0.
    /// The red test `page_methods_salience_scores_takes_zero_until_6c`
    /// should be updated to verify the takes term contributes.
    async fn get_salience_scores(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, f64>>;

    // ── Salience (Phase 7C 1-3-2) ──────────────────────────────────────────

    /// Bump `salience_touched_at` to now for a page identified by (slug, source_id).
    /// Returns `true` if the page was found and bumped, `false` if no such page.
    /// Does not bump `generation` (salience is excluded from the generation trigger
    /// to avoid over-invalidating the query cache).
    async fn touch_salience(&self, slug: &str, source_id: &str) -> crate::Result<bool> {
        let _ = slug;
        let _ = source_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "touch_salience not yet implemented for this engine",
        ))
    }

    /// Salience query: pages recently touched (within `days`), ranked by
    /// `emotional_weight * 5 + ln(1 + take_count) + recency_decay`.
    ///
    /// Recency decay: flat mode `1.0 / (1.0 + days_old)` where `days_old`
    /// is computed from `updated_at`. The time window uses
    /// `GREATEST(updated_at, COALESCE(salience_touched_at, updated_at))`
    /// so pages bumped by `touch_salience` are included.
    async fn get_recent_salience(
        &self,
        days: u32,
        limit: u32,
        slug_prefix: Option<&str>,
    ) -> crate::Result<Vec<crate::types::SalienceResult>> {
        let _ = days;
        let _ = limit;
        let _ = slug_prefix;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_recent_salience not yet implemented for this engine",
        ))
    }

    // ── Takes (Phase 7A) ──────────────────────────────────────────────────

    /// Return takes for a page, ordered by `row_num` ascending.
    ///
    /// When `takes_holders_allow_list` is `Some(list)`, only rows whose
    /// `holder` is in `list` are returned — this is the server-side filter
    /// backing the v0.28+ per-token visibility model. `None` returns all
    /// holders (trusted local callers). Mirrors TS `getTakesForPage(pageId,
    /// { takesHoldersAllowList })`.
    async fn get_takes_for_page(
        &self,
        page_id: u64,
        takes_holders_allow_list: Option<Vec<String>>,
    ) -> crate::Result<Vec<Take>> {
        let _ = (page_id, takes_holders_allow_list);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_takes_for_page not yet implemented for this engine",
        ))
    }

    /// List takes across pages with holder/kind/active/resolved filters plus
    /// the per-token `takes_holders_allow_list` server-side filter.
    /// Mirrors TS `listTakes(opts)`.
    async fn list_takes(&self, opts: &TakesListOpts) -> crate::Result<Vec<Take>> {
        let _ = opts;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_takes not yet implemented for this engine",
        ))
    }

    /// Full-text search takes by claim, honoring the per-token
    /// `takes_holders_allow_list` server-side filter. Mirrors TS
    /// `searchTakes(query, opts)`.
    async fn search_takes(
        &self,
        query: &str,
        opts: &SearchTakesOpts,
    ) -> crate::Result<Vec<TakeHit>> {
        let _ = (query, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "search_takes not yet implemented for this engine",
        ))
    }

    /// Batch-upsert takes for a page with append-only semantics.
    /// Each `TakeInput` is inserted as a new row; the caller is
    /// responsible for supersede logic via the fence parser. Returns
    /// the count of upserted rows and weight-clamp events.
    /// Mirrors TS `addTakesBatch(pageId, takes)`.
    async fn add_takes_batch(
        &self,
        page_id: u64,
        takes: &[TakeInput],
    ) -> crate::Result<UpsertTakesResult> {
        let _ = (page_id, takes);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "add_takes_batch not yet implemented for this engine",
        ))
    }

    /// Batch-load the inputs needed to recompute emotional weight for pages.
    ///
    /// `slugs` filters to specific pages (incremental mode); `None` => all
    /// non-deleted pages (full-brain recompute). Each returned [`EmotionalInput`]
    /// carries the page's tags (from `frontmatter["tags"]`) plus its active takes.
    /// Mirrors TS `batchLoadEmotionalInputs`.
    async fn batch_load_emotional_inputs(
        &self,
        slugs: Option<&[String]>,
    ) -> crate::Result<Vec<EmotionalInput>>;

    /// Write recomputed emotional weights back to pages. Keyed by the composite
    /// `(slug, source_id)`, bumping `salience_touched_at` so salient old pages
    /// surface in retrieval. Returns the number of pages updated.
    ///
    /// NOTE: TS also persists `emotional_weight_recomputed_at` here; that column
    /// is consumed only by the `backfill-registry` consumer, whose Rust wiring is
    /// deferred to the 1-6 orchestration node. See roadmap 1-2 notes.
    /// Mirrors TS `setEmotionalWeightBatch`.
    async fn set_emotional_weight_batch(&self, writes: &[EmotionalWeightWrite]) -> crate::Result<u64>;

    /// Insert a single proposed take into the `take_proposals` queue.
    /// Idempotency is enforced at the storage layer via the composite unique
    /// index on `(source_id, page_slug, content_hash, prompt_version)`; the
    /// INSERT is conflict-safe (`ON CONFLICT ... DO NOTHING`). Returns the
    /// inserted row id (0 when the conflict guard swallowed the row). Callers
    /// should pre-check [`BrainEngine::take_proposal_exists`] for the cache
    /// hit/miss accounting, mirroring `propose-takes.ts`.
    async fn add_take_proposal(&self, proposal: &TakeProposalInput) -> crate::Result<u64> {
        let _ = proposal;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "add_take_proposal not yet implemented for this engine",
        ))
    }

    /// Whether a proposal already exists for the exact idempotency tuple
    /// `(source_id, page_slug, content_hash, prompt_version)`. Drives the
    /// `propose-takes` cache hit/miss accounting so an unchanged page never
    /// re-spends LLM tokens.
    async fn take_proposal_exists(
        &self,
        source_id: &str,
        page_slug: &str,
        content_hash: &str,
        prompt_version: &str,
    ) -> crate::Result<bool> {
        let _ = (source_id, page_slug, content_hash, prompt_version);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "take_proposal_exists not yet implemented for this engine",
        ))
    }

    /// Insert a single verdict into the `take_grade_cache` cache. Idempotency is
    /// enforced at the storage layer via the composite primary key
    /// `(take_id, prompt_version, judge_model_id, evidence_signature)`; the
    /// INSERT is conflict-safe (`ON CONFLICT ... DO NOTHING`). Returns the number
    /// of rows written (0 when the conflict guard swallowed the row). Callers
    /// should pre-check [`BrainEngine::take_grade_cache_exists`] for the cache
    /// hit/miss accounting, mirroring `grade-takes.ts`.
    async fn add_take_grade_cache(&self, entry: &TakeGradeCacheInput) -> crate::Result<u64> {
        let _ = entry;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "add_take_grade_cache not yet implemented for this engine",
        ))
    }

    /// Whether a verdict cache row already exists for the exact idempotency
    /// tuple `(take_id, prompt_version, judge_model_id, evidence_signature)`.
    /// Drives the `grade-takes` cache hit/miss accounting so a re-run with
    /// unchanged evidence/verdict never re-spends LLM tokens.
    async fn take_grade_cache_exists(
        &self,
        take_id: u64,
        prompt_version: &str,
        judge_model_id: &str,
        evidence_signature: &str,
    ) -> crate::Result<bool> {
        let _ = (take_id, prompt_version, judge_model_id, evidence_signature);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "take_grade_cache_exists not yet implemented for this engine",
        ))
    }

    /// Read a cached dream-cycle significance verdict for the exact
    /// `(file_path, content_hash)` pair. Returns `None` on a cache miss so the
    /// caller can spend an LLM call to (re)judge. Mirrors `synthesize.ts`
    /// `getDreamVerdict`.
    async fn get_dream_verdict(
        &self,
        file_path: &str,
        content_hash: &str,
    ) -> crate::Result<Option<DreamVerdict>> {
        let _ = (file_path, content_hash);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_dream_verdict not yet implemented for this engine",
        ))
    }

    /// Upsert a dream-cycle significance verdict. The composite primary key
    /// `(file_path, content_hash)` makes the write idempotent — a re-judge of
    /// the same file+hash refreshes `worth_processing`/`reasons`/`judged_at`.
    /// Mirrors `synthesize.ts` `putDreamVerdict`.
    async fn put_dream_verdict(
        &self,
        file_path: &str,
        content_hash: &str,
        verdict: &DreamVerdictInput,
    ) -> crate::Result<()> {
        let _ = (file_path, content_hash, verdict);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "put_dream_verdict not yet implemented for this engine",
        ))
    }

    // ---- engine config store (1-3-4-6) ----
    // Key/value store backing dream.synthesize.* settings and cooldown
    // timestamps. Mirrors TS `engine.getConfig/setConfig/unsetConfig`
    // (src/core/engine.ts:1589-1596) over the `config` table.

    /// Read a config value by `key`. Returns `None` if the key is unset.
    /// Mirrors TS `engine.getConfig`.
    async fn get_config(&self, key: &str) -> crate::Result<Option<String>> {
        let _ = key;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_config not yet implemented for this engine",
        ))
    }

    /// Upsert a config value. Mirrors TS `engine.setConfig` (UPSERT on `key`).
    async fn set_config(&self, key: &str, value: &str) -> crate::Result<()> {
        let _ = (key, value);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "set_config not yet implemented for this engine",
        ))
    }

    /// Delete a config key, returning the number of affected rows (0 if the
    /// key was absent). Mirrors TS `engine.unsetConfig`.
    async fn unset_config(&self, key: &str) -> crate::Result<u64> {
        let _ = key;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "unset_config not yet implemented for this engine",
        ))
    }

    /// Evidence rows linking a synthesis page to the take pages it cited.
    /// Written by `persistSynthesis` (auto-think auto_commit path). Mirrors
    /// TS `engine.addSynthesisEvidence`.
    async fn add_synthesis_evidence(
        &self,
        rows: &[SynthesisEvidenceInput],
    ) -> crate::Result<u64> {
        let _ = rows;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "add_synthesis_evidence not yet implemented for this engine",
        ))
    }

    /// Collect `(slug, source_id)` pairs written by child minion jobs via the
    /// `brain_put_page` tool (status = 'complete'). Mirrors TS
    /// `collectChildPutPageSlugs` over `subagent_tool_executions`
    /// (src/core/cycle/synthesize.ts:1022). The source_id is always `'default'`
    /// because subagents are scoped to a single (default) source.
    async fn collect_child_put_page_slugs(
        &self,
        child_ids: &[i64],
    ) -> crate::Result<Vec<(String, String)>> {
        let _ = child_ids;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "collect_child_put_page_slugs not yet implemented for this engine",
        ))
    }

    // ---- op_checkpoints (shared resume state for long-running ops) ----
    // Ported for the conversation-facts-backfill cycle phase. The TS
    // `runExtractConversationFactsCore` stores per-(source,slug) resume state
    // as "<sourceId>|<slug>|<endIso>" string entries in `op_checkpoints`.

    /// Load the `completed_keys` array for `(op, fingerprint)`. Returns an
    /// empty vec when no row exists.
    async fn load_op_checkpoint(
        &self,
        op: &str,
        fingerprint: &str,
    ) -> crate::Result<Vec<String>> {
        let _ = (op, fingerprint);
        Ok(Vec::new())
    }

    /// Upsert the `completed_keys` array for `(op, fingerprint)`.
    async fn save_op_checkpoint(
        &self,
        op: &str,
        fingerprint: &str,
        completed_keys: &[String],
    ) -> crate::Result<()> {
        let _ = (op, fingerprint, completed_keys);
        Ok(())
    }

    /// Delete the `op_checkpoints` row for `(op, fingerprint)`.
    async fn clear_op_checkpoint(&self, op: &str, fingerprint: &str) -> crate::Result<()> {
        let _ = (op, fingerprint);
        Ok(())
    }

    /// Highest `row_num`+1 already stored for `(source_id, slug)` in `facts`,
    /// used to continue the page-global row accumulator across resume runs.
    /// Returns 0 when no facts exist for that (source_id, slug).
    async fn peek_fact_row_num_start(&self, source_id: &str, slug: &str) -> crate::Result<i64> {
        let _ = (source_id, slug);
        Ok(0)
    }

    /// Resolve a take by `(page_id, row_num)` — stores resolution
    /// quality/outcome/evidence/value fields. Mirrors TS `resolveTake`.
    async fn resolve_take(
        &self,
        page_id: u64,
        row_num: i32,
        resolution: &TakeResolution,
    ) -> crate::Result<()> {
        let _ = (page_id, row_num, resolution);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "resolve_take not yet implemented for this engine",
        ))
    }

    // ── Links / Backlinks / Graph (Phase 7B) ───────────────────────────────

    /// Batch-upsert links with `ON CONFLICT DO NOTHING` semantics.
    /// Returns the count of newly inserted rows (duplicates are silently
    /// skipped). Mirrors TS `addLinksBatch`.
    async fn add_links_batch(
        &self,
        links: &[LinkBatchInput],
    ) -> crate::Result<usize> {
        let _ = links;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "add_links_batch not yet implemented for this engine",
        ))
    }

    /// Remove one or more links matching `(from_slug, to_slug)`.
    /// When `link_type` is `None`, all link types between the pair are
    /// removed. `link_source` further constrains the delete to a specific
    /// provenance — used by `runAutoLink` reconciliation. Mirrors TS
    /// `removeLink`.
    async fn remove_link(
        &self,
        from: &str,
        to: &str,
        link_type: Option<&str>,
        link_source: Option<&str>,
        from_source_id: Option<&str>,
        to_source_id: Option<&str>,
    ) -> crate::Result<()> {
        let _ = (from, to, link_type, link_source, from_source_id, to_source_id);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "remove_link not yet implemented for this engine",
        ))
    }

    /// Return all outbound links for a page, joined to page slugs.
    /// Optionally scoped by `source_id`. Mirrors TS `getLinks`.
    async fn get_links(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<Link>> {
        let _ = (slug, source_id);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_links not yet implemented for this engine",
        ))
    }

    /// Return all inbound links to a page (backlinks), joined to page slugs.
    /// Optionally scoped by `source_id` on the FROM side. Mirrors TS
    /// `getBacklinks`.
    async fn get_backlinks(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<Link>> {
        let _ = (slug, source_id);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_backlinks not yet implemented for this engine",
        ))
    }

    /// For a list of slugs, return how many inbound links each has.
    /// Used by hybrid search backlink boost. Single SQL query, not N+1.
    /// Slugs with zero inbound links are present in the map with value 0.
    /// Mirrors TS `getBacklinkCounts`.
    async fn get_backlink_counts(
        &self,
        slugs: &[String],
    ) -> crate::Result<std::collections::HashMap<String, u64>> {
        let _ = slugs;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_backlink_counts not yet implemented for this engine",
        ))
    }

    /// BFS-style graph traversal from a root slug. Returns edges with depth
    /// metadata. Mirrors TS `traversePaths`. Supports direction `out`
    /// (default), `in`, or `both`; optional `link_type` filter; and
    /// optional source scoping via `source_id` / `source_ids`.
    async fn traverse_paths(
        &self,
        slug: &str,
        depth: Option<u32>,
        link_type: Option<&str>,
        direction: Option<&str>,
        source_id: Option<&str>,
        source_ids: Option<&[String]>,
    ) -> crate::Result<Vec<GraphPath>> {
        let _ = (slug, depth, link_type, direction, source_id, source_ids);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "traverse_paths not yet implemented for this engine",
        ))
    }

    /// Compute adjacency boosts for a set of page IDs within their
    /// induced sub-graph. Returns `hits` (in-set distinct from_page_id
    /// count) and `cross_source_hits` (distinct OTHER source_id count,
    /// excluding the target's own source) for each input page_id. Empty
    /// input → empty `HashMap`, no SQL.
    ///
    /// Mirrors TS `BrainEngine.getAdjacencyBoosts` (v0.40.4).
    ///
    /// **Source-scope contract**: `page_ids` MUST already be
    /// source-scoped by the caller. This method does NOT filter by
    /// source — cross-source leakage is impossible by construction
    /// because any leaked-in page_id would have to also appear in the
    /// caller's input set. TS equivalent: `hybridSearch` →
    /// `runPostFusionStages`, which is source-scoped upstream.
    async fn get_adjacency_boosts(
        &self,
        page_ids: &[u64],
    ) -> crate::Result<std::collections::HashMap<u64, AdjacencyRow>> {
        let _ = page_ids;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_adjacency_boosts not yet implemented for this engine",
        ))
    }

    // ── Facts (Phase 7B) ──────────────────────────────────────────────────

    /// Insert a fact with automatic supersede semantics.
    /// When `confidence > 0.9` and a same-entity same-kind same-source fact
    /// exists and is still active, the old fact is superseded
    /// (`superseded_by` set to the new row's id) in the same logical
    /// transaction. Returns `Inserted`, `Duplicate`, or `Superseded`.
    /// Mirrors TS `insertFact`.
    async fn insert_fact(
        &self,
        source_id: &str,
        entity_slug: &str,
        input: &crate::types::NewFact,
    ) -> crate::Result<crate::types::FactInsertStatus> {
        let _ = (source_id, entity_slug, input);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "insert_fact not yet implemented for this engine",
        ))
    }

    /// Wipe the DB facts index for a single page prior to re-inserting from
    /// its `## Facts` fence. Scoped to `source_markdown_slug = slug` so
    /// legacy NULL-`source_markdown_slug` rows (`v0.31` hot-memory facts
    /// pending the `v0.32.2` backfill) survive the reconcile pass — matching
    /// TS `deleteFactsForPage`. Returns the number of rows deleted.
    /// Mirrors TS `deleteFactsForPage`.
    async fn delete_facts_for_page(
        &self,
        _slug: &str,
        _source_id: &str,
    ) -> crate::Result<i64> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "delete_facts_for_page not yet implemented for this engine",
        ))
    }

    /// Count legacy `v0.31` fact rows: `row_num IS NULL AND entity_slug IS
    /// NOT NULL`. Used by the `extract_facts` cycle phase as an
    /// empty-fence guard — if any linger, the destructive reconcile pass is
    /// refused until `zbrain apply-migrations --yes` completes `v0.32.2`.
    /// Mirrors TS `extract_facts` legacy pre-check. Returns `0` by default
    /// for engines without a legacy-fact concept.
    async fn count_legacy_fact_rows(&self) -> crate::Result<i64> {
        Ok(0)
    }

    /// v0.41.2.1 (Part12 1-1-2): discover extractable brain pages whose
    /// `content_hash` has no corresponding `atom` row yet (NOT EXISTS
    /// idempotency subquery). Mirrors TS `discoverExtractablePages`.
    ///
    /// Default: `Err(Unsupported)`. `run_extract_atoms` treats this as
    /// fail-soft (no pages), so engines without an implementation degrade
    /// to `skipped` — matching the inmemory `run_cycle` test path.
    async fn discover_extractable_pages(
        &self,
        _source_id: &str,
        _affected_slugs: Option<&[String]>,
    ) -> crate::Result<Vec<crate::types::DiscoveredPage>> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "discover_extractable_pages not implemented for this engine",
        ))
    }

    /// v0.41.2.1 (Part12 1-1-2): transcript-side source-hash idempotency
    /// check. Returns `true` if ANY `atom` row exists for
    /// `(source_id, content_hash_16)`. Mirrors TS `atomsExistForHash`.
    ///
    /// Default: `Err(Unsupported)`. `run_extract_atoms` treats this as
    /// fail-open (re-extract), matching TS `atomsExistForHash` error
    /// behavior.
    async fn atom_exists_for_hash(
        &self,
        _source_id: &str,
        _content_hash_16: &str,
    ) -> crate::Result<bool> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "atom_exists_for_hash not implemented for this engine",
        ))
    }

    /// List facts for an entity, ordered by `created_at DESC`.
    /// Supports `active_only`, `kinds`, `visibility`, `limit`, `offset`
    /// via `FactListOpts`. Mirrors TS `listFactsByEntity`.
    async fn list_facts_by_entity(
        &self,
        source_id: &str,
        entity_slug: &str,
        opts: &crate::types::FactListOpts,
    ) -> crate::Result<Vec<crate::types::FactRow>> {
        let _ = (source_id, entity_slug, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_facts_by_entity not yet implemented for this engine",
        ))
    }

    /// Return a health snapshot for the facts domain in this source.
    /// Includes active/today/week/expired/consolidated counts and
    /// top entities by fact volume. Mirrors TS `getFactsHealth`.
    async fn get_facts_health(
        &self,
        source_id: &str,
    ) -> crate::Result<crate::types::FactsHealth> {
        let _ = source_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_facts_health not yet implemented for this engine",
        ))
    }

    /// Mark a fact as expired (set `expired_at = now()`).
    /// Returns `true` if a row was affected. Mirrors TS `expireFact`.
    async fn expire_fact(&self, source_id: &str, fact_id: i64) -> crate::Result<bool> {
        let _ = (source_id, fact_id);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "expire_fact not yet implemented for this engine",
        ))
    }

    /// Chart the typed-claim / event trajectory of an entity over time.
    /// Mirrors TS `BrainEngine.findTrajectory` (v0.35.4, D-CDX-6).
    /// Returns facts rows for `entity_slug` ordered by `(valid_from ASC, id ASC)`
    /// with optional metric / kind / visibility / date-window filters.
    /// `InMemoryEngine` relies on this default (Unsupported) — facts queries are
    /// not supported in the in-memory backend, matching `list_facts_by_entity`.
    async fn find_trajectory(
        &self,
        opts: &crate::types::TrajectoryOpts,
    ) -> crate::Result<Vec<crate::types::TrajectoryPoint>> {
        let _ = opts;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "find_trajectory not yet implemented for this engine",
        ))
    }

    // ── Eval candidates (G74 1-1-4) ────────────────────────────────────────
    //
    // Shared substrate for eval-export / eval-prune / eval-replay /
    // eval-whoknows-L2. Default impls return empty/0 so engines that predate
    // the substrate (incl. the InMemory test engine) compile unchanged;
    // libsql and postgres override with real SQL against the 0030 table.

    /// List captured eval candidates, newest first, filtered by
    /// `EvalCandidateFilter`. Mirrors TS `engine.listEvalCandidates`.
    async fn list_eval_candidates(
        &self,
        filter: &EvalCandidateFilter,
    ) -> crate::Result<Vec<EvalCandidate>> {
        let _ = filter;
        Ok(Vec::new())
    }

    /// Delete eval candidates created before `before` (ISO-8601 timestamp).
    /// Returns the number of rows deleted. Mirrors TS
    /// `engine.deleteEvalCandidatesBefore`; drives `eval-prune --older-than`.
    async fn delete_eval_candidates_before(&self, before: &str) -> crate::Result<u64> {
        let _ = before;
        Ok(0)
    }

    /// List facts created since a given ISO timestamp within a source, newest
    /// first. Mirrors TS `listFactsSince`. The `entity_slug` opt narrows the
    /// scan to a single entity when present.
    async fn list_facts_since(
        &self,
        source_id: &str,
        since_iso: &str,
        opts: &crate::types::FactListOpts,
    ) -> crate::Result<Vec<crate::types::FactRow>> {
        let _ = (source_id, since_iso, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_facts_since not yet implemented for this engine",
        ))
    }

    /// List facts captured under a session id within a source, newest first.
    /// Mirrors TS `listFactsBySession`.
    async fn list_facts_by_session(
        &self,
        source_id: &str,
        session_id: &str,
        opts: &crate::types::FactListOpts,
    ) -> crate::Result<Vec<crate::types::FactRow>> {
        let _ = (source_id, session_id, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_facts_by_session not yet implemented for this engine",
        ))
    }

    /// Audit log: facts that were superseded (expired_at + superseded_by both
    /// set), newest first. Mirrors TS `listSupersessions` (drives
    /// `zbrain recall --supersessions`).
    async fn list_supersessions(
        &self,
        source_id: &str,
        opts: &crate::types::SupersessionOpts,
    ) -> crate::Result<Vec<crate::types::FactRow>> {
        let _ = (source_id, opts);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_supersessions not yet implemented for this engine",
        ))
    }

    /// v0.32: count facts not yet promoted to takes by the consolidate phase
    /// (active + unconsolidated). Mirrors TS `countUnconsolidatedFacts`
    /// (drives `zbrain recall --pending`).
    async fn count_unconsolidated_facts(&self, source_id: &str) -> crate::Result<i64> {
        let _ = source_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "count_unconsolidated_facts not yet implemented for this engine",
        ))
    }

    // ─── Minion job queue (Phase 9, slice 1-1-1 A+B) ─────────────────────────
    //
    // Each backend implements these with its own optimal SQL: postgres.rs uses
    // `FOR UPDATE SKIP LOCKED`; libsql.rs uses `BEGIN IMMEDIATE`. The default
    // impls below return Unsupported so engines that predate the queue (and
    // the InMemory test engine) compile unchanged. See `crate::minions`.

    /// Submit a job (basic insert + idempotency). If
    /// `input.idempotency_key` matches an existing row, return that row without
    /// inserting a second. Mirrors TS `MinionQueue.add` (A+B subset).
    async fn enqueue_job(
        &self,
        input: &crate::minions::types::MinionJobInput,
    ) -> crate::Result<crate::minions::types::MinionJob> {
        let _ = input;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "enqueue_job not yet implemented for this engine",
        ))
    }

    /// Fetch a job by id. `None` if not found. Mirrors TS `getJob`.
    async fn get_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_job not yet implemented for this engine",
        ))
    }

    /// List jobs newest-first, filtered/bounded by `filters`. Mirrors TS
    /// `getJobs`.
    async fn get_jobs(
        &self,
        filters: &crate::minions::types::JobFilters,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        let _ = filters;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_jobs not yet implemented for this engine",
        ))
    }

    /// Atomically claim the next eligible waiting job. `None` when no matching
    /// waiting job exists. Token-fenced. Mirrors TS `claim`.
    async fn claim_job(
        &self,
        lock_token: &str,
        lock_duration_ms: i64,
        queue: &str,
        registered_names: &[String],
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = (lock_token, lock_duration_ms, queue, registered_names);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "claim_job not yet implemented for this engine",
        ))
    }

    /// Mark a claimed job completed (token-fenced). `None` on token/status
    /// mismatch. Mirrors TS `completeJob` (core transition; parent hooks are
    /// D-layer / 1-1-3).
    async fn complete_job(
        &self,
        id: i64,
        lock_token: &str,
        result: Option<&serde_json::Value>,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = (id, lock_token, result);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "complete_job not yet implemented for this engine",
        ))
    }

    /// Fail a claimed job (token-fenced) into delayed/failed/dead. `backoff_ms`
    /// sets `delay_until` when `outcome` is `Delayed`. `None` on token/status
    /// mismatch. Mirrors TS `failJob` (core transition; parent hooks 1-1-3).
    async fn fail_job(
        &self,
        id: i64,
        lock_token: &str,
        error_text: &str,
        outcome: crate::minions::types::FailOutcome,
        backoff_ms: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = (id, lock_token, error_text, outcome, backoff_ms);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "fail_job not yet implemented for this engine",
        ))
    }

    /// Extend the lease on an active job (worker heartbeat). `true` if renewed.
    /// Mirrors TS `renewLock`.
    async fn renew_job_lock(
        &self,
        id: i64,
        lock_token: &str,
        lock_duration_ms: i64,
    ) -> crate::Result<bool> {
        let _ = (id, lock_token, lock_duration_ms);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "renew_job_lock not yet implemented for this engine",
        ))
    }

    /// Requeue a failed/dead job to waiting, clearing error/lock/delay. `None`
    /// if not in a failed/dead state. Mirrors TS `retryJob`.
    async fn retry_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "retry_job not yet implemented for this engine",
        ))
    }

    // ─── Minion background sweeps (Phase 9, slice 1-1-2 = C) ─────────────────
    //
    // Four time-driven state-machine transitions the worker/supervisor loop
    // calls periodically. Each is the *pure* transition only (roadmap 1-1-2
    // decision 1): the child_done inbox inserts and waiting-children parent
    // unblock that TS folds into the timeout sweeps are deferred to the D-layer
    // (1-1-3). Backends implement each with their own optimal SQL; the default
    // impls return Unsupported so pre-queue engines compile unchanged.
    //
    // Time handling (roadmap 1-1-2 decisions 4, 6): scheduling columns are
    // epoch-ms integers compared against now; `handle_wall_clock_timeouts`
    // derives its threshold in-SQL (PG `EXTRACT(EPOCH ...)`, SQLite
    // `julianday`). Tests inject past timestamps rather than sleeping.

    /// Promote delayed jobs whose `delay_until` has passed back to `waiting`
    /// (clearing delay/lock). Returns the promoted jobs. Mirrors TS
    /// `promoteDelayed`.
    async fn promote_delayed(
        &self,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "promote_delayed not yet implemented for this engine",
        ))
    }

    /// Sweep stalled active jobs (lease expired, `lock_until < now`). Splits by
    /// stall budget: `stalled_counter + 1 < max_stalled` -> requeued to
    /// `waiting`; otherwise dead-lettered. Returns both sets. Mirrors TS
    /// `handleStalled` (pure sweep; parent hooks are D-layer / 1-1-3).
    async fn handle_stalled(
        &self,
    ) -> crate::Result<crate::minions::types::StalledSweep> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "handle_stalled not yet implemented for this engine",
        ))
    }

    /// Dead-letter active jobs whose per-job `timeout_at` has passed while the
    /// lease is still held (`lock_until > now`, so a stalled job is left for
    /// `handle_stalled` instead). Returns the timed-out jobs. Mirrors TS
    /// `handleTimeouts` (pure sweep; parent hooks are D-layer / 1-1-3).
    async fn handle_timeouts(
        &self,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "handle_timeouts not yet implemented for this engine",
        ))
    }

    /// Dead-letter active jobs that exceed a wall-clock runtime threshold,
    /// regardless of lease state (catches jobs stuck while holding DB
    /// resources, which stall sweeps skip). Threshold in ms:
    /// `timeout_ms` set -> `timeout_ms * 2`; else
    /// `lock_duration_ms * 2 * GREATEST(max_stalled, 1)`. Mirrors TS
    /// `handleWallClockTimeouts` (pure sweep; parent hooks D-layer / 1-1-3).
    async fn handle_wall_clock_timeouts(
        &self,
        lock_duration_ms: i64,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        let _ = lock_duration_ms;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "handle_wall_clock_timeouts not yet implemented for this engine",
        ))
    }

    /// Test-support: force a job's `started_at` record column to an arbitrary
    /// RFC-3339 timestamp. Sweep contract tests use this to synthesize a job
    /// that started far enough in the past to trip `handle_wall_clock_timeouts`
    /// without sleeping (roadmap 1-1-2 decision 6). Not part of the production
    /// surface — each backend rewrites the `started_at` column in place.
    async fn set_started_at_for_test(
        &self,
        id: i64,
        started_at_rfc3339: &str,
    ) -> crate::Result<()> {
        let _ = (id, started_at_rfc3339);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "set_started_at_for_test not implemented for this engine",
        ))
    }

    /// Test-support: force a job's `timeout_at` scheduling column to an
    /// arbitrary epoch-ms value. Sweep contract tests use this to synthesize an
    /// already-expired per-job timeout without violating the
    /// `chk_minion_timeout_positive` CHECK (a negative `timeout_ms` on enqueue
    /// is rejected by the SQL backends). Not part of the production surface.
    async fn set_timeout_at_for_test(&self, id: i64, timeout_at_ms: i64) -> crate::Result<()> {
        let _ = (id, timeout_at_ms);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "set_timeout_at_for_test not implemented for this engine",
        ))
    }

    // ─── Minion dependency graph + inbox (Phase 9, slice 1-1-3-1 = D) ────────
    //
    // The parent<->child coordination layer. These methods share one atomic
    // chain (all keyed on the same `child_done + resolve_parent` semantics and
    // the `minion_inbox` table), so they land together:
    //   - `cancel_job`: recursive-CTE cascade cancel + child_done + resolve.
    //   - `send_message` / `read_inbox` / `read_child_completions`: the inbox
    //     sidechannel, token-fenced.
    //   - `update_tokens`: token-fenced accumulate.
    //   - `remove_child_dependency`: null out a child's parent link.
    // The parent hooks that `enqueue_job` / `complete_job` / `fail_job` grew in
    // this slice (token rollup, child_done emission, on_child_fail policy,
    // resolve_parent) are folded INTO those existing methods, not added here.
    //
    // Backends implement each with their own transaction + SQL; the default
    // impls return Unsupported so pre-queue engines compile unchanged.

    /// Cancel a job and its entire descendant subtree in one transaction.
    /// Uses a recursive CTE (depth-capped at 100) to collect descendants, flips
    /// every non-terminal one to `cancelled`, emits a `child_done`
    /// (outcome=`cancelled`) into each affected parent's inbox, and resolves any
    /// aggregator parent left in `waiting-children`. Returns the root job (the
    /// cancel target), or `None` if it was already terminal / absent. Mirrors TS
    /// `cancelJob` (`queue.ts` L382-460).
    async fn cancel_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "cancel_job not yet implemented for this engine",
        ))
    }

    /// Post a sidechannel message into a job's inbox. `sender` must be `'admin'`
    /// or the job's `parent_job_id` (as a string); any other sender is rejected
    /// (`None`). The target job must be non-terminal. Returns the persisted row.
    /// Mirrors TS `sendMessage` (`queue.ts` L1143-1161).
    async fn send_message(
        &self,
        job_id: i64,
        payload: &serde_json::Value,
        sender: &str,
    ) -> crate::Result<Option<crate::minions::types::InboxMessage>> {
        let _ = (job_id, payload, sender);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "send_message not yet implemented for this engine",
        ))
    }

    /// Read and consume all unread inbox messages for a job (marks `read_at`).
    /// Token-fenced: the caller must currently hold the job's active lease
    /// (`lock_token` matches, status `active`), else returns empty. Mirrors TS
    /// `readInbox` (`queue.ts` L1164-1179).
    async fn read_inbox(
        &self,
        job_id: i64,
        lock_token: &str,
    ) -> crate::Result<Vec<crate::minions::types::InboxMessage>> {
        let _ = (job_id, lock_token);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "read_inbox not yet implemented for this engine",
        ))
    }

    /// Read `child_done` envelopes from a parent's inbox in send order, WITHOUT
    /// marking them read (supports repeated cursor polling). Token-fenced like
    /// `read_inbox`. `since_rfc3339` filters to entries strictly newer than the
    /// cursor. Mirrors TS `readChildCompletions` (`queue.ts` L1232-1262).
    async fn read_child_completions(
        &self,
        parent_id: i64,
        lock_token: &str,
        since_rfc3339: Option<&str>,
    ) -> crate::Result<Vec<crate::minions::types::ChildDoneMessage>> {
        let _ = (parent_id, lock_token, since_rfc3339);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "read_child_completions not yet implemented for this engine",
        ))
    }

    /// Accumulate token counts on an active job (adds to existing). Token-fenced
    /// (`lock_token` matches, status `active`). Returns `true` if applied.
    /// Mirrors TS `updateTokens` (`queue.ts` L1182-1194).
    async fn update_tokens(
        &self,
        id: i64,
        lock_token: &str,
        tokens: &crate::minions::types::TokenUpdate,
    ) -> crate::Result<bool> {
        let _ = (id, lock_token, tokens);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "update_tokens not yet implemented for this engine",
        ))
    }

    /// Set structured progress on an active job (replaces prior progress).
    /// Token-fenced (`lock_token` matches, status `active`). Returns `true` if
    /// applied. Backs `MinionJobContext::update_progress`; mirrors TS
    /// `updateProgress` (`queue.ts` L1014), which the worker exposes on the
    /// job context so handlers can report progress mid-run.
    async fn update_progress(
        &self,
        id: i64,
        lock_token: &str,
        progress: &serde_json::Value,
    ) -> crate::Result<bool> {
        let _ = (id, lock_token, progress);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "update_progress not yet implemented for this engine",
        ))
    }

    /// Append one entry to an active job's `stacktrace` log array. Token-fenced
    /// (`lock_token` matches, status `active`). Returns `true` if applied.
    /// Backs `MinionJobContext::log`; mirrors the TS worker's inline
    /// `executeRaw` that does `stacktrace = COALESCE(stacktrace,'[]') || entry`
    /// (`worker.ts` L703-711). A dedicated trait method rather than raw SQL
    /// keeps zbrain-core free of an `execute_raw` escape hatch.
    async fn append_log(&self, id: i64, lock_token: &str, entry: &str) -> crate::Result<bool> {
        let _ = (id, lock_token, entry);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "append_log not yet implemented for this engine",
        ))
    }

    /// Whether the job is still actively leased by this lock token (status
    /// `active` AND `lock_token` matches). Backs `MinionJobContext::is_active`,
    /// which long-running handlers poll to detect lock loss; mirrors the TS
    /// worker's inline `SELECT id ... WHERE status='active' AND lock_token=$`
    /// (`worker.ts` L712-718).
    async fn is_job_active(&self, id: i64, lock_token: &str) -> crate::Result<bool> {
        let _ = (id, lock_token);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "is_job_active not yet implemented for this engine",
        ))
    }

    /// Sever a child's dependency on its parent (`parent_job_id = NULL`). Used
    /// by the `remove_dep` on-child-fail policy and manual detach. Idempotent.
    /// Mirrors TS `removeChildDependency` (`queue.ts` L1217-1222).
    async fn remove_child_dependency(&self, child_id: i64) -> crate::Result<()> {
        let _ = child_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "remove_child_dependency not yet implemented for this engine",
        ))
    }

    // ─── Minion attachments (Phase 9, slice 1-1-3-2) ─────────────────────────
    //
    // Per-job binary blob storage backed by the `minion_attachments` table.
    // Unlike the inbox/dependency methods above, attachment CRUD is NOT
    // token-fenced: it only checks job existence, not lease ownership (mirrors
    // the TS surface, which takes no lock_token here).
    //
    // Validation (filename safety, size cap, base64, sha256, duplicate) is a
    // backend-agnostic pure function
    // ([`validate_attachment`](crate::minions::attachments::validate_attachment))
    // orchestrated by the facade `MinionQueue::add_attachment`; the backends
    // only verify job existence and run the INSERT of already-decoded bytes.

    /// Insert a validated attachment (already decoded + hashed by the facade).
    /// The backend verifies the parent job exists (returning a `NotFound`
    /// `job N not found` error otherwise) and INSERTs the inline bytes, then
    /// returns the persisted metadata row. `storage_uri` is always NULL for the
    /// current port. Mirrors the INSERT half of TS `addAttachment`
    /// (`queue.ts` L1272-1306).
    async fn insert_attachment(
        &self,
        job_id: i64,
        att: &crate::minions::types::NormalizedAttachment,
    ) -> crate::Result<crate::minions::types::Attachment> {
        let _ = (job_id, att);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "insert_attachment not yet implemented for this engine",
        ))
    }

    /// List existing attachment filenames for a job — the facade uses this for
    /// the friendly duplicate early-out before validation. Order is unspecified.
    /// Mirrors the `SELECT filename` probe in TS `addAttachment`
    /// (`queue.ts` L1284-1287).
    async fn list_attachment_filenames(&self, job_id: i64) -> crate::Result<Vec<String>> {
        let _ = job_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_attachment_filenames not yet implemented for this engine",
        ))
    }

    /// List attachments for a job (metadata only, no bytes), ordered
    /// `created_at ASC, id ASC`. Mirrors TS `listAttachments`
    /// (`queue.ts` L1309-1318).
    async fn list_attachments(
        &self,
        job_id: i64,
    ) -> crate::Result<Vec<crate::minions::types::Attachment>> {
        let _ = job_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_attachments not yet implemented for this engine",
        ))
    }

    /// Fetch a single attachment with its bytes by (job_id, filename). Returns
    /// `None` if absent. When the row exists but `content` is NULL, the bytes are
    /// an empty vec (external-storage rows are not populated in this port).
    /// Mirrors TS `getAttachment` (`queue.ts` L1324-1346).
    async fn get_attachment(
        &self,
        job_id: i64,
        filename: &str,
    ) -> crate::Result<Option<(crate::minions::types::Attachment, Vec<u8>)>> {
        let _ = (job_id, filename);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_attachment not yet implemented for this engine",
        ))
    }

    /// Delete an attachment by (job_id, filename). Returns `true` if a row was
    /// removed. Mirrors TS `deleteAttachment` (`queue.ts` L1349-1355).
    async fn delete_attachment(&self, job_id: i64, filename: &str) -> crate::Result<bool> {
        let _ = (job_id, filename);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "delete_attachment not yet implemented for this engine",
        ))
    }

    // ─── Minion ops (Phase 9, slice 1-1-3-3) ─────────────────────────────────
    //
    // Operator-facing lifecycle + housekeeping: pause/resume a single job,
    // prune terminal jobs, and read aggregate statistics. None are token-fenced
    // (they are admin operations, matching the TS surface). `get_stats` is a
    // pure read; the other three are single-statement writes.

    /// Pause a `waiting`/`active`/`delayed` job (→ `paused`), clearing its lock
    /// so an active worker's abort fires and the handler stops. No-op (`None`)
    /// for any other status. `waiting-children` is intentionally NOT pausable
    /// (pausing an aggregator parent would strand `resolve_parent` against a
    /// paused parent — out of scope; registered in docs/plans/KNOWN-GAPS.md).
    /// Mirrors TS `pauseJob` (`queue.ts` L1119-1128).
    async fn pause_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "pause_job not yet implemented for this engine",
        ))
    }

    /// Resume a `paused` job back to `waiting`, clearing any residual lock.
    /// No-op (`None`) for any non-`paused` status. Mirrors TS `resumeJob`
    /// (`queue.ts` L1131-1140).
    async fn resume_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let _ = id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "resume_job not yet implemented for this engine",
        ))
    }

    /// Delete jobs in terminal statuses older than a cutoff; returns the count
    /// deleted. `statuses` defaults to `[completed, dead, cancelled]` (NOT
    /// `failed` — those stay retryable) at the facade. `older_than_rfc3339` is
    /// compared against the `updated_at` record column (RFC-3339 text; ISO-8601
    /// sorts lexicographically = chronologically on both backends). Sibling
    /// `minion_inbox`/`minion_attachments` rows are removed by the
    /// `ON DELETE CASCADE` FK, not by this method. Mirrors TS `prune`
    /// (`queue.ts` L476-490).
    async fn prune_jobs(
        &self,
        statuses: &[crate::minions::types::MinionJobStatus],
        older_than_rfc3339: &str,
    ) -> crate::Result<i64> {
        let _ = (statuses, older_than_rfc3339);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "prune_jobs not yet implemented for this engine",
        ))
    }

    /// Aggregate queue statistics. `by_status` counts every job by status
    /// (all-time); `by_type` breaks down jobs whose `created_at` is at or after
    /// `since_rfc3339` by name with terminal-outcome counts and mean runtime;
    /// `queue_health` snapshots waiting/active/stalled. `since_rfc3339` compares
    /// against the `created_at` record column (RFC-3339 text). Mirrors TS
    /// `getStats` (`queue.ts` L493-543).
    async fn get_stats(
        &self,
        since_rfc3339: &str,
    ) -> crate::Result<crate::minions::types::QueueStats> {
        let _ = since_rfc3339;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_stats not yet implemented for this engine",
        ))
    }

    /// Thin-client banner identity packet (mirrors TS `get_brain_identity`).
    ///
    /// Default returns the engine kind plus zero counters. Backends that expose
    /// admin stats (Libsql) override this to populate `page_count` /
    /// `chunk_count` from [`crate::admin_queries::AdminQueries::get_full_stats`].
    async fn brain_identity(&self) -> crate::error::Result<BrainIdentity> {
        Ok(BrainIdentity {
            version: env!("CARGO_PKG_VERSION").to_string(),
            engine: engine_kind_str(self.kind()).to_string(),
            page_count: 0,
            chunk_count: 0,
            last_sync_iso: None,
        })
    }

    /// Supervisor-level health probe on the minion jobs table. Returns live
    /// counts of stalled (expired-lease) and waiting jobs, plus the most recent
    /// completion timestamp. Supervisor uses this to detect stalled queues and
    /// connection degradation. PG-only — other backends return `Unsupported`.
    /// Mirrors TS `MinionSupervisor.healthCheck()` (`supervisor.ts`).
    async fn health_check(
        &self,
    ) -> crate::Result<crate::minions::types::SupervisorHealth> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "health_check not yet implemented for this engine",
        ))
    }

    // ─── Minion budget management (roadmap 1-3-2) ──────────────────────────

    /// Attempt to reserve `amount_cents` from a job's remaining budget.
    ///
    /// Uses a CAS-style UPDATE (`WHERE budget_remaining_cents >= amount_cents`)
    /// to guarantee correctness under concurrency without explicit transactions.
    /// On success, an audit log row is written internally.
    ///
    /// Returns `Reserved` on success, or a variant indicating why the
    /// reservation was not granted. All variants are `Ok` — a shortage is a
    /// business decision, not an I/O error.
    async fn reserve_budget(
        &self,
        job_id: i64,
        amount_cents: i64,
        reason: &str,
    ) -> crate::Result<crate::minions::types::ReservationOutcome> {
        let _ = (job_id, amount_cents, reason);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "reserve_budget not yet implemented for this engine",
        ))
    }

    /// Refund `amount_cents` back to a job's remaining budget.
    ///
    /// No-op if the job has no budget. An audit log row (negative `cents_delta`)
    /// is written internally.
    async fn refund_budget(
        &self,
        job_id: i64,
        amount_cents: i64,
        reason: &str,
    ) -> crate::Result<()> {
        let _ = (job_id, amount_cents, reason);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "refund_budget not yet implemented for this engine",
        ))
    }

    /// Set a job as its own budget owner with the given initial budget in cents.
    ///
    /// Sets both `budget_remaining_cents` and `budget_owner_job_id = job_id`
    /// (self-owned). Re-setting an existing budget is allowed — the previous
    /// remaining amount is replaced.
    async fn set_owner_budget(
        &self,
        job_id: i64,
        budget_cents: i64,
    ) -> crate::Result<()> {
        let _ = (job_id, budget_cents);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "set_owner_budget not yet implemented for this engine",
        ))
    }

    /// Clear `budget_remaining_cents` (set to NULL) for all jobs in the
    /// subtree rooted at `owner_job_id`. This effectively halts all budget
    /// reservations for the entire job subtree.
    ///
    /// Returns the number of jobs whose budget was cleared.
    async fn halt_budget_subtree(
        &self,
        owner_job_id: i64,
    ) -> crate::Result<i64> {
        let _ = owner_job_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "halt_budget_subtree not yet implemented for this engine",
        ))
    }

    /// Transfer budget ownership: set `budget_owner_job_id` on `job_id` to
    /// `new_owner_job_id`. This lets a job's spend count against a different
    /// owner's pool (e.g., a tool call inside a parent's budget scope).
    ///
    /// No-op if `job_id` has no budget (`budget_owner_job_id IS NULL`).
    async fn inherit_budget_owner(
        &self,
        job_id: i64,
        new_owner_job_id: i64,
    ) -> crate::Result<()> {
        let _ = (job_id, new_owner_job_id);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "inherit_budget_owner not yet implemented for this engine",
        ))
    }

    /// Return the budget owner (`budget_owner_job_id`) for a job, or `None`
    /// if the job has no budget owner.
    async fn get_budget_owner(
        &self,
        job_id: i64,
    ) -> crate::Result<Option<i64>> {
        let _ = job_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_budget_owner not yet implemented for this engine",
        ))
    }

    /// Acquire a rate lease for `key` (e.g. `"anthropic:messages"`) under
    /// `job_id`, respecting `max_concurrent`. `ttl_ms` sets the expiry from now.
    ///
    /// PG implementation uses `pg_advisory_xact_lock(fnv1a(key))` inside a
    /// self-managed transaction to serialise concurrent acquires on the same
    /// key. Other backends return `Unsupported`.
    ///
    /// Mirrors TS `acquireLease` (`src/core/minions/rate-leases.ts`).
    async fn acquire_rate_lease(
        &self,
        key: &str,
        job_id: i64,
        max_concurrent: i32,
        ttl_ms: i64,
    ) -> crate::Result<crate::minions::types::LeaseAcquireResult> {
        let _ = (key, job_id, max_concurrent, ttl_ms);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "acquire_rate_lease not yet implemented for this engine",
        ))
    }

    /// Extend the expiry of lease `lease_id` by `ttl_ms` from now. Returns
    /// `true` if the lease was still alive, `false` if it was already gone
    /// (pruned or CASCADE-deleted).
    ///
    /// Mirrors TS `renewLease`.
    async fn renew_rate_lease(
        &self,
        lease_id: i64,
        ttl_ms: i64,
    ) -> crate::Result<bool> {
        let _ = (lease_id, ttl_ms);
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "renew_rate_lease not yet implemented for this engine",
        ))
    }

    /// Release (delete) a rate lease by id. Idempotent — deleting a missing
    /// row is a no-op, not an error.
    ///
    /// Mirrors TS `releaseLease`.
    async fn release_rate_lease(
        &self,
        lease_id: i64,
    ) -> crate::Result<()> {
        let _ = lease_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "release_rate_lease not yet implemented for this engine",
        ))
    }

    /// Compute a `BrainHealth` snapshot — page counts, embed coverage,
    /// orphan/dead-link metrics, and the composite `brain_score` (0-100).
    ///
    /// Mirrors TS `engine.getHealth()`. Used by autopilot's targeted-submit
    /// path and `zbrain doctor`. The default implementation returns
    /// "unsupported"; each backend overrides with an efficient query.
    async fn get_health(&self) -> crate::Result<crate::autopilot::brain_score::BrainHealth> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_health not yet implemented for this engine",
        ))
    }

    /// Brain content counters (page/chunk/embedding/link/tag/timeline totals
    /// plus a per-type page breakdown). Mirrors TS `engine.getStats()` →
    /// `BrainStats`. Distinct from the admin `get_stats` (dashboard `Stats`)
    /// and the minions `get_stats(since)` (`QueueStats`) — this is the
    /// brain-content view consumed by the `stats` operation, the CLI banner,
    /// and the `features` recommender. See `BrainStats` docs for the
    /// backend-sourcing caveats. Default is `Unsupported`; real backends
    /// override.
    async fn get_brain_stats(&self) -> crate::Result<crate::admin_queries::BrainStats> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_brain_stats not yet implemented for this engine",
        ))
    }

    /// Enumerate live pages whose embedding has not been computed yet
    /// (`embedding IS NULL AND deleted_at IS NULL`). Powers `features
    /// --auto-fix`'s `embed --stale` step: the caller re-embeds each returned
    /// page and writes the vector back via [`BrainEngine::put_page_embedding`].
    ///
    /// Mirrors the `missing_embeddings` count in [`BrainEngine::get_health`]
    /// but returns the actual rows (not just a count), and excludes
    /// soft-deleted pages. Real backends override with a single indexed
    /// `WHERE` scan; the default is `Unsupported`.
    async fn list_stale_pages(&self) -> crate::Result<Vec<Page>> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "list_stale_pages not yet implemented for this engine",
        ))
    }

    /// Surgically write a page-level embedding (page-level vector, G24) without
    /// touching any other column. Used by `embed --stale` to backfill vectors
    /// for pages whose `embedding` is currently NULL.
    async fn put_page_embedding(
        &self,
        _slug: &str,
        _source_id: &str,
        _embedding: Vec<u8>,
    ) -> crate::Result<()> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "put_page_embedding not yet implemented for this engine",
        ))
    }

    /// Surgically overwrite a page's `timeline` TEXT column without reading or
    /// rewriting the rest of the row.
    async fn set_page_timeline(
        &self,
        _slug: &str,
        _source_id: &str,
        _timeline: String,
    ) -> crate::Result<()> {
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "set_page_timeline not yet implemented for this engine",
        ))
    }

    /// Append a single entry to a page's `timeline` TEXT column. Default impl
    /// reads the page, parses the existing timeline, appends, and writes back
    /// via `set_page_timeline`. Backends may override for a single SQL round-trip.
    async fn add_timeline_entry(
        &self,
        slug: &str,
        source_id: &str,
        entry: &str,
    ) -> crate::Result<()> {
        let current = match self.get_page(slug, &GetPageOpts::default()).await {
            Ok(Some(p)) => p.timeline,
            _ => String::new(),
        };
        let mut lines: Vec<String> = current
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect();
        lines.push(entry.to_string());
        let next = lines.join("\n");
        self.set_page_timeline(slug, source_id, next).await
    }
}

// ─── InMemoryEngine ──────────────────────────────────────────────────────────

/// Internal raw-data entry used by `InMemoryEngine`.
/// Keyed by `(page_id, source)` for upsert semantics.
#[derive(Debug, Clone)]
struct InternalRawData {
    page_id: u64,
    source: String,
    data: Value,
    fetched_at: String,
}

/// Internal link row used by `InMemoryEngine` to store links by page IDs
/// (mirroring the `links` DB table). Converted to public `Link` on read
/// by resolving IDs back to slugs via the page store.
#[derive(Debug, Clone)]
struct InternalLink {
    from_page_id: u64,
    to_page_id: u64,
    link_type: String,
    context: String,
    link_source: Option<String>,
    origin_page_id: Option<u64>,
    origin_field: Option<String>,
}

/// Internal in-memory attachment row: the persisted metadata plus the decoded
/// bytes. Keyed logically by `(meta.job_id, meta.filename)`.
#[derive(Debug, Clone)]
struct InternalAttachment {
    meta: crate::minions::types::Attachment,
    bytes: Vec<u8>,
}

/// Extract the sorted, deduped tag list from `Page::frontmatter["tags"]`.
fn page_tags(page: &Page) -> Vec<String> {
    page.frontmatter
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Set the tag list on `Page::frontmatter["tags"]`, sorting and deduping.
fn set_page_tags(page: &mut Page, mut tags: Vec<String>) {
    tags.sort();
    tags.dedup();
    if let Some(metadata) = page.frontmatter.as_object_mut() {
        metadata.insert("tags".to_string(), json!(tags));
    } else {
        page.frontmatter = json!({"tags": tags});
    }
}

/// Extract the `tags` array from a parsed `frontmatter` JSON value. Shared by
/// the libsql / postgres engine methods that read `frontmatter` as a column.
pub(crate) fn frontmatter_tags(fm: &serde_json::Value) -> Vec<String> {
    fm.get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Extract the `tags` array from a `frontmatter` JSON text. Parsing failures
/// degrade to an empty tag list (matches `page_tags`).
pub(crate) fn frontmatter_tags_text(text: &str) -> Vec<String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => frontmatter_tags(&v),
        Err(_) => Vec::new(),
    }
}

/// Clamp confidence: 1/(1 + 0.3*d) clamped to [0.05, 1.0], matches TS contract.
/// Used by all three backends for consistent confidence calculation.
pub fn clamp_confidence(depth: usize) -> f32 {
    let d = depth as f32;
    let c = 1.0 / (1.0 + 0.3 * d);
    c.max(0.05).min(1.0)
}

/// In-process engine backed by a `Vec<Page>`. Not persistent, not
/// transactional — its only job is to validate the trait contract in unit
/// tests and integration harnesses.
#[derive(Debug, Default)]
pub struct InMemoryEngine {
    store: Mutex<Vec<Page>>,
    next_id: Mutex<u64>,
    file_store: Mutex<Vec<FileRow>>,
    next_file_id: Mutex<u64>,
    /// 1-6-7-5: ingest-log entries (in-memory, for testing).
    ingest_log_store: Mutex<Vec<IngestLogEntry>>,
    next_ingest_id: Mutex<u64>,
    /// Raw sidecar data, keyed by `(page_id, source)`.
    raw_data_store: Mutex<Vec<InternalRawData>>,
    /// Page version snapshots, newest-first within each page.
    version_store: Mutex<Vec<PageVersion>>,
    next_version_id: Mutex<u64>,
    /// Token-scope map for testing: access_token → scopes.
    token_scopes: Mutex<std::collections::HashMap<String, Vec<String>>>,
    /// Source configuration rows for testing: source id → SourceRow.
    sources: Mutex<Vec<SourceRow>>,
    // #110: chunk storage (in-memory, for testing)
    chunk_store: Mutex<std::collections::HashMap<String, Vec<crate::import::ChunkInput>>>,
    chunk_upsert_error: Mutex<Option<crate::error::StructuredError>>,
    // Phase 7A: takes storage (in-memory, for testing)
    takes_store: Mutex<Vec<Take>>,
    next_take_id: Mutex<u64>,
    // Phase 7B: links storage (in-memory, for testing)
    links_store: Mutex<Vec<InternalLink>>,
    // Phase 7B: facts storage (in-memory, for testing)
    facts_store: Mutex<Vec<crate::types::FactRow>>,
    next_fact_id: Mutex<i64>,
    // Phase 9 (1-1-1): minion job queue storage (in-memory, for testing)
    minion_jobs_store: Mutex<Vec<crate::minions::types::MinionJob>>,
    next_job_id: Mutex<i64>,
    // Phase 9 (1-1-3-1): minion inbox storage (in-memory, for testing)
    minion_inbox_store: Mutex<Vec<crate::minions::types::InboxMessage>>,
    next_inbox_id: Mutex<i64>,
    // Phase 9 (1-1-3-2): minion attachment storage (in-memory, for testing).
    // Stores the decoded bytes alongside the metadata so get_attachment returns
    // real content (unlike a metadata-only stub).
    minion_attachments_store: Mutex<Vec<InternalAttachment>>,
    next_attachment_id: Mutex<i64>,
    // 1-6-7-10-1: code-graph edge storage (in-memory, for testing).
    // Holds both resolved (code_edges_chunk) and unresolved (code_edges_symbol)
    // rows; `resolved` flags which table the row would live in.
    code_edges_store: Mutex<Vec<InternalCodeEdge>>,
    next_code_edge_id: Mutex<i64>,
    // 1-6-7-11: image-search spend rows (in-memory, for testing the daily
    // budget cap without a real SQLite/Postgres backend).
    image_search_spend_store: Mutex<Vec<InternalImageSearchSpend>>,
    // 1-1-4: propose_takes phase queue (in-memory, for testing). Mirrors the
    // `take_proposals` table; idempotency key is
    // (source_id, page_slug, content_hash, prompt_version).
    take_proposals_store: Mutex<Vec<InternalTakeProposal>>,
    next_take_proposal_id: Mutex<u64>,
    take_grade_cache_store: Mutex<Vec<InternalTakeGradeCache>>,
    /// 1-3-4-1: dream-cycle significance verdict cache (in-memory, for testing).
    dream_verdicts_store: Mutex<Vec<InternalDreamVerdict>>,
    /// 1-1-6: op_checkpoints resume state (op, fingerprint) -> completed_keys.
    op_checkpoints_store: Mutex<std::collections::HashMap<(String, String), Vec<String>>>,
    /// 1-3-4-6: engine config key/value store (in-memory, for testing).
    config_store: Mutex<std::collections::HashMap<String, String>>,
    /// 1-3-4-6: subagent tool-execution log (in-memory, for testing the read
    /// path; the Rust minion does not yet write this table — KNOWN-GAP).
    subagent_tool_executions_store: Mutex<Vec<InternalSubagentToolExecution>>,
}

/// In-memory `subagent_tool_executions` row (1-3-4-6, read-path testing).
/// Only the fields the synthesize phase reads are modelled; the writer (Rust
/// minion `brain_put_page` tool) is a tracked KNOWN-GAP.
#[derive(Debug, Clone)]
struct InternalSubagentToolExecution {
    job_id: i64,
    tool_name: String,
    status: String,
    /// JSON tool input. The subagent passes `{ slug, ... }`, occasionally
    /// double-encoded as `{ input: { slug, ... } }`. Mirrors TS
    /// `subagent_tool_executions.input` (synthesize.ts:1022).
    input: serde_json::Value,
}

/// In-memory `take_proposals` queue row (1-1-4: propose_takes phase).
/// Mirrors the `take_proposals` table columns written by `propose-takes.ts`.
#[derive(Debug, Clone)]
struct InternalTakeProposal {
    id: u64,
    source_id: String,
    page_slug: String,
    content_hash: String,
    prompt_version: String,
    proposal_run_id: String,
    claim_text: String,
    kind: String,
    holder: String,
    weight: f64,
    domain: Option<String>,
    dedup_against_fence_rows: Option<String>,
    model_id: String,
    status: String,
}

/// In-memory `take_grade_cache` row. Keyed by the composite
/// `(take_id, prompt_version, judge_model_id, evidence_signature)`.
#[derive(Debug, Clone)]
struct InternalTakeGradeCache {
    take_id: u64,
    prompt_version: String,
    judge_model_id: String,
    evidence_signature: String,
    wave_version: String,
    verdict: String,
    confidence: f64,
    applied: bool,
    cost_usd: Option<f64>,
}

/// In-memory `dream_verdicts` row. Keyed by the composite `(file_path,
/// content_hash)`.
#[derive(Debug, Clone)]
struct InternalDreamVerdict {
    file_path: String,
    content_hash: String,
    worth_processing: bool,
    reasons: Vec<String>,
    judged_at: String,
}

/// In-memory code-graph edge row. `resolved == true` mirrors a `code_edges_chunk`
/// row (both endpoints are known chunk IDs); `false` mirrors `code_edges_symbol`
/// (target known only by qualified name).
#[derive(Debug, Clone)]
struct InternalCodeEdge {
    id: i64,
    from_chunk_id: i64,
    to_chunk_id: Option<i64>,
    from_symbol_qualified: String,
    to_symbol_qualified: String,
    edge_type: String,
    edge_metadata: serde_json::Value,
    source_id: Option<String>,
    resolved: bool,
}

/// Map an in-memory code-edge row to the public `CodeEdgeResult` contract.
fn edge_row_to_result(row: &InternalCodeEdge) -> crate::import::CodeEdgeResult {
    crate::import::CodeEdgeResult {
        id: row.id,
        from_chunk_id: row.from_chunk_id,
        to_chunk_id: row.to_chunk_id,
        from_symbol_qualified: row.from_symbol_qualified.clone(),
        to_symbol_qualified: row.to_symbol_qualified.clone(),
        edge_type: row.edge_type.clone(),
        edge_metadata: row.edge_metadata.clone(),
        source_id: row.source_id.clone(),
        resolved: row.resolved,
    }
}

/// In-memory image-search spend row (1-6-7-11: search_by_image daily budget).
#[derive(Debug, Clone)]
struct InternalImageSearchSpend {
    client_id: String,
    amount_cents: i64,
    provider: String,
    model: String,
    /// ISO-8601 UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ` (prefix-compared for
    /// "since UTC midnight" without parsing).
    created_at: String,
}

/// Source scoping for `get_callers_of` / `get_callees_of` (mirrors TS):
/// apply the filter only when a concrete `source_id` is set AND not all-sources.
/// This version works for the in-memory internal storage representation.
fn edge_source_match_inmem(row: &InternalCodeEdge, opts: &crate::import::CodeGraphQueryOpts) -> bool {
    if opts.all_sources {
        return true;
    }
    match &opts.source_id {
        None => true,
        Some(sid) => row.source_id.as_deref() == Some(sid.as_str()),
    }
}

/// Source scoping for `get_callers_of` / `get_callees_of` (mirrors TS):
/// apply the filter only when a concrete `source_id` is set AND not all-sources.
/// This version works on the exported CodeEdgeResult that is returned by the getters.
fn edge_source_match(row: &crate::import::CodeEdgeResult, opts: &crate::import::CodeGraphQueryOpts) -> bool {
    if opts.all_sources {
        return true;
    }
    match &opts.source_id {
        None => true,
        Some(sid) => row.source_id.as_deref() == Some(sid.as_str()),
    }
}

/// Cap a result set to `limit` (default 100, hard cap 500 for callers/callees).
fn apply_edge_limit<T>(out: &mut Vec<T>, limit: Option<usize>) {
    let cap = limit.unwrap_or(100).min(500);
    if out.len() > cap {
        out.truncate(cap);
    }
}

/// Definition-site symbol types (aligns with TS `DEF_TYPES` in
/// `src/commands/code-def.ts`). Used by `find_code_def` to restrict the
/// `symbol_type` column to real definitions rather than usage sites.
fn is_def_type(symbol_type: &str) -> bool {
    matches!(
        symbol_type,
        "function"
            | "class"
            | "interface"
            | "type"
            | "enum"
            | "struct"
            | "trait"
            | "module"
            | "contract"
            | "table"
            | "view"
            | "index"
            | "procedure"
            | "schema"
            | "database"
            | "trigger"
            | "export statement"
    )
}
impl InMemoryEngine {
    /// Create a new empty InMemoryEngine for testing.
    pub fn new() -> Self {
        Self {
            store: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
            file_store: Mutex::new(Vec::new()),
            next_file_id: Mutex::new(1),
            ingest_log_store: Mutex::new(Vec::new()),
            next_ingest_id: Mutex::new(1),
            raw_data_store: Mutex::new(Vec::new()),
            version_store: Mutex::new(Vec::new()),
            next_version_id: Mutex::new(1),
            token_scopes: Mutex::new(std::collections::HashMap::new()),
            sources: Mutex::new(Vec::new()),
            // #110: chunk storage (in-memory, for testing)
            chunk_store: Mutex::new(std::collections::HashMap::new()),
            chunk_upsert_error: Mutex::new(None),
            // Phase 7A: takes storage (in-memory, for testing)
            takes_store: Mutex::new(Vec::new()),
            next_take_id: Mutex::new(1),
            // Phase 7B: links storage (in-memory, for testing)
            links_store: Mutex::new(Vec::new()),
            // Phase 7B: facts storage (in-memory, for testing)
            facts_store: Mutex::new(Vec::new()),
            next_fact_id: Mutex::new(1),
            // Phase 9 (1-1-1): minion job queue storage (in-memory, for testing)
            minion_jobs_store: Mutex::new(Vec::new()),
            next_job_id: Mutex::new(1),
            minion_inbox_store: Mutex::new(Vec::new()),
            next_inbox_id: Mutex::new(1),
            // Phase 9 (1-1-3-2): minion attachment storage (in-memory, for testing)
            minion_attachments_store: Mutex::new(Vec::new()),
            next_attachment_id: Mutex::new(1),
            // 1-6-7-10-1: code-graph edge storage (in-memory, for testing)
            code_edges_store: Mutex::new(Vec::new()),
            next_code_edge_id: Mutex::new(1),
            // 1-6-7-11: image-search spend rows (in-memory, for testing)
            image_search_spend_store: Mutex::new(Vec::new()),
            // 1-1-4: propose_takes phase queue (in-memory, for testing)
            take_proposals_store: Mutex::new(Vec::new()),
            next_take_proposal_id: Mutex::new(1),
            // 1-1-5: grade_takes phase verdict cache (in-memory, for testing)
            take_grade_cache_store: Mutex::new(Vec::new()),
            // 1-3-4-1: dream-cycle significance verdict cache (in-memory, for testing)
            dream_verdicts_store: Mutex::new(Vec::new()),
            // 1-1-6: conversation_facts_backfill resume state (in-memory, for testing)
            op_checkpoints_store: Mutex::new(std::collections::HashMap::new()),
            // 1-3-4-6: engine config + subagent tool-execution log (in-memory)
            config_store: Mutex::new(std::collections::HashMap::new()),
            subagent_tool_executions_store: Mutex::new(Vec::new()),
        }
    }

    /// Wrap in an `Arc` for use as `Arc<dyn BrainEngine>`.
    #[must_use]
    pub fn into_arc(self) -> Arc<dyn BrainEngine> {
        Arc::new(self)
    }

    /// Add a source row for testing webhook source lookups.
    pub fn add_source(&self, source: SourceRow) {
        self.sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned")
            .push(source);
    }

    /// Phase 7A: add a take record directly for test setup.
    /// The take will be inserted with `active = true` and `created_at`/`updated_at` = now.
    pub fn add_take(&self, take: Take) {
        let mut store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let mut next_id = self
            .next_take_id
            .lock()
            .expect("InMemoryEngine next_take_id mutex poisoned");
        if take.id >= *next_id {
            *next_id = take.id + 1;
        }
        store.push(take);
    }

    /// Phase 7B: add a fact row directly for test setup.
    pub fn add_fact(&self, fact: FactRow) {
        let mut store = self
            .facts_store
            .lock()
            .expect("InMemoryEngine facts_store mutex poisoned");
        let mut next_id = self
            .next_fact_id
            .lock()
            .expect("InMemoryEngine next_fact_id mutex poisoned");
        if fact.id >= *next_id {
            *next_id = fact.id + 1;
        }
        store.push(fact);
    }

    /// Configure chunk upserts to fail in tests.
    pub fn fail_chunk_upserts_for_tests(&self, error: crate::error::StructuredError) {
        *self
            .chunk_upsert_error
            .lock()
            .expect("InMemoryEngine chunk_upsert_error mutex poisoned") = Some(error);
    }

    /// Set a page's `emotional_weight` directly for tests. `PageInput` does not
    /// carry `emotional_weight` (it is normally produced by the
    /// `recompute_emotional_weight` pipeline), so salience-boost tests need a
    /// backdoor to seed the value that `get_salience_scores` reads.
    pub fn set_emotional_weight_for_tests(&self, slug: &str, source_id: &str, weight: f64) {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        if let Some(page) = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id && p.deleted_at.is_none())
        {
            page.emotional_weight = Some(weight);
        }
    }

    /// Set a page's `effective_date` directly for tests. The recency stage
    /// reads dates via `get_effective_dates` (COALESCE effective_date /
    /// updated_at / created_at), so seeding `effective_date` gives a
    /// deterministic date without going through the import pipeline.
    pub fn set_effective_date_for_tests(&self, slug: &str, source_id: &str, iso8601: &str) {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        if let Some(page) = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id && p.deleted_at.is_none())
        {
            page.effective_date = Some(iso8601.to_string());
        }
    }

    /// Direct access to the page store for tests that need to seed pages
    /// without going through `put_page` (which requires a source_id).
    pub fn store_for_test(&self) -> std::sync::MutexGuard<'_, Vec<Page>> {
        self.store.lock().expect("InMemoryEngine store mutex poisoned")
    }

    /// Direct access to the chunk store for tests.
    pub fn chunk_store_for_test(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Vec<crate::import::ChunkInput>>>
    {
        self.chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned")
    }

    /// Direct access to the links store for tests.
    pub fn links_store_for_test(&self) -> std::sync::MutexGuard<'_, Vec<InternalLink>> {
        self.links_store
            .lock()
            .expect("InMemoryEngine links_store mutex poisoned")
    }

    /// Direct access to the code-graph edge store for tests (1-6-7-10-1).
    pub fn code_edges_store_for_test(
        &self,
    ) -> std::sync::MutexGuard<'_, Vec<InternalCodeEdge>> {
        self.code_edges_store
            .lock()
            .expect("InMemoryEngine code_edges_store mutex poisoned")
    }

    // ─── Minion D-layer helpers (1-1-3-1) ───────────────────────────────────
    //
    // Shared building blocks for the InMemory parent/child coordination hooks,
    // mirroring the SQL `emit_child_done` INSERT-with-EXISTS-guard and the
    // `resolve_parent` UPDATE. The caller holds the jobs-store lock; these take
    // it by ref so complete_job/fail_job/cancel_job compose them without
    // re-locking (which would deadlock the std Mutex).

    /// Post a `child_done` envelope into `parent_id`'s inbox, but ONLY if the
    /// parent still exists and is non-terminal (mirrors the SQL `WHERE EXISTS
    /// (... status NOT IN terminal)` guard). Locks the inbox store internally.
    fn emit_child_done_inmem(
        &self,
        jobs: &[crate::minions::types::MinionJob],
        parent_id: i64,
        child_id: i64,
        job_name: &str,
        result: serde_json::Value,
        outcome: crate::minions::types::ChildOutcome,
        error: Option<String>,
    ) {
        let parent_open = jobs
            .iter()
            .any(|j| j.id == parent_id && !j.status.is_terminal());
        if !parent_open {
            return;
        }
        let envelope = crate::minions::types::ChildDoneMessage::new(
            child_id, job_name, result, outcome, error,
        );
        let payload = serde_json::to_value(&envelope)
            .expect("ChildDoneMessage serializes to JSON");

        let mut inbox = self
            .minion_inbox_store
            .lock()
            .expect("InMemoryEngine minion_inbox_store mutex poisoned");
        let mut next = self
            .next_inbox_id
            .lock()
            .expect("InMemoryEngine next_inbox_id mutex poisoned");
        let msg_id = *next;
        *next += 1;
        inbox.push(crate::minions::types::InboxMessage {
            id: msg_id,
            job_id: parent_id,
            sender: "minions".to_string(),
            payload,
            sent_at: crate::time::current_utc_iso8601(),
            read_at: None,
        });
    }

    /// Flip a parent out of `waiting-children` back to `waiting` once none of
    /// its children remain non-terminal (mirrors the SQL `resolve_parent`
    /// UPDATE). No-op if the parent isn't waiting-children.
    fn resolve_parent_inmem(jobs: &mut [crate::minions::types::MinionJob], parent_id: i64) {
        use crate::minions::types::MinionJobStatus;
        let any_open = jobs
            .iter()
            .any(|j| j.parent_job_id == Some(parent_id) && !j.status.is_terminal());
        if any_open {
            return;
        }
        if let Some(parent) = jobs
            .iter_mut()
            .find(|j| j.id == parent_id && j.status == MinionJobStatus::WaitingChildren)
        {
            parent.status = MinionJobStatus::Waiting;
            parent.updated_at = crate::time::current_utc_iso8601();
        }
    }
}

/// Returns true if `holder` passes the per-token takes-holders allow-list
/// filter (v0.28+ visibility model).
///
/// - `None` (unset) => all holders allowed (trusted local callers).
/// - `Some(list)` => only holders present in `list` pass; a remote token
///   restricted to a subset of holders cannot read other holders' takes.
fn holder_allowed(holder: &str, allow_list: &Option<Vec<String>>) -> bool {
    match allow_list {
        None => true,
        Some(list) => list.iter().any(|h| h == holder),
    }
}

/// Shared `FactListOpts` predicate for the in-memory fact-list family
/// (`list_facts_by_entity` inlines the same logic; the newer methods share
/// this helper). Mirrors the SQL filters in libsql's
/// `append_fact_list_filters`.
fn fact_passes_list_filters(f: &FactRow, opts: &FactListOpts) -> bool {
    if opts.active_only.unwrap_or(false) && (f.expired_at.is_some() || f.superseded_by.is_some()) {
        return false;
    }
    if let Some(ks) = opts.kinds.as_ref() {
        if !ks.iter().any(|k| f.kind == *k) {
            return false;
        }
    }
    if let Some(vs) = opts.visibility.as_ref() {
        if !vs.iter().any(|v| f.visibility == *v) {
            return false;
        }
    }
    true
}

/// Newest first (mirrors SQL `ORDER BY created_at DESC`).
fn sort_facts_newest_first(rows: &mut [FactRow]) {
    rows.sort_by(|a, b| {
        b.created_at
            .as_deref()
            .unwrap_or("")
            .cmp(&a.created_at.as_deref().unwrap_or(""))
    });
}

/// Apply `offset` + `limit` paging from `FactListOpts` (mirrors SQL
/// `LIMIT ? OFFSET ?`).
fn apply_fact_paging(rows: &mut Vec<FactRow>, opts: &FactListOpts) {
    let offset = opts.offset.unwrap_or(0) as usize;
    if offset > 0 {
        *rows = rows.split_off(offset.min(rows.len()));
    }
    if let Some(limit) = opts.limit {
        rows.truncate(limit as usize);
    }
}

#[async_trait]
impl BrainEngine for InMemoryEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::InMemory
    }

    async fn connect(&self, _config: &EngineConfig) -> crate::Result<()> {
        Ok(())
    }

    async fn disconnect(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn init_schema(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn get_source_by_github_repo(
        &self,
        github_repo: &str,
    ) -> crate::Result<Option<SourceRow>> {
        let sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");
        Ok(sources
            .iter()
            .find(|s| {
                s.config
                    .get("github_repo")
                    .and_then(|v| v.as_str())
                    .is_some_and(|repo| repo == github_repo)
            })
            .cloned())
    }

    async fn list_sources(&self, include_archived: bool) -> crate::Result<Vec<SourceRow>> {
        let sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");
        Ok(sources
            .iter()
            .filter(|s| include_archived || !s.archived)
            .cloned()
            .collect())
    }

    async fn get_source(&self, id: &str) -> crate::Result<Option<SourceRow>> {
        let sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");
        Ok(sources.iter().find(|s| s.id == id).cloned())
    }

    async fn source_sync_stats(
        &self,
    ) -> crate::Result<Vec<crate::sync_status::SourceSyncStat>> {
        let sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let chunk_store = self
            .chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");

        // slug -> (source_id, deleted) so we can attribute chunks to sources
        // without a chunk-level source_id (ChunkInput carries only a kind).
        let slug_index: std::collections::HashMap<String, (String, bool)> = store
            .iter()
            .map(|p| (p.slug.clone(), (p.source_id.clone(), p.deleted_at.is_some())))
            .collect();

        let mut out: Vec<crate::sync_status::SourceSyncStat> = Vec::new();
        for src in sources.iter() {
            let sync_enabled = src
                .config
                .get("syncEnabled")
                .and_then(|v| v.as_bool())
                != Some(false);
            let pages = store
                .iter()
                .filter(|p| p.source_id == src.id && p.deleted_at.is_none())
                .count() as u64;

            let mut chunks_total = 0u64;
            let mut chunks_unembedded = 0u64;
            for (slug, chunks) in chunk_store.iter() {
                if let Some((psrc, deleted)) = slug_index.get(slug) {
                    if *psrc == src.id && !*deleted {
                        for c in chunks.iter() {
                            chunks_total += 1;
                            if c.embedding.is_none() {
                                chunks_unembedded += 1;
                            }
                        }
                    }
                }
            }

            out.push(crate::sync_status::SourceSyncStat {
                source_id: src.id.clone(),
                name: src.name.clone(),
                local_path: src.local_path.clone(),
                sync_enabled,
                last_sync_at: src.last_sync_at.clone(),
                last_commit: src.last_commit.clone(),
                pages,
                chunks_total,
                chunks_unembedded,
            });
        }
        Ok(out)
    }

    async fn create_source(&self, input: &CreateSourceInput) -> crate::Result<SourceRow> {
        // Validate source id format
        if !is_valid_source_id(&input.id) {
            return Err(Error::engine(format!(
                "invalid source id: '{}' — must match ^[a-z0-9](?:[a-z0-9-]{{0,30}}[a-z0-9])?$",
                input.id
            )));
        }

        let mut sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");

        // Check uniqueness
        if sources.iter().any(|s| s.id == input.id) {
            return Err(Error::engine(format!(
                "source id '{}' already exists",
                input.id
            )));
        }
        if sources.iter().any(|s| s.name == input.name) {
            return Err(Error::engine(format!(
                "source name '{}' already exists",
                input.name
            )));
        }

        let row = SourceRow {
            id: input.id.clone(),
            name: input.name.clone(),
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config: input.config.clone().unwrap_or_default(),
            created_at: Some(current_utc_iso8601()),
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        };
        sources.push(row.clone());
        Ok(row)
    }

    async fn update_source(
        &self,
        id: &str,
        input: &UpdateSourceInput,
    ) -> crate::Result<SourceRow> {
        let mut sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");
        let idx = sources
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| Error::engine(format!("source '{}' not found", id)))?;

        // Check name uniqueness before taking mutable borrow
        if let Some(ref name) = input.name {
            if sources.iter().any(|other| other.id != id && other.name == *name) {
                return Err(Error::engine(format!(
                    "source name '{}' already taken",
                    name
                )));
            }
        }

        let s = &mut sources[idx];
        if let Some(ref name) = input.name {
            s.name = name.clone();
        }
        if let Some(ref config) = input.config {
            s.config = config.clone();
        }
        if input.local_path.is_some() {
            s.local_path = input.local_path.clone();
        }
        if input.last_commit.is_some() {
            s.last_commit = input.last_commit.clone();
        }
        if input.last_sync_at.is_some() {
            s.last_sync_at = input.last_sync_at.clone();
        }
        if input.chunker_version.is_some() {
            s.chunker_version = input.chunker_version.clone();
        }
        if input.contextual_retrieval_mode.is_some() {
            s.contextual_retrieval_mode = input.contextual_retrieval_mode.clone();
        }
        if let Some(trust) = input.trust_frontmatter_overrides {
            s.trust_frontmatter_overrides = trust;
        }
        Ok(s.clone())
    }

    async fn delete_source(&self, id: &str) -> crate::Result<bool> {
        let mut sources = self
            .sources
            .lock()
            .expect("InMemoryEngine sources mutex poisoned");
        if let Some(s) = sources.iter_mut().find(|s| s.id == id && !s.archived) {
            let now = current_utc_iso8601();
            s.archived = true;
            s.archived_at = Some(now.clone());
            // Archive expires in 72h (mirrors TS v0.26.5)
            s.archive_expires_at = Some(crate::time::add_hours(&now, 72));
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn get_page(&self, slug: &str, opts: &GetPageOpts) -> crate::Result<Option<Page>> {
        let source_id = opts.source_id.as_deref();
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .find(|p| {
                p.slug == slug
                    && source_id.is_none_or(|scope| p.source_id == scope)
                    && (opts.include_deleted || p.deleted_at.is_none())
            })
            .cloned())
    }

    async fn put_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
        input: &PageInput,
    ) -> crate::Result<Page> {
        // S6-T8 — normalise `source_id = None` to "default" to mirror TS
        // `opts?.sourceId ?? 'default'` (pglite-engine.ts:838).
        let source_id_norm = source_id.unwrap_or("default");

        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut id_guard = self
            .next_id
            .lock()
            .expect("InMemoryEngine next_id mutex poisoned");

        // S6-T8 — match on compound key `(slug, source_id)` so two sources
        // can hold independent rows under the same slug.
        if let Some(existing) = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id_norm)
        {
            existing.page_type.clone_from(&input.page_type);
            existing.title.clone_from(&input.title);
            existing.compiled_truth.clone_from(&input.compiled_truth);
            if let Some(ref pk) = input.page_kind {
                existing.page_kind = *pk;
            }
            return Ok(existing.clone());
        }

        *id_guard += 1;
        let now = current_utc_iso8601();
        let page = Page {
            id: *id_guard,
            slug: slug.to_string(),
            page_type: input.page_type.clone(),
            page_kind: input.page_kind.unwrap_or(PageKind::Markdown),
            title: input.title.clone(),
            compiled_truth: input.compiled_truth.clone(),
            timeline: input.timeline.clone().unwrap_or_default(),
            frontmatter: input
                .frontmatter
                .clone()
                .unwrap_or(Value::Object(Map::default())),
            content_hash: input.content_hash.clone(),
            emotional_weight: None,
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
            // Mirrors the input on the way in; `None` for fresh rows.
            last_retrieved_at: input.last_retrieved_at.clone(),
            effective_date: input.effective_date.clone(),
            effective_date_source: input.effective_date_source,
            import_filename: input.import_filename.clone(),
            salience_touched_at: None,
            // S5 catch-up: salience_score column added in S4 SQL; fresh rows
            // start with no score (recomputed by background salience job).
            salience_score: None,
            // PG `BIGINT DEFAULT 1` — fresh rows start at generation 1, not 0.
            generation: 1,
            embedding: input.embedding.clone(),
            // PG `INT NOT NULL DEFAULT 1` — fresh rows start at chunker v1
            // unless the caller pins an explicit version.
            chunker_version: input.chunker_version.unwrap_or(1),
            source_path: input.source_path.clone(),
            source_id: source_id_norm.to_string(),
            source_kind: input.source_kind.clone(),
            source_uri: input.source_uri.clone(),
            ingested_via: input.ingested_via.clone(),
            ingested_at: input.ingested_at.clone(),
            contextual_retrieval_mode: None,
            corpus_generation: None,
        };
        store.push(page.clone());
        Ok(page)
    }

    async fn delete_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<()> {
        let source_id = source_id.unwrap_or("default");
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // Soft delete: set deleted_at instead of removing
        for p in store.iter_mut() {
            if p.slug == slug && p.source_id == source_id {
                p.deleted_at = Some("2026-01-01T00:00:00Z".to_string());
            }
        }
        Ok(())
    }

    async fn list_pages(&self, filters: &PageFilters) -> crate::Result<Vec<Page>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let source_ids_filter = filters.source_ids.as_ref().filter(|ids| !ids.is_empty());
        let mut pages: Vec<Page> = store
            .iter()
            .filter(|p| {
                filters
                    .page_type
                    .as_deref()
                    .is_none_or(|t| p.page_type == t)
            })
            .filter(|p| {
                if let Some(ids) = source_ids_filter {
                    ids.iter().any(|id| id == &p.source_id)
                } else {
                    filters
                        .source_id
                        .as_deref()
                        .is_none_or(|source_id| p.source_id == source_id)
                }
            })
            .cloned()
            .collect();
        match filters.sort.unwrap_or_default() {
            PageSort::UpdatedDesc => pages.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)),
            PageSort::UpdatedAsc => pages.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            PageSort::CreatedDesc => pages.sort_by(|a, b| b.created_at.cmp(&a.created_at)),
            PageSort::Slug => pages.sort_by(|a, b| a.slug.cmp(&b.slug)),
        }
        if let Some(limit) = filters.limit {
            pages.truncate(limit);
        }
        Ok(pages)
    }

    async fn list_stale_pages(&self) -> crate::Result<Vec<Page>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let stale: Vec<Page> = store
            .iter()
            .filter(|p| p.deleted_at.is_none() && p.embedding.is_none())
            .cloned()
            .collect();
        Ok(stale)
    }

    async fn put_page_embedding(
        &self,
        slug: &str,
        source_id: &str,
        embedding: Vec<u8>,
    ) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        match store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id)
        {
            Some(p) => {
                p.embedding = Some(embedding);
                Ok(())
            }
            None => Err(crate::error::StructuredError::new(
                "NotFound",
                "not_found",
                &format!("page not found: {source_id}::{slug}"),
            )),
        }
    }

    async fn set_page_timeline(
        &self,
        slug: &str,
        source_id: &str,
        timeline: String,
    ) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        match store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id)
        {
            Some(p) => {
                p.timeline = timeline;
                Ok(())
            }
            None => Err(crate::error::StructuredError::new(
                "NotFound",
                "not_found",
                &format!("page not found: {source_id}::{slug}"),
            )),
        }
    }

    async fn resolve_slugs(
        &self,
        partial: &str,
        opts: &ResolveSlugsOpts,
    ) -> crate::Result<Vec<String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let source_matches =
            |source_id: &str| match opts.source_ids.as_ref().filter(|ids| !ids.is_empty()) {
                Some(ids) => ids.iter().any(|id| id == source_id),
                None => opts.source_id.as_deref().is_none_or(|id| source_id == id),
            };

        let exact: Vec<String> = store
            .iter()
            .filter(|p| p.deleted_at.is_none() && p.slug == partial && source_matches(&p.source_id))
            .map(|p| p.slug.clone())
            .collect();
        if !exact.is_empty() {
            return Ok(exact);
        }

        Ok(store
            .iter()
            .filter(|p| {
                p.deleted_at.is_none() && p.slug.contains(partial) && source_matches(&p.source_id)
            })
            .take(5)
            .map(|p| p.slug.clone())
            .collect())
    }

    async fn upsert_file(&self, spec: &FileSpec) -> crate::Result<UpsertFileResult> {
        let source_id = spec.source_id.as_deref().unwrap_or("default");
        let metadata = spec.metadata.clone().unwrap_or_else(|| json!({}));
        let mut file_store = self
            .file_store
            .lock()
            .expect("InMemoryEngine file_store mutex poisoned");

        if let Some(existing) = file_store
            .iter_mut()
            .find(|file| file.storage_path == spec.storage_path)
        {
            existing.source_id = source_id.to_string();
            existing.page_slug = spec.page_slug.clone();
            existing.page_id = spec.page_id;
            existing.filename.clone_from(&spec.filename);
            existing.mime_type = spec.mime_type.clone();
            existing.size_bytes = spec.size_bytes;
            existing.content_hash.clone_from(&spec.content_hash);
            existing.metadata = metadata;
            return Ok(UpsertFileResult {
                id: existing.id,
                created: false,
            });
        }

        let mut next_file_id = self
            .next_file_id
            .lock()
            .expect("InMemoryEngine next_file_id mutex poisoned");
        *next_file_id += 1;
        let id = *next_file_id;
        let row = FileRow {
            id,
            source_id: source_id.to_string(),
            page_slug: spec.page_slug.clone(),
            page_id: spec.page_id,
            filename: spec.filename.clone(),
            storage_path: spec.storage_path.clone(),
            mime_type: spec.mime_type.clone(),
            size_bytes: spec.size_bytes,
            content_hash: spec.content_hash.clone(),
            metadata,
            created_at: current_utc_iso8601(),
        };
        file_store.push(row);
        Ok(UpsertFileResult { id, created: true })
    }

    async fn get_file(
        &self,
        source_id: &str,
        storage_path: &str,
    ) -> crate::Result<Option<FileRow>> {
        let file_store = self
            .file_store
            .lock()
            .expect("InMemoryEngine file_store mutex poisoned");
        Ok(file_store
            .iter()
            .find(|file| file.source_id == source_id && file.storage_path == storage_path)
            .cloned())
    }

    async fn list_files_for_page(&self, page_id: u64) -> crate::Result<Vec<FileRow>> {
        let file_store = self
            .file_store
            .lock()
            .expect("InMemoryEngine file_store mutex poisoned");
        Ok(file_store
            .iter()
            .filter(|file| file.page_id == Some(page_id))
            .cloned()
            .collect())
    }

    // ── 1-6-7-5: file listing + ingestion + chunks ──────────────────────

    async fn list_files(&self, slug: Option<&str>) -> crate::Result<Vec<FileListRow>> {
        let file_store = self
            .file_store
            .lock()
            .expect("InMemoryEngine file_store mutex poisoned");
        Ok(file_store
            .iter()
            .filter(|file| slug.map_or(true, |s| file.page_slug.as_deref() == Some(s)))
            .map(|f| FileListRow {
                id: f.id as i64,
                page_slug: f.page_slug.clone(),
                filename: f.filename.clone(),
                storage_path: f.storage_path.clone(),
                mime_type: f.mime_type.clone(),
                size_bytes: f.size_bytes,
                content_hash: f.content_hash.clone(),
                created_at: f.created_at.clone(),
            })
            .collect())
    }

    async fn get_chunks(&self, slug: &str, _source_id: &str) -> crate::Result<Vec<Chunk>> {
        let store = self
            .chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");
        let chunks = store.get(slug).cloned().unwrap_or_default();
        Ok(chunks
            .into_iter()
            .map(|ci| Chunk {
                page_id: 0,
                chunk_index: ci.chunk_index as i64,
                chunk_text: ci.chunk_text,
                chunk_source: match ci.chunk_source {
                    crate::import::ChunkSource::CompiledTruth => "compiled_truth",
                    crate::import::ChunkSource::Timeline => "timeline",
                    crate::import::ChunkSource::FencedCode => "fenced_code",
                    crate::import::ChunkSource::Image => "image",
                }
                .to_string(),
                model: None,
                token_count: ci.token_count.map(|t| t as i64),
                language: ci.language,
                symbol_name: ci.symbol_name,
                symbol_type: ci.symbol_type,
                start_line: ci.start_line.map(|v| v as i64),
                end_line: ci.end_line.map(|v| v as i64),
                parent_symbol_path: if ci.parent_symbol_path.is_empty() {
                    None
                } else {
                    Some(ci.parent_symbol_path.join("/"))
                },
                doc_comment: None,
                symbol_name_qualified: ci.symbol_name_qualified,
                created_at: current_utc_iso8601(),
            })
            .collect())
    }

    async fn log_ingest(&self, input: &IngestLogInput) -> crate::Result<()> {
        let mut id_guard = self
            .next_ingest_id
            .lock()
            .expect("InMemoryEngine next_ingest_id mutex poisoned");
        *id_guard += 1;
        let entry_id = *id_guard;
        drop(id_guard);
        let entry = IngestLogEntry {
            id: entry_id as i64,
            source_id: input.source_id.clone(),
            source_type: input.source_type.clone(),
            source_ref: input.source_ref.clone(),
            pages_updated: input.pages_updated.clone(),
            summary: input.summary.clone(),
            created_at: current_utc_iso8601(),
        };
        self.ingest_log_store
            .lock()
            .expect("InMemoryEngine ingest_log_store mutex poisoned")
            .push(entry);
        Ok(())
    }

    async fn get_ingest_log(&self, limit: u32) -> crate::Result<Vec<IngestLogEntry>> {
        let store = self
            .ingest_log_store
            .lock()
            .expect("InMemoryEngine ingest_log_store mutex poisoned");
        let mut out: Vec<IngestLogEntry> = store.iter().rev().take(limit as usize).cloned().collect();
        Ok(out)
    }

    async fn get_calibration_profile(
        &self,
        holder: &str,
        source_id: Option<&str>,
        source_ids: Option<&[String]>,
    ) -> crate::Result<Option<crate::calibration_queries::CalibrationProfileRow>> {
        crate::calibration_queries::CalibrationQueries::get_latest_profile(self, holder, source_id, source_ids).await
    }

    async fn get_scorecard(
        &self,
        query: &crate::calibration_queries::ScorecardQuery<'_>,
    ) -> crate::Result<crate::calibration_queries::TakesScorecard> {
        crate::calibration_queries::CalibrationQueries::get_scorecard(self, query).await
    }

    async fn insert_calibration_profile(
        &self,
        row: &crate::calibration_queries::CalibrationProfileInsert<'_>,
    ) -> crate::Result<i64> {
        crate::calibration_queries::CalibrationQueries::insert_calibration_profile(self, row).await
    }

    async fn get_calibration_curve(
        &self,
        query: &crate::calibration_queries::CalibrationCurveQuery<'_>,
    ) -> crate::Result<Vec<crate::calibration_queries::CalibrationBucket>> {
        crate::calibration_queries::CalibrationQueries::get_calibration_curve(self, query).await
    }

    // ── undo-wave reversal bridge (1-3-3-2) ──

    async fn revert_wave_resolutions(
        &self,
        wave_version: &str,
        resolved_by: &str,
        dry_run: bool,
    ) -> crate::Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::revert_wave_resolutions(self, wave_version, resolved_by, dry_run).await
    }

    async fn unapply_wave_grade_cache(&self, wave_version: &str, dry_run: bool) -> crate::Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::unapply_wave_grade_cache(self, wave_version, dry_run).await
    }

    async fn delete_calibration_profiles_for_wave(
        &self,
        wave_version: &str,
        dry_run: bool,
    ) -> crate::Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::delete_calibration_profiles_for_wave(self, wave_version, dry_run).await
    }

    async fn purge_nudge_log_for_wave(&self, wave_version: &str, dry_run: bool) -> crate::Result<u64> {
        crate::calibration_queries::CalibrationWaveQueries::purge_nudge_log_for_wave(self, wave_version, dry_run).await
    }

    async fn find_duplicate_page(
        &self,
        source_id: &str,
        opts: &FindDuplicatePageOpts,
    ) -> crate::Result<Option<DuplicatePage>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .find(|p| {
                p.source_id == source_id
                    && p.deleted_at.is_none()
                    && (p.content_hash.as_deref() == Some(opts.content_hash.as_str())
                        || opts.frontmatter_id.as_deref().is_some_and(|id| {
                            p.frontmatter.get("id").and_then(Value::as_str) == Some(id)
                        }))
            })
            .map(|p| DuplicatePage {
                slug: p.slug.clone(),
                id: p.id,
            }))
    }

    async fn soft_delete_page(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Option<String>> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_none()
                && source_id.is_none_or(|source_id| p.source_id == source_id)
        }) else {
            return Ok(None);
        };

        page.deleted_at = Some(current_utc_iso8601());
        Ok(Some(page.slug.clone()))
    }

    async fn restore_page(&self, slug: &str, source_id: Option<&str>) -> crate::Result<bool> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_some()
                && source_id.is_none_or(|source_id| p.source_id == source_id)
        }) else {
            return Ok(false);
        };

        page.deleted_at = None;
        page.updated_at = current_utc_iso8601();
        Ok(true)
    }

    async fn purge_deleted_pages(&self, _older_than_hours: u32) -> crate::Result<PurgeResult> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut slugs = Vec::new();
        store.retain(|page| {
            if page.deleted_at.is_some() {
                slugs.push(page.slug.clone());
                false
            } else {
                true
            }
        });

        Ok(PurgeResult {
            count: slugs.len() as u64,
            slugs,
        })
    }

    async fn add_tag(&self, slug: &str, tag: &str, source_id: Option<&str>) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_none()
                && source_id.is_none_or(|sid| p.source_id == sid)
        }) else {
            return Err(Error::page_not_found(slug, source_id));
        };
        let mut tags = page_tags(page);
        tags.push(tag.to_string());
        set_page_tags(page, tags);
        Ok(())
    }

    async fn remove_tag(
        &self,
        slug: &str,
        tag: &str,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // `deleted_at IS NULL` mirrors the inner SELECT guard used in
        // postgres.rs:634 / libsql.rs:859. TS `removeTag` is silently
        // no-op on soft-deleted rows; we preserve the same shape here.
        if let Some(page) = store.iter_mut().find(|p| {
            p.slug == slug
                && p.deleted_at.is_none()
                && source_id.is_none_or(|sid| p.source_id == sid)
        }) {
            let mut tags = page_tags(page);
            tags.retain(|t| t != tag);
            set_page_tags(page, tags);
        }
        // Silent OK for missing page or absent tag — mirrors TS `removeTag`.
        Ok(())
    }

    async fn get_tags(&self, slug: &str, source_id: Option<&str>) -> crate::Result<Vec<String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // `deleted_at IS NULL` mirrors the inner SELECT guard used in
        // postgres.rs:655 / libsql.rs:878 so soft-deleted pages report
        // no tags regardless of stored state.
        Ok(store
            .iter()
            .find(|p| {
                p.slug == slug
                    && p.deleted_at.is_none()
                    && source_id.is_none_or(|sid| p.source_id == sid)
            })
            .map(page_tags)
            .unwrap_or_default())
    }

    async fn search_pages(&self, opts: &SearchOpts) -> crate::Result<Vec<SearchResult>> {
        // Backend-specific half: materialize the live (non-deleted),
        // optionally source-scoped candidate pages, then hand them to the
        // shared `fuse_and_boost` core. Cloning to an owned Vec here drops the
        // non-Send store guard before the async boost reads await — a guard
        // held across an await would make this future non-Send.
        let candidates: Vec<Page> = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            store
                .iter()
                .filter(|page| {
                    page.deleted_at.is_none()
                        && opts
                            .source_id
                            .as_ref()
                            .is_none_or(|sid| page.source_id == *sid)
                })
                .cloned()
                .collect()
        }; // store lock dropped here

        fuse_and_boost(self, &candidates, opts).await
    }

    async fn search_pages_by_embedding(
        &self,
        query_embedding: &[f32],
        limit: usize,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<Page>> {
        let store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let chunk_store = self.chunk_store.lock().expect("InMemoryEngine chunk_store mutex poisoned");

        // Score each live page by its best chunk embedding similarity.
        let mut scored: Vec<(Page, f64)> = Vec::new();
        for page in store.iter() {
            if page.deleted_at.is_some() {
                continue;
            }
            if let Some(sid) = source_id {
                if page.source_id != sid {
                    continue;
                }
            }
            let best_score = chunk_store
                .get(&page.slug)
                .map(|chunks| {
                    chunks
                        .iter()
                        .filter_map(|c| c.embedding.as_deref())
                        .map(|emb| cosine_similarity(query_embedding, emb))
                        .fold(0.0_f64, f64::max)
                })
                .unwrap_or(0.0);
            scored.push((page.clone(), best_score));
        } // store/chunk_store locks dropped

        // Sort descending by similarity score.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Truncate to top-N.
        scored.truncate(limit.min(scored.len()));

        Ok(scored.into_iter().map(|(p, _)| p).collect())
    }

    async fn image_search_daily_spend_cents(&self, client_id: &str) -> crate::Result<i64> {
        let today_prefix = &crate::time::current_utc_iso8601()[..10];
        let store = self
            .image_search_spend_store
            .lock()
            .expect("InMemoryEngine image_search_spend_store mutex poisoned");
        Ok(store
            .iter()
            .filter(|r| r.client_id == client_id && r.created_at.starts_with(today_prefix))
            .map(|r| r.amount_cents)
            .sum())
    }

    async fn record_image_search_spend(
        &self,
        client_id: &str,
        amount_cents: i64,
        provider: &str,
        model: &str,
    ) -> crate::Result<()> {
        let mut store = self
            .image_search_spend_store
            .lock()
            .expect("InMemoryEngine image_search_spend_store mutex poisoned");
        store.push(InternalImageSearchSpend {
            client_id: client_id.to_string(),
            amount_cents,
            provider: provider.to_string(),
            model: model.to_string(),
            created_at: crate::time::current_utc_iso8601(),
        });
        Ok(())
    }

    async fn refresh_page_body(&self, args: &RefreshPageBodyArgs) -> crate::Result<()> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        if let Some(page) = store.iter_mut().find(|p| {
            p.slug == args.slug && p.source_id == args.source_id && p.deleted_at.is_none()
        }) {
            page.compiled_truth.clone_from(&args.compiled_truth);
            page.timeline = args.timeline.to_string();
            page.content_hash = Some(args.content_hash.clone());
            page.updated_at = current_utc_iso8601();
        }
        // Silent Ok for missing or soft-deleted rows — mirrors
        // postgres.rs:689 / libsql.rs:899 WHERE deleted_at IS NULL.
        Ok(())
    }

    async fn update_page_contextual_retrieval_state(
        &self,
        slug: &str,
        source_id: &str,
        mode: &str,
        corpus_generation: Option<&str>,
    ) -> crate::Result<()> {
        let cr_mode = match mode {
            "none" => CRMode::None,
            "title" => CRMode::Title,
            "per_chunk_synopsis" => CRMode::PerChunkSynopsis,
            other => {
                return Err(Error::engine(format!(
                    "invalid contextual_retrieval_mode: {other}"
                )))
            }
        };
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        if let Some(page) = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id && p.deleted_at.is_none())
        {
            page.contextual_retrieval_mode = Some(cr_mode);
            page.corpus_generation = corpus_generation.map(std::string::ToString::to_string);
            page.updated_at = current_utc_iso8601();
        }
        // Silent Ok for missing or soft-deleted rows — mirrors
        // postgres.rs:718 / libsql.rs:926 WHERE deleted_at IS NULL.
        Ok(())
    }

    // ─── Raw data / versions / slug rewrite (7) ─────────────────────────────
    // Slice #20: full InMemory behavior replacing the #19 unimplemented!() stubs.

    async fn put_raw_data(
        &self,
        slug: &str,
        source: &str,
        data: &Value,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        // Resolve the page_id for this (slug, source_id).
        let page_id = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            let source_id_norm = source_id.unwrap_or("default");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm)
                .map(|p| p.id)
            // Silent no-op for missing pages mirrors TS putRawData (no
            // explicit page existence check in the TS impl).
        };
        let Some(page_id) = page_id else {
            // Page not found — return an error to surface mis-wired callers.
            return Err(Error::page_not_found(slug, source_id));
        };
        let mut rd_store = self
            .raw_data_store
            .lock()
            .expect("InMemoryEngine raw_data_store mutex poisoned");
        if let Some(existing) = rd_store
            .iter_mut()
            .find(|r| r.page_id == page_id && r.source == source)
        {
            // Upsert: overwrite data + refresh fetched_at.
            existing.data = data.clone();
            existing.fetched_at = current_utc_iso8601();
        } else {
            rd_store.push(InternalRawData {
                page_id,
                source: source.to_string(),
                data: data.clone(),
                fetched_at: current_utc_iso8601(),
            });
        }
        Ok(())
    }

    async fn get_raw_data(
        &self,
        slug: &str,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<RawData>> {
        // Resolve page_id.
        let page_id = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            let source_id_norm = source_id.unwrap_or("default");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm)
                .map(|p| p.id)
        };
        let Some(page_id) = page_id else {
            // Missing page → empty result (mirrors TS getRawData returning []).
            return Ok(vec![]);
        };
        let rd_store = self
            .raw_data_store
            .lock()
            .expect("InMemoryEngine raw_data_store mutex poisoned");
        Ok(rd_store
            .iter()
            .filter(|r| r.page_id == page_id && source.is_none_or(|s| r.source == s))
            .map(|r| RawData {
                source: r.source.clone(),
                data: r.data.clone(),
                fetched_at: r.fetched_at.clone(),
            })
            .collect())
    }

    async fn create_version(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<PageVersion> {
        let source_id_norm = source_id.unwrap_or("default");
        // Snapshot the current page state.
        let (page_id, compiled_truth, frontmatter) = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm && p.deleted_at.is_none())
                .map(|p| (p.id, p.compiled_truth.clone(), p.frontmatter.clone()))
                .ok_or_else(|| Error::page_not_found(slug, source_id))?
        };
        let mut vid_guard = self
            .next_version_id
            .lock()
            .expect("InMemoryEngine next_version_id mutex poisoned");
        *vid_guard += 1;
        let version = PageVersion {
            id: *vid_guard,
            page_id,
            compiled_truth,
            frontmatter,
            snapshot_at: current_utc_iso8601(),
        };
        self.version_store
            .lock()
            .expect("InMemoryEngine version_store mutex poisoned")
            .push(version.clone());
        Ok(version)
    }

    async fn get_versions(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<PageVersion>> {
        let source_id_norm = source_id.unwrap_or("default");
        let page_id = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            store
                .iter()
                .find(|p| p.slug == slug && p.source_id == source_id_norm)
                .map(|p| p.id)
        };
        let Some(page_id) = page_id else {
            return Ok(vec![]);
        };
        let vs = self
            .version_store
            .lock()
            .expect("InMemoryEngine version_store mutex poisoned");
        // Newest-first: sort by id DESC (InMemory ids are monotonically
        // increasing, so id order == insertion order == snapshot_at order).
        let mut versions: Vec<PageVersion> = vs
            .iter()
            .filter(|v| v.page_id == page_id)
            .cloned()
            .collect();
        versions.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(versions)
    }

    async fn revert_to_version(
        &self,
        slug: &str,
        version_id: u64,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        let source_id_norm = source_id.unwrap_or("default");
        // Find the version snapshot.
        let (compiled_truth, frontmatter) = {
            let vs = self
                .version_store
                .lock()
                .expect("InMemoryEngine version_store mutex poisoned");
            vs.iter()
                .find(|v| v.id == version_id)
                .map(|v| (v.compiled_truth.clone(), v.frontmatter.clone()))
                .ok_or_else(|| {
                    Error::engine(format!("version {version_id} not found for page '{slug}'"))
                })?
        };
        // Apply the snapshot to the live page.
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let page = store
            .iter_mut()
            .find(|p| p.slug == slug && p.source_id == source_id_norm && p.deleted_at.is_none())
            .ok_or_else(|| Error::page_not_found(slug, source_id))?;
        page.compiled_truth = compiled_truth;
        page.frontmatter = frontmatter;
        page.updated_at = current_utc_iso8601();
        Ok(())
    }

    async fn update_slug(
        &self,
        old_slug: &str,
        new_slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<()> {
        let source_id_norm = source_id.unwrap_or("default");
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        // Conflict check: new_slug must not already exist in this source.
        if store
            .iter()
            .any(|p| p.slug == new_slug && p.source_id == source_id_norm)
        {
            return Err(Error::engine(format!(
                "slug '{new_slug}' already exists in source '{source_id_norm}'"
            )));
        }
        let page = store
            .iter_mut()
            .find(|p| p.slug == old_slug && p.source_id == source_id_norm)
            .ok_or_else(|| Error::page_not_found(old_slug, source_id))?;
        page.slug = new_slug.to_string();
        page.updated_at = current_utc_iso8601();
        Ok(())
    }

    /// Explicit no-op — `InMemoryEngine` uses integer `page_id` foreign keys
    /// so there are no embedded slug strings to rewrite. Returns `Ok(())`.
    async fn rewrite_links(&self, _old_slug: &str, _new_slug: &str) -> crate::Result<()> {
        Ok(())
    }

    // ─── Advanced-read overrides (C1 Task 4) ────────────────────────────────
    // Six read-only methods promoted from trait-default `Unsupported` to real
    // InMemory implementations, mirroring postgres.rs:757-983 semantics.

    /// `get_all_slugs` — does NOT filter `deleted_at` (mirrors postgres.rs:759).
    async fn get_all_slugs(
        &self,
        source_id: Option<&str>,
    ) -> crate::Result<std::collections::HashSet<String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        Ok(store
            .iter()
            .filter(|p| source_id.is_none_or(|sid| p.source_id == sid))
            .map(|p| p.slug.clone())
            .collect())
    }

    /// `list_all_page_refs` — live pages only, ordered by `(source_id, slug)`.
    async fn list_all_page_refs(&self) -> crate::Result<Vec<PageRef>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut refs: Vec<PageRef> = store
            .iter()
            .filter(|p| p.deleted_at.is_none())
            .map(|p| PageRef {
                slug: p.slug.clone(),
                source_id: p.source_id.clone(),
            })
            .collect();
        refs.sort_by(|a, b| {
            a.source_id
                .cmp(&b.source_id)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        Ok(refs)
    }

    /// `find_orphan_pages` — no links table in `InMemory`, so ALL live pages are
    /// orphans. `title` uses `COALESCE(title, slug)` (slug when title empty).
    /// `domain` is extracted from `frontmatter["domain"]` as a string.
    /// Ordered by slug ASC.
    async fn find_orphan_pages(&self) -> crate::Result<Vec<OrphanPage>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut orphans: Vec<OrphanPage> = store
            .iter()
            .filter(|p| p.deleted_at.is_none())
            .map(|p| {
                let title = if p.title.is_empty() {
                    p.slug.clone()
                } else {
                    p.title.clone()
                };
                let domain = p
                    .frontmatter
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(std::string::ToString::to_string);
                OrphanPage {
                    slug: p.slug.clone(),
                    title,
                    domain,
                }
            })
            .collect();
        orphans.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(orphans)
    }

    /// `find_anomalies` — InMemory computes the densified baseline + target-day
    /// snapshot in Rust (no SQL). Mirrors the TS `findAnomalies` algorithm:
    /// zero-fill every window day per cohort so rare cohorts don't get
    /// sparse-day-biased baselines.
    async fn find_anomalies(
        &self,
        opts: crate::anomaly::AnomaliesOpts,
    ) -> crate::Result<Vec<crate::anomaly::AnomalyResult>> {
        use crate::anomaly::{
            compute_anomalies_from_buckets, resolve_anomaly_windows, CohortDayRow,
            CohortKind, CohortTodayRow,
        };
        use std::collections::{HashMap, HashSet};

        let (_baseline_from, baseline_to, _today_from, today_to, window_days, sigma, limit) =
            resolve_anomaly_windows(&opts)?;
        let baseline_from_day = window_days.first().map(String::as_str).unwrap_or("");
        let today_day = baseline_to.get(..10).unwrap_or("");
        let today_end_day = today_to.get(..10).unwrap_or("");

        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");

        // ((kind, value), day) -> distinct slugs touched that day.
        let mut baseline_cells: HashMap<((CohortKind, String), String), HashSet<String>> =
            HashMap::new();
        // (kind, value) -> distinct slugs touched on the target day.
        let mut today_cells: HashMap<(CohortKind, String), HashSet<String>> = HashMap::new();

        for page in store.iter() {
            if page.deleted_at.is_some() {
                continue;
            }
            let day = page.updated_at.get(..10).unwrap_or("");
            let in_baseline = day >= baseline_from_day && day < today_day;
            let is_today = day >= today_day && day < today_end_day;
            if !in_baseline && !is_today {
                continue;
            }
            let slug = page.slug.clone();
            let cohorts: Vec<(CohortKind, String)> = std::iter::once((CohortKind::Type, page.page_type.clone()))
                .chain(page_tags(page).into_iter().map(|t| (CohortKind::Tag, t)))
                .collect();
            for (kind, value) in cohorts {
                if in_baseline {
                    baseline_cells
                        .entry(((kind, value.clone()), day.to_string()))
                        .or_default()
                        .insert(slug.clone());
                }
                if is_today {
                    today_cells.entry((kind, value)).or_default().insert(slug.clone());
                }
            }
        }

        // Densify: for each cohort present in baseline, emit a CohortDayRow for
        // every window day (zero-filled where inactive).
        let mut seen_cohorts: HashSet<(CohortKind, String)> = HashSet::new();
        for ((kind, value), _) in baseline_cells.keys() {
            seen_cohorts.insert((kind.clone(), value.clone()));
        }
        let mut baseline: Vec<CohortDayRow> = Vec::new();
        for (kind, value) in seen_cohorts {
            for d in &window_days {
                let count = baseline_cells
                    .get(&((kind.clone(), value.clone()), d.clone()))
                    .map(HashSet::len)
                    .unwrap_or(0);
                baseline.push(CohortDayRow {
                    cohort_kind: kind,
                    cohort_value: value.clone(),
                    day: d.clone(),
                    count: count as i64,
                });
            }
        }

        let today: Vec<CohortTodayRow> = today_cells
            .into_iter()
            .map(|((kind, value), slugs)| CohortTodayRow {
                cohort_kind: kind,
                cohort_value: value,
                count: slugs.len() as i64,
                page_slugs: slugs.into_iter().collect(),
            })
            .collect();

        Ok(compute_anomalies_from_buckets(&baseline, &today, sigma, limit))
    }

    /// `get_page_timestamps` — key = slug, value = `COALESCE(updated_at,
    /// created_at)`. Mirrors TS deleted-row visibility; missing slugs omitted.
    async fn get_page_timestamps(
        &self,
        slugs: &[String],
    ) -> crate::Result<std::collections::HashMap<String, String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut out = std::collections::HashMap::new();
        for p in store.iter().filter(|p| slugs.iter().any(|s| s == &p.slug)) {
            let ts = if p.updated_at.is_empty() {
                p.created_at.clone()
            } else {
                p.updated_at.clone()
            };
            out.insert(p.slug.clone(), ts);
        }
        Ok(out)
    }

    /// `get_effective_dates` — key = `"{source_id}::{slug}"`, value =
    /// `COALESCE(effective_date, updated_at, created_at)`.
    async fn get_effective_dates(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, String>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let mut out = std::collections::HashMap::new();
        for r in refs {
            if let Some(p) = store
                .iter()
                .find(|p| p.slug == r.slug && p.source_id == r.source_id && p.deleted_at.is_none())
            {
                let ts = p.effective_date.clone().unwrap_or_else(|| {
                    if p.updated_at.is_empty() {
                        p.created_at.clone()
                    } else {
                        p.updated_at.clone()
                    }
                });
                out.insert(format!("{}::{}", r.source_id, r.slug), ts);
            }
        }
        Ok(out)
    }

    /// `get_salience_scores` — key = `"{source_id}::{slug}"`, score =
    /// `emotional_weight.unwrap_or(0.0) * 5.0 + ln(1 + distinct_active_take_count)`.
    async fn get_salience_scores(
        &self,
        refs: &[PageRef],
    ) -> crate::Result<std::collections::HashMap<String, f64>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let takes = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let mut out = std::collections::HashMap::new();
        for r in refs {
            if let Some(p) = store
                .iter()
                .find(|p| p.slug == r.slug && p.source_id == r.source_id && p.deleted_at.is_none())
            {
                let base = p.emotional_weight.unwrap_or(0.0) * 5.0;
                let distinct_takes = takes.iter().filter(|t| t.page_id == p.id && t.active).count() as f64;
                let value = base + (1.0 + distinct_takes).ln();
                out.insert(format!("{}::{}", r.source_id, r.slug), value);
            }
        }
        Ok(out)
    }

    // --- Phase 7C 1-3-2: Salience ---

    async fn touch_salience(&self, slug: &str, source_id: &str) -> crate::Result<bool> {
        let mut store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(page) = store.iter_mut().find(|p| p.slug == slug && p.source_id == source_id) {
            page.salience_touched_at = Some(now);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn get_recent_salience(
        &self,
        days: u32,
        limit: u32,
        slug_prefix: Option<&str>,
    ) -> crate::Result<Vec<crate::types::SalienceResult>> {
        let store = self
            .store
            .lock()
            .expect("InMemoryEngine store mutex poisoned");
        let takes = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let now = chrono::Utc::now();
        let boundary = now - chrono::Duration::days(days as i64);
        let boundary_str = boundary.to_rfc3339();

        let effective_date = |p: &Page| -> Option<chrono::DateTime<chrono::Utc>> {
            p.salience_touched_at
                .as_deref()
                .or(Some(&p.updated_at))
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc)))
        };

        let limit = limit.min(100);

        let mut results: Vec<crate::types::SalienceResult> = store
            .iter()
            .filter(|p| p.deleted_at.is_none())
            .filter(|p| slug_prefix.map_or(true, |pfx| p.slug.starts_with(pfx)))
            .filter(|p| {
                effective_date(p)
                    .map(|dt| dt >= boundary)
                    .unwrap_or(false)
            })
            .map(|p| {
                let take_count = takes.iter().filter(|t| t.page_id == p.id && t.active).count() as u32;
                let take_avg_weight = if take_count > 0 {
                    let sum: f64 = takes.iter()
                        .filter(|t| t.page_id == p.id && t.active)
                        .map(|t| t.weight)
                        .sum();
                    sum / take_count as f64
                } else {
                    0.0
                };
                let ew = p.emotional_weight.unwrap_or(0.0);
                let days_old = effective_date(p)
                    .map(|dt| {
                        let dur = now.signed_duration_since(dt);
                        dur.num_milliseconds() as f64 / (86400.0 * 1000.0)
                    })
                    .unwrap_or(0.0);
                let recency_decay = 1.0 / (1.0 + days_old.max(0.0));
                let score = ew * 5.0 + (1.0 + take_count as f64).ln() + recency_decay;

                crate::types::SalienceResult {
                    slug: p.slug.clone(),
                    source_id: p.source_id.clone(),
                    title: p.title.clone(),
                    page_type: p.page_type.clone(),
                    updated_at: p.updated_at.clone(),
                    emotional_weight: ew,
                    take_count,
                    take_avg_weight,
                    score,
                }
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit as usize);
        Ok(results)
    }

    // --- Phase 7A: Takes ---

    async fn get_takes_for_page(
        &self,
        page_id: u64,
        takes_holders_allow_list: Option<Vec<String>>,
    ) -> crate::Result<Vec<Take>> {
        let store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let mut takes: Vec<_> = store
            .iter()
            .filter(|t| t.page_id == page_id)
            .filter(|t| holder_allowed(&t.holder, &takes_holders_allow_list))
            .cloned()
            .collect();
        takes.sort_by_key(|t| t.row_num);
        Ok(takes)
    }

    async fn list_takes(&self, opts: &TakesListOpts) -> crate::Result<Vec<Take>> {
        let store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let mut takes: Vec<Take> = store
            .iter()
            .filter(|t| opts.page_id.map(|pid| t.page_id == pid).unwrap_or(true))
            .filter(|t| opts.holder.as_ref().map(|h| &t.holder == h).unwrap_or(true))
            .filter(|t| opts.kind.as_ref().map(|k| &t.kind == k).unwrap_or(true))
            .filter(|t| opts.active.map(|a| t.active == a).unwrap_or(true))
            .filter(|t| {
                opts.resolved
                    .map(|r| r == t.resolved_at.is_some())
                    .unwrap_or(true)
            })
            .filter(|t| holder_allowed(&t.holder, &opts.takes_holders_allow_list))
            .cloned()
            .collect();
        takes.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));
        let offset = opts.offset.unwrap_or(0) as usize;
        let limit = opts.limit.unwrap_or(100) as usize;
        takes.truncate(offset + limit);
        takes.drain(..offset.min(takes.len()));
        Ok(takes)
    }

    async fn search_takes(&self, query: &str, opts: &SearchTakesOpts) -> crate::Result<Vec<TakeHit>> {
        let store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let q = query.to_lowercase();
        let mut hits: Vec<(f64, Take)> = store
            .iter()
            .filter(|t| t.active)
            .filter(|t| holder_allowed(&t.holder, &opts.takes_holders_allow_list))
            .filter(|t| t.claim.to_lowercase().contains(&q))
            .map(|t| {
                // Lightweight relevance: more query-term coverage => higher score.
                let score = if q.is_empty() {
                    0.0
                } else {
                    let hits = t.claim.to_lowercase().matches(&q).count() as f64;
                    hits * (1.0 + t.weight)
                };
                (score, t.clone())
            })
            .collect();
        hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let limit = opts.limit.unwrap_or(30) as usize;
        Ok(hits
            .into_iter()
            .take(limit)
            .map(|(score, t)| TakeHit {
                take_id: t.id,
                page_id: t.page_id,
                page_slug: String::new(),
                row_num: t.row_num,
                claim: t.claim,
                kind: t.kind,
                holder: t.holder,
                weight: t.weight,
                score,
            })
            .collect())
    }

    async fn add_takes_batch(
        &self,
        page_id: u64,
        takes: &[TakeInput],
    ) -> crate::Result<UpsertTakesResult> {
        let mut store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let mut next_id = self
            .next_take_id
            .lock()
            .expect("InMemoryEngine next_take_id mutex poisoned");
        let upserted = takes.len();
        let mut weight_clamped = 0usize;

        let now = crate::time::current_utc_iso8601();
        for input in takes {
            let weight = input.weight.clamp(0.0, 1.0);
            if (weight - input.weight).abs() > f64::EPSILON {
                weight_clamped += 1;
            }
            let id = *next_id;
            *next_id += 1;
            store.push(Take {
                id,
                page_id,
                row_num: input.row_num.unwrap_or(0),
                claim: input.claim.clone(),
                kind: input.kind.clone(),
                holder: input.holder.clone(),
                weight,
                since_date: input.since_date.clone(),
                until_date: input.until_date.clone(),
                source: input.source.clone(),
                superseded_by: input.superseded_by,
                active: input.active.unwrap_or(true),
                resolved_at: None,
                resolved_quality: None,
                resolved_outcome: None,
                resolved_evidence: None,
                resolved_value: None,
                resolved_unit: None,
                resolved_by: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            });
        }

        Ok(UpsertTakesResult {
            upserted,
            weight_clamped,
        })
    }

    async fn batch_load_emotional_inputs(
        &self,
        slugs: Option<&[String]>,
    ) -> crate::Result<Vec<EmotionalInput>> {
        let store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let takes_store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");

        let slug_set: Option<std::collections::HashSet<&String>> =
            slugs.map(|s| s.iter().collect());

        let mut out: Vec<EmotionalInput> = Vec::new();
        for page in store.iter() {
            if page.deleted_at.is_some() {
                continue;
            }
            if let Some(set) = &slug_set {
                if !set.contains(&page.slug) {
                    continue;
                }
            }
            let tags = page_tags(page);
            let takes: Vec<EmotionalWeightTake> = takes_store
                .iter()
                .filter(|t| t.page_id == page.id && t.active)
                .map(|t| EmotionalWeightTake {
                    holder: t.holder.clone(),
                    weight: t.weight,
                    kind: t.kind.clone(),
                    active: t.active,
                })
                .collect();
            out.push(EmotionalInput {
                slug: page.slug.clone(),
                source_id: page.source_id.clone(),
                tags,
                takes,
            });
        }
        Ok(out)
    }

    async fn set_emotional_weight_batch(&self, writes: &[EmotionalWeightWrite]) -> crate::Result<u64> {
        let mut store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let now = crate::time::current_utc_iso8601();
        let mut updated: u64 = 0;
        for w in writes {
            if let Some(page) = store.iter_mut().find(|p| {
                p.slug == w.slug && p.source_id == w.source_id && p.deleted_at.is_none()
            }) {
                page.emotional_weight = Some(w.emotional_weight);
                page.salience_touched_at = Some(now.clone());
                updated += 1;
            }
        }
        Ok(updated)
    }

    async fn take_proposal_exists(
        &self,
        source_id: &str,
        page_slug: &str,
        content_hash: &str,
        prompt_version: &str,
    ) -> crate::Result<bool> {
        let store = self
            .take_proposals_store
            .lock()
            .expect("InMemoryEngine take_proposals_store mutex poisoned");
        Ok(store.iter().any(|p| {
            p.source_id == source_id
                && p.page_slug == page_slug
                && p.content_hash == content_hash
                && p.prompt_version == prompt_version
        }))
    }

    async fn add_take_proposal(&self, proposal: &TakeProposalInput) -> crate::Result<u64> {
        let mut store = self
            .take_proposals_store
            .lock()
            .expect("InMemoryEngine take_proposals_store mutex poisoned");
        let mut next_id = self
            .next_take_proposal_id
            .lock()
            .expect("InMemoryEngine next_take_proposal_id mutex poisoned");
        let id = *next_id;
        *next_id += 1;
        store.push(InternalTakeProposal {
            id,
            source_id: proposal.source_id.clone(),
            page_slug: proposal.page_slug.clone(),
            content_hash: proposal.content_hash.clone(),
            prompt_version: proposal.prompt_version.clone(),
            proposal_run_id: proposal.proposal_run_id.clone(),
            claim_text: proposal.claim_text.clone(),
            kind: proposal.kind.clone(),
            holder: proposal.holder.clone(),
            weight: proposal.weight,
            domain: proposal.domain.clone(),
            dedup_against_fence_rows: proposal.dedup_against_fence_rows.clone(),
            model_id: proposal.model_id.clone(),
            status: "pending".to_string(),
        });
        Ok(id)
    }

    async fn take_grade_cache_exists(
        &self,
        take_id: u64,
        prompt_version: &str,
        judge_model_id: &str,
        evidence_signature: &str,
    ) -> crate::Result<bool> {
        let store = self
            .take_grade_cache_store
            .lock()
            .expect("InMemoryEngine take_grade_cache_store mutex poisoned");
        Ok(store.iter().any(|c| {
            c.take_id == take_id
                && c.prompt_version == prompt_version
                && c.judge_model_id == judge_model_id
                && c.evidence_signature == evidence_signature
        }))
    }

    async fn add_take_grade_cache(&self, entry: &TakeGradeCacheInput) -> crate::Result<u64> {
        let mut store = self
            .take_grade_cache_store
            .lock()
            .expect("InMemoryEngine take_grade_cache_store mutex poisoned");
        // Conflict-safe: never double-count a verdict already in the cache.
        let exists = store.iter().any(|c| {
            c.take_id == entry.take_id
                && c.prompt_version == entry.prompt_version
                && c.judge_model_id == entry.judge_model_id
                && c.evidence_signature == entry.evidence_signature
        });
        if exists {
            return Ok(0);
        }
        store.push(InternalTakeGradeCache {
            take_id: entry.take_id,
            prompt_version: entry.prompt_version.clone(),
            judge_model_id: entry.judge_model_id.clone(),
            evidence_signature: entry.evidence_signature.clone(),
            wave_version: entry.wave_version.clone(),
            verdict: entry.verdict.clone(),
            confidence: entry.confidence,
            applied: entry.applied,
            cost_usd: entry.cost_usd,
        });
        Ok(1)
    }

    async fn get_dream_verdict(
        &self,
        file_path: &str,
        content_hash: &str,
    ) -> crate::Result<Option<DreamVerdict>> {
        let store = self
            .dream_verdicts_store
            .lock()
            .expect("InMemoryEngine dream_verdicts_store mutex poisoned");
        Ok(store
            .iter()
            .find(|v| v.file_path == file_path && v.content_hash == content_hash)
            .map(|v| DreamVerdict {
                worth_processing: v.worth_processing,
                reasons: v.reasons.clone(),
                judged_at: v.judged_at.clone(),
            }))
    }

    async fn put_dream_verdict(
        &self,
        file_path: &str,
        content_hash: &str,
        verdict: &DreamVerdictInput,
    ) -> crate::Result<()> {
        let mut store = self
            .dream_verdicts_store
            .lock()
            .expect("InMemoryEngine dream_verdicts_store mutex poisoned");
        let judged_at = chrono::Utc::now().to_rfc3339();
        if let Some(existing) = store
            .iter_mut()
            .find(|v| v.file_path == file_path && v.content_hash == content_hash)
        {
            existing.worth_processing = verdict.worth_processing;
            existing.reasons = verdict.reasons.clone();
            existing.judged_at = judged_at;
        } else {
            store.push(InternalDreamVerdict {
                file_path: file_path.to_string(),
                content_hash: content_hash.to_string(),
                worth_processing: verdict.worth_processing,
                reasons: verdict.reasons.clone(),
                judged_at,
            });
        }
        Ok(())
    }

    // ---- engine config store (1-3-4-6) ----

    async fn get_config(&self, key: &str) -> crate::Result<Option<String>> {
        let store = self
            .config_store
            .lock()
            .expect("InMemoryEngine config_store mutex poisoned");
        Ok(store.get(key).cloned())
    }

    async fn set_config(&self, key: &str, value: &str) -> crate::Result<()> {
        let mut store = self
            .config_store
            .lock()
            .expect("InMemoryEngine config_store mutex poisoned");
        store.insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn unset_config(&self, key: &str) -> crate::Result<u64> {
        let mut store = self
            .config_store
            .lock()
            .expect("InMemoryEngine config_store mutex poisoned");
        Ok(if store.remove(key).is_some() { 1 } else { 0 })
    }

    async fn collect_child_put_page_slugs(
        &self,
        child_ids: &[i64],
    ) -> crate::Result<Vec<(String, String)>> {
        let store = self
            .subagent_tool_executions_store
            .lock()
            .expect("InMemoryEngine subagent_tool_executions_store mutex poisoned");
        let ids: Vec<i64> = child_ids.to_vec();
        let mut out: Vec<(String, String)> = Vec::new();
        for row in store.iter() {
            if !ids.contains(&row.job_id) {
                continue;
            }
            if row.tool_name != "brain_put_page" || row.status != "complete" {
                continue;
            }
            let slug = row
                .input
                .get("slug")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    row.input
                        .get("input")
                        .and_then(|i| i.get("slug"))
                        .and_then(serde_json::Value::as_str)
                });
            if let Some(slug) = slug {
                if !slug.is_empty() {
                    out.push((slug.to_string(), "default".to_string()));
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    async fn load_op_checkpoint(
        &self,
        op: &str,
        fingerprint: &str,
    ) -> crate::Result<Vec<String>> {
        let store = self.op_checkpoints_store.lock().expect("poisoned");
        Ok(store
            .get(&(op.to_string(), fingerprint.to_string()))
            .cloned()
            .unwrap_or_default())
    }

    async fn save_op_checkpoint(
        &self,
        op: &str,
        fingerprint: &str,
        completed_keys: &[String],
    ) -> crate::Result<()> {
        let mut store = self.op_checkpoints_store.lock().expect("poisoned");
        store.insert(
            (op.to_string(), fingerprint.to_string()),
            completed_keys.to_vec(),
        );
        Ok(())
    }

    async fn clear_op_checkpoint(&self, op: &str, fingerprint: &str) -> crate::Result<()> {
        let mut store = self.op_checkpoints_store.lock().expect("poisoned");
        store.remove(&(op.to_string(), fingerprint.to_string()));
        Ok(())
    }

    async fn peek_fact_row_num_start(&self, source_id: &str, slug: &str) -> crate::Result<i64> {
        let store = self.facts_store.lock().expect("poisoned");
        let max = store
            .iter()
            .filter(|f| f.source_id == source_id && f.source_markdown_slug.as_deref() == Some(slug))
            .filter_map(|f| f.row_num)
            .max();
        Ok(max.map(|m| m as i64 + 1).unwrap_or(0))
    }

    async fn resolve_take(
        &self,
        page_id: u64,
        row_num: i32,
        resolution: &TakeResolution,
    ) -> crate::Result<()> {
        let mut store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let now = crate::time::current_utc_iso8601();
        // Existence check first, matching canonical TS ordering
        // (TAKE_ROW_NOT_FOUND is thrown before deriveResolutionTuple).
        let Some(take) = store
            .iter_mut()
            .find(|t| t.page_id == page_id && t.row_num == row_num)
        else {
            return Err(crate::error::StructuredError::new(
                "Not Found",
                "not_found",
                format!("no take found for page_id={page_id} row_num={row_num}"),
            ));
        };
        // Derive the canonical (resolved_quality, resolved_outcome) tuple only
        // after confirming the take exists — errors out on invalid/contradictory
        // input, matching canonical TS `deriveResolutionTuple`.
        let (resolved_quality, resolved_outcome) = resolution.derive_quality_outcome()?;
        take.resolved_at = Some(now.clone());
        take.resolved_quality = Some(resolved_quality);
        take.resolved_outcome = resolved_outcome;
        take.resolved_evidence = resolution.evidence.clone();
        take.resolved_value = resolution.value;
        take.resolved_unit = resolution.unit.clone();
        take.resolved_by = resolution.by.clone();
        take.updated_at = now.clone();
        Ok(())
    }

    // ── Links (Phase 7B) ──────────────────────────────────────────────────

    async fn add_links_batch(
        &self,
        links: &[LinkBatchInput],
    ) -> crate::Result<usize> {
        let store = self.store.lock().expect("poisoned");
        let mut links_store = self.links_store.lock().expect("poisoned");

        let mut inserted = 0usize;
        for input in links {
            // Resolve slugs to page IDs; skip if either doesn't exist.
            let from_page = store.iter().find(|p| p.slug == input.from_slug);
            let to_page = store.iter().find(|p| p.slug == input.to_slug);
            let (from_page_id, to_page_id) = match (from_page, to_page) {
                (Some(f), Some(t)) => (f.id, t.id),
                _ => continue,
            };

            let origin_page_id = input.origin_slug.as_ref().and_then(|s| {
                store.iter().find(|p| p.slug == *s).map(|p| p.id)
            });

            // Normalize None → "markdown" for duplicate detection,
            // matching the storage default in the insert branch below.
            let normalized_link_source = input.link_source.as_deref().unwrap_or("markdown");
            let existing = links_store.iter().any(|l| {
                l.from_page_id == from_page_id
                    && l.to_page_id == to_page_id
                    && l.link_type == input.link_type.as_deref().unwrap_or("")
                    && l.link_source.as_deref() == Some(normalized_link_source)
                    && l.origin_page_id == origin_page_id
            });

            if !existing {
                links_store.push(InternalLink {
                    from_page_id,
                    to_page_id,
                    link_type: input.link_type.clone().unwrap_or_default(),
                    context: input.context.clone().unwrap_or_default(),
                    link_source: input.link_source.clone().or(Some("markdown".into())),
                    origin_page_id,
                    origin_field: input.origin_field.clone(),
                });
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    async fn remove_link(
        &self,
        from: &str,
        to: &str,
        link_type: Option<&str>,
        link_source: Option<&str>,
        from_source_id: Option<&str>,
        to_source_id: Option<&str>,
    ) -> crate::Result<()> {
        let store = self.store.lock().expect("poisoned");
        let mut links_store = self.links_store.lock().expect("poisoned");

        // Find matching page IDs
        let from_page = store.iter().find(|p| {
            p.slug == from && (from_source_id.is_none() || Some(p.source_id.as_str()) == from_source_id)
        });
        let to_page = store.iter().find(|p| {
            p.slug == to && (to_source_id.is_none() || Some(p.source_id.as_str()) == to_source_id)
        });

        let (from_id, to_id) = match (from_page, to_page) {
            (Some(f), Some(t)) => (f.id, t.id),
            _ => return Ok(()), // nothing to remove
        };

        links_store.retain(|l| {
            !(l.from_page_id == from_id
                && l.to_page_id == to_id
                && (link_type.is_none() || l.link_type == link_type.unwrap_or(""))
                && (link_source.is_none() || l.link_source.as_deref() == link_source))
        });
        Ok(())
    }

    async fn get_links(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<Link>> {
        let store = self.store.lock().expect("poisoned");
        let links_store = self.links_store.lock().expect("poisoned");

        let from_page = store.iter().find(|p| {
            p.slug == slug && (source_id.is_none() || p.source_id == source_id.unwrap_or(""))
        });
        let Some(from_page) = from_page else {
            return Ok(Vec::new());
        };

        let result: Vec<Link> = links_store
            .iter()
            .filter(|l| l.from_page_id == from_page.id)
            .filter_map(|l| {
                let to_page = store.iter().find(|p| p.id == l.to_page_id)?;
                let origin_slug = l.origin_page_id.and_then(|oid| {
                    store.iter().find(|p| p.id == oid).map(|p| p.slug.clone())
                });
                Some(Link {
                    from_slug: from_page.slug.clone(),
                    to_slug: to_page.slug.clone(),
                    link_type: l.link_type.clone(),
                    context: l.context.clone(),
                    link_source: l.link_source.clone(),
                    origin_slug,
                    origin_field: l.origin_field.clone(),
                })
            })
            .collect();
        Ok(result)
    }

    async fn get_backlinks(
        &self,
        slug: &str,
        source_id: Option<&str>,
    ) -> crate::Result<Vec<Link>> {
        let store = self.store.lock().expect("poisoned");
        let links_store = self.links_store.lock().expect("poisoned");

        let to_page = store.iter().find(|p| {
            p.slug == slug && (source_id.is_none() || p.source_id == source_id.unwrap_or(""))
        });
        let Some(to_page) = to_page else {
            return Ok(Vec::new());
        };

        let result: Vec<Link> = links_store
            .iter()
            .filter(|l| l.to_page_id == to_page.id)
            .filter_map(|l| {
                let from_page = store.iter().find(|p| p.id == l.from_page_id)?;
                let origin_slug = l.origin_page_id.and_then(|oid| {
                    store.iter().find(|p| p.id == oid).map(|p| p.slug.clone())
                });
                Some(Link {
                    from_slug: from_page.slug.clone(),
                    to_slug: to_page.slug.clone(),
                    link_type: l.link_type.clone(),
                    context: l.context.clone(),
                    link_source: l.link_source.clone(),
                    origin_slug,
                    origin_field: l.origin_field.clone(),
                })
            })
            .collect();
        Ok(result)
    }

    async fn get_backlink_counts(
        &self,
        slugs: &[String],
    ) -> crate::Result<std::collections::HashMap<String, u64>> {
        let store = self.store.lock().expect("poisoned");
        let links_store = self.links_store.lock().expect("poisoned");

        let slug_to_id: std::collections::HashMap<&str, u64> = store
            .iter()
            .filter_map(|p| {
                if slugs.contains(&p.slug) { Some((p.slug.as_str(), p.id)) } else { None }
            })
            .collect();

        let mut counts: std::collections::HashMap<String, u64> =
            slugs.iter().map(|s| (s.clone(), 0u64)).collect();

        for link in links_store.iter() {
            for (slug, page_id) in &slug_to_id {
                if link.to_page_id == *page_id {
                    *counts.get_mut(*slug).unwrap_or(&mut 0) += 1;
                }
            }
        }
        Ok(counts)
    }

    async fn get_adjacency_boosts(
        &self,
        page_ids: &[u64],
    ) -> crate::Result<std::collections::HashMap<u64, AdjacencyRow>> {
        if page_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let page_set: std::collections::HashSet<u64> = page_ids.iter().copied().collect();
        let store = self.store.lock().expect("poisoned");
        let links_store = self.links_store.lock().expect("poisoned");

        // Build page_id → source_id lookup for cross_source_hits calc
        let page_source: std::collections::HashMap<u64, String> = store
            .iter()
            .filter(|p| page_set.contains(&p.id))
            .map(|p| (p.id, p.source_id.clone()))
            .collect();

        let mut result: std::collections::HashMap<u64, AdjacencyRow> = std::collections::HashMap::new();

        for to_page_id in page_ids {
            let target_source = page_source.get(to_page_id).cloned().unwrap_or_default();

            // Collect distinct from_page_ids (in-set only) and their source_ids
            let mut from_sources: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut from_count: u32 = 0;

            for link in links_store.iter() {
                if link.to_page_id == *to_page_id && page_set.contains(&link.from_page_id) {
                    from_count += 1;
                    // Look up source of the linking page
                    if let Some(src) = page_source.get(&link.from_page_id) {
                        from_sources.insert(src.clone());
                    }
                }
            }

            if from_count > 0 {
                let cross_source_hits = from_sources
                    .iter()
                    .filter(|src| **src != target_source)
                    .count() as u32;

                result.insert(
                    *to_page_id,
                    AdjacencyRow {
                        hits: from_count,
                        cross_source_hits,
                    },
                );
            }
        }

        Ok(result)
    }

    async fn traverse_paths(
        &self,
        slug: &str,
        depth: Option<u32>,
        link_type: Option<&str>,
        direction: Option<&str>,
        source_id: Option<&str>,
        _source_ids: Option<&[String]>,
    ) -> crate::Result<Vec<GraphPath>> {
        let store = self.store.lock().expect("poisoned");
        let links_store = self.links_store.lock().expect("poisoned");

        let max_depth = depth.unwrap_or(1);
        let dir = direction.unwrap_or("out");

        let start_page = store.iter().find(|p| {
            p.slug == slug && (source_id.is_none() || p.source_id == source_id.unwrap_or(""))
        });
        let Some(start_page) = start_page else {
            return Ok(Vec::new());
        };

        let id_to_slug: std::collections::HashMap<u64, &str> = store
            .iter()
            .map(|p| (p.id, p.slug.as_str()))
            .collect();

        let mut result: Vec<GraphPath> = Vec::new();
        // BFS: (page_id, depth)
        let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<(u64, u32)> = std::collections::VecDeque::new();
        visited.insert(start_page.id);
        queue.push_back((start_page.id, 0));

        while let Some((current_id, current_depth)) = queue.pop_front() {
            if current_depth >= max_depth {
                continue;
            }

            let edges: Vec<&InternalLink> = links_store
                .iter()
                .filter(|l| {
                    if dir == "out" {
                        l.from_page_id == current_id
                    } else if dir == "in" {
                        l.to_page_id == current_id
                    } else if dir == "both" {
                        l.from_page_id == current_id || l.to_page_id == current_id
                    } else {
                        false
                    }
                })
                .filter(|l| {
                    link_type.is_none() || l.link_type == link_type.unwrap_or("")
                })
                .collect();

            for edge in &edges {
                // Determine the "other" side of the edge relative to current_id.
                // For outgoing (from→to where from=current): neighbor = to.
                // For incoming (to→from where to=current): neighbor = from.
                let neighbor_id = if dir == "in" {
                    edge.from_page_id
                } else if dir == "both" && edge.to_page_id == current_id {
                    // Incoming edge in "both" mode: walk back to the source.
                    edge.from_page_id
                } else {
                    edge.to_page_id
                };

                // GraphPath always preserves original edge direction:
                // from_slug = edge source, to_slug = edge target.
                let (from_slug_opt, to_slug_opt) = (
                    id_to_slug.get(&edge.from_page_id).map(|s| s.to_string()),
                    id_to_slug.get(&edge.to_page_id).map(|s| s.to_string()),
                );

                if let (Some(from), Some(to)) = (from_slug_opt, to_slug_opt) {
                    result.push(GraphPath {
                        from_slug: from,
                        to_slug: to,
                        link_type: edge.link_type.clone(),
                        context: edge.context.clone(),
                        depth: current_depth + 1,
                    });
                }

                if !visited.contains(&neighbor_id) {
                    visited.insert(neighbor_id);
                    queue.push_back((neighbor_id, current_depth + 1));
                }
            }
        }

        Ok(result)
    }

    // ── Facts (Phase 7B) ──────────────────────────────────────────────────

    async fn insert_fact(
        &self,
        source_id: &str,
        entity_slug: &str,
        input: &NewFact,
    ) -> crate::Result<FactInsertStatus> {
        let mut store = self.facts_store.lock().expect("poisoned");
        let mut next_id = self.next_fact_id.lock().expect("poisoned");
        let now = crate::time::current_utc_iso8601();

        // Duplicate detection: same source + same entity + same fact text +
        // same kind + still active. Duplicates are silently ignored.
        // Confidence threshold for supersede: > 0.9.
        let supersede_threshold = 0.9;
        let maybe_supersede = if input.confidence.unwrap_or(1.0) > supersede_threshold {
            // Find an active same-entity same-kind fact to supersede
            let target = store.iter().position(|f| {
                f.source_id == source_id
                    && f.entity_slug.as_deref() == Some(entity_slug)
                    && f.kind == input.kind.clone().unwrap_or(FactKind::Fact)
                    && f.expired_at.is_none()
                    && f.superseded_by.is_none()
            });
            target
        } else {
            None
        };

        // Always check for exact duplicate (regardless of confidence)
        let is_duplicate = store.iter().any(|f| {
            f.source_id == source_id
                && f.entity_slug.as_deref() == Some(entity_slug)
                && f.fact == input.fact
                && f.kind == input.kind.clone().unwrap_or(FactKind::Fact)
                && f.expired_at.is_none()
                && f.superseded_by.is_none()
        });

        if is_duplicate {
            return Ok(FactInsertStatus::Duplicate);
        }

        let new_id = *next_id;
        *next_id += 1;

        // If superseding, mark the old fact
        if let Some(pos) = maybe_supersede {
            store[pos].superseded_by = Some(new_id);
        }

        let row = FactRow {
            id: new_id,
            source_id: source_id.to_string(),
            entity_slug: Some(entity_slug.to_string()),
            fact: input.fact.clone(),
            kind: input.kind.clone().unwrap_or(FactKind::Fact),
            visibility: input.visibility.clone().unwrap_or(FactVisibility::Private),
            notability: input
                .notability
                .clone()
                .unwrap_or_else(|| "medium".to_string()),
            context: input.context.clone(),
            valid_from: Some(
                input
                    .valid_from
                    .clone()
                    .unwrap_or_else(|| now.clone()),
            ),
            valid_until: input.valid_until.clone(),
            expired_at: None,
            superseded_by: None,
            consolidated_at: None,
            consolidated_into: None,
            source: input.source.clone(),
            source_session: input.source_session.clone(),
            confidence: input.confidence.unwrap_or(1.0),
            created_at: Some(now),
            row_num: input.row_num,
            source_markdown_slug: input.source_markdown_slug.clone(),
        };
        store.push(row);

        if maybe_supersede.is_some() {
            Ok(FactInsertStatus::Superseded)
        } else {
            Ok(FactInsertStatus::Inserted)
        }
    }

    async fn delete_facts_for_page(
        &self,
        slug: &str,
        source_id: &str,
    ) -> crate::Result<i64> {
        let mut store = self.facts_store.lock().expect("poisoned");
        let before = store.len();
        store.retain(|f| {
            !(f.source_markdown_slug.as_deref() == Some(slug) && f.source_id == source_id)
        });
        Ok((before - store.len()) as i64)
    }

    async fn count_legacy_fact_rows(&self) -> crate::Result<i64> {
        let store = self.facts_store.lock().expect("poisoned");
        let n = store
            .iter()
            .filter(|f| f.row_num.is_none() && f.entity_slug.is_some())
            .count();
        Ok(n as i64)
    }

    async fn list_facts_by_entity(
        &self,
        source_id: &str,
        entity_slug: &str,
        opts: &FactListOpts,
    ) -> crate::Result<Vec<FactRow>> {
        let store = self.facts_store.lock().expect("poisoned");

        let mut rows: Vec<FactRow> = store
            .iter()
            .filter(|f| f.source_id == source_id)
            .filter(|f| f.entity_slug.as_deref() == Some(entity_slug))
            .filter(|f| {
                if opts.active_only.unwrap_or(false) {
                    f.expired_at.is_none() && f.superseded_by.is_none()
                } else {
                    true
                }
            })
            .filter(|f| {
                opts.kinds
                    .as_ref()
                    .map_or(true, |ks| ks.iter().any(|k| f.kind == *k))
            })
            .filter(|f| {
                opts.visibility
                    .as_ref()
                    .map_or(true, |vs| vs.iter().any(|v| f.visibility == *v))
            })
            .cloned()
            .collect();

        // Newest first (mirrors TS ORDER BY created_at DESC)
        rows.sort_by(|a, b| {
            b.created_at
                .as_deref()
                .unwrap_or("")
                .cmp(&a.created_at.as_deref().unwrap_or(""))
        });

        let offset = opts.offset.unwrap_or(0) as usize;
        if offset > 0 {
            rows = rows.into_iter().skip(offset).collect();
        }
        if let Some(limit) = opts.limit {
            rows.truncate(limit as usize);
        }

        Ok(rows)
    }

    async fn list_facts_since(
        &self,
        source_id: &str,
        since_iso: &str,
        opts: &FactListOpts,
    ) -> crate::Result<Vec<FactRow>> {
        let store = self.facts_store.lock().expect("poisoned");
        let mut rows: Vec<FactRow> = store
            .iter()
            .filter(|f| f.source_id == source_id)
            .filter(|f| f.created_at.as_deref().unwrap_or("") >= since_iso)
            .filter(|f| fact_passes_list_filters(f, opts))
            .cloned()
            .collect();
        sort_facts_newest_first(&mut rows);
        apply_fact_paging(&mut rows, opts);
        Ok(rows)
    }

    async fn list_facts_by_session(
        &self,
        source_id: &str,
        session_id: &str,
        opts: &FactListOpts,
    ) -> crate::Result<Vec<FactRow>> {
        let store = self.facts_store.lock().expect("poisoned");
        let mut rows: Vec<FactRow> = store
            .iter()
            .filter(|f| f.source_id == source_id)
            .filter(|f| f.source_session.as_deref() == Some(session_id))
            .filter(|f| fact_passes_list_filters(f, opts))
            .cloned()
            .collect();
        sort_facts_newest_first(&mut rows);
        apply_fact_paging(&mut rows, opts);
        Ok(rows)
    }

    async fn list_supersessions(
        &self,
        source_id: &str,
        opts: &crate::types::SupersessionOpts,
    ) -> crate::Result<Vec<FactRow>> {
        let store = self.facts_store.lock().expect("poisoned");
        let mut rows: Vec<FactRow> = store
            .iter()
            .filter(|f| f.source_id == source_id)
            .filter(|f| f.expired_at.is_some() && f.superseded_by.is_some())
            .filter(|f| {
                opts.since
                    .as_deref()
                    .map_or(true, |s| f.expired_at.as_deref().unwrap_or("") >= s)
            })
            .cloned()
            .collect();
        // Newest first by expiry (mirrors TS ORDER BY expired_at DESC)
        rows.sort_by(|a, b| {
            b.expired_at
                .as_deref()
                .unwrap_or("")
                .cmp(&a.expired_at.as_deref().unwrap_or(""))
        });
        if let Some(limit) = opts.limit {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    async fn count_unconsolidated_facts(&self, source_id: &str) -> crate::Result<i64> {
        let store = self.facts_store.lock().expect("poisoned");
        Ok(store
            .iter()
            .filter(|f| {
                f.source_id == source_id
                    && f.consolidated_at.is_none()
                    && f.expired_at.is_none()
            })
            .count() as i64)
    }

    async fn get_facts_health(&self, source_id: &str) -> crate::Result<FactsHealth> {
        let store = self.facts_store.lock().expect("poisoned");
        let now = crate::time::current_utc_iso8601();

        let total_active = store
            .iter()
            .filter(|f| f.source_id == source_id && f.expired_at.is_none() && f.superseded_by.is_none())
            .count() as i64;

        let total_today = store
            .iter()
            .filter(|f| f.source_id == source_id && f.created_at.as_deref().unwrap_or("") >= &now[..10])
            .count() as i64;

        // Week: last 7 days (approximate by prefix check on date part)
        let week_cutoff = {
            // Simple approximation: today minus 7 days
            use chrono::{NaiveDate, Duration};
            let today = NaiveDate::parse_from_str(&now[..10], "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
            let cutoff = today - Duration::days(7);
            cutoff.format("%Y-%m-%d").to_string()
        };
        let total_week = store
            .iter()
            .filter(|f| f.source_id == source_id && f.created_at.as_deref().unwrap_or("") >= week_cutoff.as_str())
            .count() as i64;

        let total_expired = store
            .iter()
            .filter(|f| f.source_id == source_id && f.expired_at.is_some())
            .count() as i64;

        let total_consolidated = store
            .iter()
            .filter(|f| f.source_id == source_id && f.consolidated_at.is_some())
            .count() as i64;

        // Top entities by fact count
        let mut entity_counts: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for f in store.iter().filter(|f| f.source_id == source_id) {
            if let Some(ref slug) = f.entity_slug {
                *entity_counts.entry(slug.clone()).or_insert(0) += 1;
            }
        }
        let mut top_entities: Vec<EntityCount> = entity_counts
            .into_iter()
            .map(|(entity_slug, count)| EntityCount { entity_slug, count })
            .collect();
        top_entities.sort_by(|a, b| b.count.cmp(&a.count));
        top_entities.truncate(10);

        Ok(FactsHealth {
            source_id: source_id.to_string(),
            total_active,
            total_today,
            total_week,
            total_expired,
            total_consolidated,
            top_entities,
        })
    }

    async fn expire_fact(&self, source_id: &str, fact_id: i64) -> crate::Result<bool> {
        let mut store = self.facts_store.lock().expect("poisoned");
        let now = crate::time::current_utc_iso8601();

        if let Some(fact) = store
            .iter_mut()
            .find(|f| f.id == fact_id && f.source_id == source_id && f.expired_at.is_none())
        {
            fact.expired_at = Some(now);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // --- Minion job queue (Phase 9, slice 1-1-1 A+B) ---
    //
    // In-memory implementation of the job queue trait methods. The `Mutex`
    // held across each method body is the InMemory analogue of the SQL
    // backends' row-level locking: because only one method can hold the lock
    // at a time, claim/complete/fail are atomic here for free. Scheduling
    // columns (lock_until/delay_until/timeout_at) are epoch-ms and all
    // `now + N ms` arithmetic happens in Rust via `now_epoch_ms()`.

    async fn enqueue_job(
        &self,
        input: &crate::minions::types::MinionJobInput,
    ) -> crate::Result<crate::minions::types::MinionJob> {
        use crate::minions::types::{MinionJob, MinionJobStatus};

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");
        let mut next_id = self
            .next_job_id
            .lock()
            .expect("InMemoryEngine next_job_id mutex poisoned");

        // Idempotency fast path: a matching non-null key returns the existing
        // row without inserting a second (mirrors the unique partial index).
        if let Some(ref key) = input.idempotency_key {
            if let Some(existing) = store
                .iter()
                .find(|j| j.idempotency_key.as_deref() == Some(key.as_str()))
            {
                return Ok(existing.clone());
            }
        }

        let now_iso = crate::time::current_utc_iso8601();
        let now_ms = crate::time::now_epoch_ms();

        // A delay sets status=delayed and delay_until = now + delay.
        let (status, delay_until) = match input.delay {
            Some(d) if d > 0 => (MinionJobStatus::Delayed, Some(now_ms + d)),
            _ => (MinionJobStatus::Waiting, None),
        };

        // Per-job stall tolerance override: clamped to [1, 100]; omitted ->
        // schema DEFAULT (5).
        let max_stalled = input.max_stalled.map_or(5, |v| v.clamp(1, 100));

        // D-layer (1-1-3-1): parent/child spawn. Validate spawn depth +
        // max_children against the parent, and derive this child's depth. The
        // parent flip to `waiting-children` happens after the child is pushed.
        // maxSpawnDepth default = 5 (TS DEFAULT_MAX_SPAWN_DEPTH).
        const MAX_SPAWN_DEPTH: i32 = 5;
        let mut depth = 0;
        if let Some(parent_id) = input.parent_job_id {
            let Some(parent) = store.iter().find(|j| j.id == parent_id) else {
                return Err(crate::error::StructuredError::new(
                    "InvalidInput",
                    "invalid_input",
                    format!("parent_job_id {parent_id} not found"),
                ));
            };
            depth = parent.depth + 1;
            if depth > MAX_SPAWN_DEPTH {
                return Err(crate::error::StructuredError::new(
                    "InvalidInput",
                    "invalid_input",
                    format!("spawn depth {depth} exceeds maxSpawnDepth {MAX_SPAWN_DEPTH}"),
                ));
            }
            if let Some(cap) = parent.max_children {
                let live = store
                    .iter()
                    .filter(|j| {
                        j.parent_job_id == Some(parent_id) && !j.status.is_terminal()
                    })
                    .count() as i32;
                if live >= cap {
                    return Err(crate::error::StructuredError::new(
                        "InvalidInput",
                        "invalid_input",
                        format!(
                            "parent {parent_id} already has {live} live children (max_children={cap})"
                        ),
                    ));
                }
            }
        }

        let id = *next_id;
        *next_id += 1;

        let job = MinionJob {
            id,
            name: input.name.clone(),
            queue: input.queue.clone().unwrap_or_else(|| "default".to_string()),
            status,
            priority: input.priority.unwrap_or(0),
            data: input.data.clone().unwrap_or_else(|| serde_json::json!({})),
            max_attempts: input.max_attempts.unwrap_or(3),
            attempts_made: 0,
            attempts_started: 0,
            backoff_type: input
                .backoff_type
                .unwrap_or(crate::minions::types::BackoffType::Exponential),
            backoff_delay: input.backoff_delay.unwrap_or(1000),
            backoff_jitter: input.backoff_jitter.unwrap_or(0.2),
            stalled_counter: 0,
            max_stalled,
            lock_token: None,
            lock_until: None,
            delay_until,
            parent_job_id: input.parent_job_id,
            on_child_fail: input
                .on_child_fail
                .unwrap_or(crate::minions::types::ChildFailPolicy::FailParent),
            tokens_input: 0,
            tokens_output: 0,
            tokens_cache_read: 0,
            depth,
            max_children: input.max_children,
            timeout_ms: input.timeout_ms,
            timeout_at: None,
            remove_on_complete: input.remove_on_complete.unwrap_or(false),
            remove_on_fail: input.remove_on_fail.unwrap_or(false),
            idempotency_key: input.idempotency_key.clone(),
            quiet_hours: None,
            stagger_key: None,
            result: None,
            progress: None,
            error_text: None,
            stacktrace: Vec::new(),
            created_at: now_iso.clone(),
            started_at: None,
            finished_at: None,
            updated_at: now_iso,
        };
        store.push(job.clone());

        // Flip the parent into waiting-children now that a fresh child exists.
        // Only from non-terminal, non-already-waiting-children states.
        if let Some(parent_id) = input.parent_job_id {
            if let Some(parent) = store.iter_mut().find(|j| j.id == parent_id) {
                if matches!(
                    parent.status,
                    MinionJobStatus::Waiting
                        | MinionJobStatus::Active
                        | MinionJobStatus::Delayed
                ) {
                    parent.status = MinionJobStatus::WaitingChildren;
                    parent.updated_at = crate::time::current_utc_iso8601();
                }
            }
        }

        Ok(job)
    }

    async fn get_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        let store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");
        Ok(store.iter().find(|j| j.id == id).cloned())
    }

    async fn get_jobs(
        &self,
        filters: &crate::minions::types::JobFilters,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        let store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let mut rows: Vec<crate::minions::types::MinionJob> = store
            .iter()
            .filter(|j| filters.status.is_none_or(|s| j.status == s))
            .filter(|j| filters.queue.as_ref().is_none_or(|q| j.queue == *q))
            .filter(|j| filters.name.as_ref().is_none_or(|n| j.name == *n))
            .cloned()
            .collect();

        // Newest first: by id DESC (monotonic proxy for created_at DESC).
        rows.sort_by(|a, b| b.id.cmp(&a.id));

        let offset = filters.offset.unwrap_or(0).max(0) as usize;
        if offset > 0 {
            rows = rows.into_iter().skip(offset).collect();
        }
        let limit = filters.limit.unwrap_or(50).max(0) as usize;
        rows.truncate(limit);

        Ok(rows)
    }

    async fn claim_job(
        &self,
        lock_token: &str,
        lock_duration_ms: i64,
        queue: &str,
        registered_names: &[String],
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        if registered_names.is_empty() {
            return Ok(None);
        }

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // Select the next eligible waiting job: priority ASC, then created_at
        // ASC (id ASC as monotonic proxy). Filtered by queue + registered name.
        let idx = store
            .iter()
            .enumerate()
            .filter(|(_, j)| {
                j.status == MinionJobStatus::Waiting
                    && j.queue == queue
                    && registered_names.iter().any(|n| *n == j.name)
            })
            .min_by(|(_, a), (_, b)| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)))
            .map(|(i, _)| i);

        let Some(i) = idx else {
            return Ok(None);
        };

        let now_iso = crate::time::current_utc_iso8601();
        let now_ms = crate::time::now_epoch_ms();
        let job = &mut store[i];
        job.status = MinionJobStatus::Active;
        job.lock_token = Some(lock_token.to_string());
        job.lock_until = Some(now_ms + lock_duration_ms);
        job.timeout_at = job.timeout_ms.map(|t| now_ms + t);
        job.attempts_started += 1;
        if job.started_at.is_none() {
            job.started_at = Some(now_iso.clone());
        }
        job.updated_at = now_iso;
        Ok(Some(job.clone()))
    }

    async fn complete_job(
        &self,
        id: i64,
        lock_token: &str,
        result: Option<&serde_json::Value>,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // Token-fenced: only an active job with the matching lock_token flips.
        let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && j.status == MinionJobStatus::Active
                && j.lock_token.as_deref() == Some(lock_token)
        }) else {
            return Ok(None);
        };

        let now_iso = crate::time::current_utc_iso8601();
        job.status = MinionJobStatus::Completed;
        job.result = result.cloned();
        job.finished_at = Some(now_iso.clone());
        job.lock_token = None;
        job.lock_until = None;
        job.updated_at = now_iso;
        let completed = job.clone();

        // D-layer (1-1-3-1) parent hook: roll up tokens, emit child_done, and
        // resolve the parent if all its children are now terminal.
        if let Some(parent_id) = completed.parent_job_id {
            if completed.tokens_input > 0
                || completed.tokens_output > 0
                || completed.tokens_cache_read > 0
            {
                if let Some(parent) = store
                    .iter_mut()
                    .find(|j| j.id == parent_id && !j.status.is_terminal())
                {
                    parent.tokens_input += completed.tokens_input;
                    parent.tokens_output += completed.tokens_output;
                    parent.tokens_cache_read += completed.tokens_cache_read;
                    parent.updated_at = crate::time::current_utc_iso8601();
                }
            }
            self.emit_child_done_inmem(
                &store,
                parent_id,
                completed.id,
                &completed.name,
                result.cloned().unwrap_or(serde_json::Value::Null),
                crate::minions::types::ChildOutcome::Complete,
                None,
            );
            Self::resolve_parent_inmem(&mut store, parent_id);
        }

        // remove_on_complete: drop the row after capturing the return value.
        if completed.remove_on_complete {
            store.retain(|j| j.id != id);
        }
        Ok(Some(completed))
    }

    async fn fail_job(
        &self,
        id: i64,
        lock_token: &str,
        error_text: &str,
        outcome: crate::minions::types::FailOutcome,
        backoff_ms: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::{ChildFailPolicy, FailOutcome, MinionJobStatus};

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && j.status == MinionJobStatus::Active
                && j.lock_token.as_deref() == Some(lock_token)
        }) else {
            return Ok(None);
        };

        let now_iso = crate::time::current_utc_iso8601();
        let now_ms = crate::time::now_epoch_ms();
        job.status = outcome.as_status();
        job.error_text = Some(error_text.to_string());
        job.attempts_made += 1;
        job.stacktrace.push(error_text.to_string());
        job.lock_token = None;
        job.lock_until = None;
        // Delayed retry sets delay_until; terminal outcomes set finished_at.
        if outcome == FailOutcome::Delayed {
            job.delay_until = Some(now_ms + backoff_ms);
            job.finished_at = None;
        } else {
            job.delay_until = None;
            job.finished_at = Some(now_iso.clone());
        }
        job.updated_at = now_iso;
        let failed = job.clone();

        // D-layer (1-1-3-1) parent hook on terminal failure. Emit child_done
        // BEFORE any parent-terminal flip (the EXISTS guard on emit would drop
        // the message once the parent is failed), then apply on_child_fail.
        if outcome.is_terminal() {
            if let Some(parent_id) = failed.parent_job_id {
                let child_outcome = if outcome == FailOutcome::Dead {
                    crate::minions::types::ChildOutcome::Dead
                } else {
                    crate::minions::types::ChildOutcome::Failed
                };
                self.emit_child_done_inmem(
                    &store,
                    parent_id,
                    failed.id,
                    &failed.name,
                    serde_json::Value::Null,
                    child_outcome,
                    Some(error_text.to_string()),
                );

                match failed.on_child_fail {
                    ChildFailPolicy::FailParent => {
                        if let Some(parent) = store.iter_mut().find(|j| {
                            j.id == parent_id && j.status == MinionJobStatus::WaitingChildren
                        }) {
                            parent.status = MinionJobStatus::Failed;
                            parent.error_text = Some(format!(
                                "child job {} failed: {error_text}",
                                failed.id
                            ));
                            let now = crate::time::current_utc_iso8601();
                            parent.finished_at = Some(now.clone());
                            parent.updated_at = now;
                        }
                    }
                    ChildFailPolicy::RemoveDep => {
                        // Drop this child's dep, then try to resolve the parent
                        // if all OTHER kids are terminal.
                        if let Some(this) = store.iter_mut().find(|j| j.id == failed.id) {
                            this.parent_job_id = None;
                            this.updated_at = crate::time::current_utc_iso8601();
                        }
                        Self::resolve_parent_inmem(&mut store, parent_id);
                    }
                    ChildFailPolicy::Ignore | ChildFailPolicy::Continue => {
                        // Parent stays in waiting-children on siblings; the last
                        // terminal child transitioning here still unblocks it.
                        Self::resolve_parent_inmem(&mut store, parent_id);
                    }
                }
            }
        }

        // remove_on_fail on a terminal outcome: drop the row.
        if outcome.is_terminal() && failed.remove_on_fail {
            store.retain(|j| j.id != id);
        }
        Ok(Some(failed))
    }

    async fn renew_job_lock(
        &self,
        id: i64,
        lock_token: &str,
        lock_duration_ms: i64,
    ) -> crate::Result<bool> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        if let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && j.status == MinionJobStatus::Active
                && j.lock_token.as_deref() == Some(lock_token)
        }) {
            let now_iso = crate::time::current_utc_iso8601();
            job.lock_until = Some(crate::time::now_epoch_ms() + lock_duration_ms);
            job.updated_at = now_iso;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn retry_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // Only failed/dead jobs can be requeued; clears error/lock/delay.
        let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && matches!(j.status, MinionJobStatus::Failed | MinionJobStatus::Dead)
        }) else {
            return Ok(None);
        };

        job.status = MinionJobStatus::Waiting;
        job.error_text = None;
        job.lock_token = None;
        job.lock_until = None;
        job.delay_until = None;
        job.finished_at = None;
        job.updated_at = crate::time::current_utc_iso8601();
        Ok(Some(job.clone()))
    }

    // --- Ops: pause / resume (1-1-3-3) ---

    async fn pause_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // Pausable only from waiting/active/delayed; clears the lock so an
        // active worker's abort fires. waiting-children is intentionally out
        // (matches TS pauseJob WHERE); registered in
        // docs/plans/KNOWN-GAPS.md (G28).
        let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && matches!(
                    j.status,
                    MinionJobStatus::Waiting
                        | MinionJobStatus::Active
                        | MinionJobStatus::Delayed
                )
        }) else {
            return Ok(None);
        };

        job.status = MinionJobStatus::Paused;
        job.lock_token = None;
        job.lock_until = None;
        job.updated_at = crate::time::current_utc_iso8601();
        Ok(Some(job.clone()))
    }

    async fn resume_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // Strict paused -> waiting.
        let Some(job) = store
            .iter_mut()
            .find(|j| j.id == id && j.status == MinionJobStatus::Paused)
        else {
            return Ok(None);
        };

        job.status = MinionJobStatus::Waiting;
        job.lock_token = None;
        job.lock_until = None;
        job.updated_at = crate::time::current_utc_iso8601();
        Ok(Some(job.clone()))
    }

    async fn prune_jobs(
        &self,
        statuses: &[crate::minions::types::MinionJobStatus],
        older_than_rfc3339: &str,
    ) -> crate::Result<i64> {
        // Delete terminal jobs whose `updated_at` is older than the cutoff.
        // `updated_at` is an RFC-3339 string; ISO-8601 lexical order == time
        // order, so a plain `<` string compare is a valid time compare and
        // matches the SQL backends (which also compare the text/timestamptz
        // column). Manually cascade to inbox + attachments because the
        // in-memory store has no DB foreign keys (the SQL backends rely on
        // ON DELETE CASCADE).
        let removed_ids: Vec<i64> = {
            let mut store = self
                .minion_jobs_store
                .lock()
                .expect("InMemoryEngine minion_jobs_store mutex poisoned");
            let mut removed = Vec::new();
            store.retain(|j| {
                let prune = statuses.contains(&j.status)
                    && j.updated_at.as_str() < older_than_rfc3339;
                if prune {
                    removed.push(j.id);
                }
                !prune
            });
            removed
        };

        if !removed_ids.is_empty() {
            let mut inbox = self
                .minion_inbox_store
                .lock()
                .expect("InMemoryEngine minion_inbox_store mutex poisoned");
            inbox.retain(|m| !removed_ids.contains(&m.job_id));

            let mut atts = self
                .minion_attachments_store
                .lock()
                .expect("InMemoryEngine minion_attachments_store mutex poisoned");
            atts.retain(|a| !removed_ids.contains(&a.meta.job_id));
        }

        Ok(removed_ids.len() as i64)
    }

    async fn get_stats(
        &self,
        since_rfc3339: &str,
    ) -> crate::Result<crate::minions::types::QueueStats> {
        use crate::minions::types::{MinionJobStatus, QueueHealth, QueueStats, QueueTypeStat};
        use std::collections::BTreeMap;

        let store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // by_status: count every job by its status label (all-time).
        let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
        for job in store.iter() {
            *by_status.entry(job.status.as_str().to_string()).or_insert(0) += 1;
        }

        // by_type: per-name breakdown within the `since` window. Accumulate
        // totals + terminal counts + a running duration sum/count so we can
        // average at the end. `created_at` is RFC-3339 text; ISO-8601 lexical
        // order == time order, so a `>=` string compare bounds the window.
        struct Acc {
            total: i64,
            completed: i64,
            failed: i64,
            dead: i64,
            dur_sum_ms: i64,
            dur_count: i64,
        }
        let mut types: BTreeMap<String, Acc> = BTreeMap::new();
        for job in store.iter() {
            if job.created_at.as_str() < since_rfc3339 {
                continue;
            }
            let acc = types.entry(job.name.clone()).or_insert(Acc {
                total: 0,
                completed: 0,
                failed: 0,
                dead: 0,
                dur_sum_ms: 0,
                dur_count: 0,
            });
            acc.total += 1;
            match job.status {
                MinionJobStatus::Completed => acc.completed += 1,
                MinionJobStatus::Failed => acc.failed += 1,
                MinionJobStatus::Dead => acc.dead += 1,
                _ => {}
            }
            // avg_duration: only rows with both started_at and finished_at,
            // matching the TS `FILTER (WHERE finished_at IS NOT NULL AND
            // started_at IS NOT NULL)`.
            if let (Some(started), Some(finished)) =
                (job.started_at.as_deref(), job.finished_at.as_deref())
            {
                if let (Ok(s), Ok(f)) = (
                    chrono::DateTime::parse_from_rfc3339(started),
                    chrono::DateTime::parse_from_rfc3339(finished),
                ) {
                    acc.dur_sum_ms += f.timestamp_millis() - s.timestamp_millis();
                    acc.dur_count += 1;
                }
            }
        }
        // TS orders by total DESC; ties fall back to name for determinism.
        let mut by_type: Vec<QueueTypeStat> = types
            .into_iter()
            .map(|(name, a)| QueueTypeStat {
                name,
                total: a.total,
                completed: a.completed,
                failed: a.failed,
                dead: a.dead,
                avg_duration_ms: if a.dur_count > 0 {
                    // Round to nearest ms, matching TS Math.round.
                    Some((a.dur_sum_ms as f64 / a.dur_count as f64).round() as i64)
                } else {
                    None
                },
            })
            .collect();
        by_type.sort_by(|x, y| y.total.cmp(&x.total).then_with(|| x.name.cmp(&y.name)));

        // queue_health: stalled = active jobs whose lease (lock_until, epoch-ms)
        // has expired.
        let now_ms = crate::time::now_epoch_ms();
        let stalled = store
            .iter()
            .filter(|j| {
                j.status == MinionJobStatus::Active
                    && j.lock_until.is_some_and(|lu| lu < now_ms)
            })
            .count() as i64;

        Ok(QueueStats {
            queue_health: QueueHealth {
                waiting: by_status.get("waiting").copied().unwrap_or(0),
                active: by_status.get("active").copied().unwrap_or(0),
                stalled,
            },
            by_status,
            by_type,
        })
    }


    //
    // Hold the store mutex and scan the Vec. The 3 pure sweeps compare the
    // epoch-ms scheduling columns against `now_ms`; wall-clock parses the
    // RFC-3339 `started_at` string. Pure C-layer only: no inbox / parent
    // unblock (D-layer, 1-1-3).

    async fn promote_delayed(
        &self,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let now_ms = crate::time::now_epoch_ms();
        let now_iso = crate::time::current_utc_iso8601();
        let mut promoted = Vec::new();
        for job in store.iter_mut() {
            if job.status == MinionJobStatus::Delayed
                && job.delay_until.is_some_and(|d| d <= now_ms)
            {
                job.status = MinionJobStatus::Waiting;
                job.delay_until = None;
                job.lock_token = None;
                job.lock_until = None;
                job.updated_at = now_iso.clone();
                promoted.push(job.clone());
            }
        }
        Ok(promoted)
    }

    async fn handle_stalled(
        &self,
    ) -> crate::Result<crate::minions::types::StalledSweep> {
        use crate::minions::types::{MinionJobStatus, StalledSweep};

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let now_ms = crate::time::now_epoch_ms();
        let now_iso = crate::time::current_utc_iso8601();
        let mut sweep = StalledSweep::default();
        for job in store.iter_mut() {
            // Stalled = active with an expired lease.
            if job.status != MinionJobStatus::Active
                || !job.lock_until.is_some_and(|l| l < now_ms)
            {
                continue;
            }
            job.stalled_counter += 1;
            job.lock_token = None;
            job.lock_until = None;
            job.updated_at = now_iso.clone();
            if job.stalled_counter < job.max_stalled {
                job.status = MinionJobStatus::Waiting;
                sweep.requeued.push(job.clone());
            } else {
                job.status = MinionJobStatus::Dead;
                job.error_text = Some("max stalled count exceeded".to_string());
                job.finished_at = Some(now_iso.clone());
                sweep.dead.push(job.clone());
            }
        }
        Ok(sweep)
    }

    async fn handle_timeouts(
        &self,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let now_ms = crate::time::now_epoch_ms();
        let now_iso = crate::time::current_utc_iso8601();
        let mut dead = Vec::new();
        for job in store.iter_mut() {
            // Active, per-job timeout elapsed, lease still held (a stalled job
            // with an expired lease is left for handle_stalled).
            if job.status == MinionJobStatus::Active
                && job.timeout_at.is_some_and(|t| t < now_ms)
                && job.lock_until.is_some_and(|l| l > now_ms)
            {
                job.status = MinionJobStatus::Dead;
                job.error_text = Some("timeout exceeded".to_string());
                job.lock_token = None;
                job.lock_until = None;
                job.finished_at = Some(now_iso.clone());
                job.updated_at = now_iso.clone();
                dead.push(job.clone());
            }
        }
        Ok(dead)
    }

    async fn handle_wall_clock_timeouts(
        &self,
        lock_duration_ms: i64,
    ) -> crate::Result<Vec<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let now_ms = crate::time::now_epoch_ms();
        let now_iso = crate::time::current_utc_iso8601();
        let mut dead = Vec::new();
        for job in store.iter_mut() {
            if job.status != MinionJobStatus::Active {
                continue;
            }
            let Some(started) = job.started_at.as_deref() else {
                continue;
            };
            // Parse the RFC-3339 record column to epoch-ms; skip unparseable.
            let Ok(started_dt) = chrono::DateTime::parse_from_rfc3339(started) else {
                continue;
            };
            let elapsed_ms = now_ms - started_dt.timestamp_millis();
            let threshold_ms = match job.timeout_ms {
                Some(t) => t * 2,
                None => lock_duration_ms * 2 * job.max_stalled.max(1) as i64,
            };
            if elapsed_ms > threshold_ms {
                job.status = MinionJobStatus::Dead;
                job.error_text = Some("wall-clock timeout exceeded".to_string());
                job.lock_token = None;
                job.lock_until = None;
                job.finished_at = Some(now_iso.clone());
                job.updated_at = now_iso.clone();
                dead.push(job.clone());
            }
        }
        Ok(dead)
    }

    async fn set_started_at_for_test(
        &self,
        id: i64,
        started_at_rfc3339: &str,
    ) -> crate::Result<()> {
        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");
        if let Some(job) = store.iter_mut().find(|j| j.id == id) {
            job.started_at = Some(started_at_rfc3339.to_string());
        }
        Ok(())
    }

    async fn set_timeout_at_for_test(&self, id: i64, timeout_at_ms: i64) -> crate::Result<()> {
        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");
        if let Some(job) = store.iter_mut().find(|j| j.id == id) {
            job.timeout_at = Some(timeout_at_ms);
        }
        Ok(())
    }

    // ─── Minion D-layer methods (1-1-3-1) ───────────────────────────────────

    async fn cancel_job(
        &self,
        id: i64,
    ) -> crate::Result<Option<crate::minions::types::MinionJob>> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // Collect the descendant subtree (BFS, depth-capped at 100 to match the
        // SQL recursive CTE). Only non-terminal rows are actually cancelled.
        let mut subtree: Vec<i64> = Vec::new();
        let mut frontier = vec![id];
        let mut depth = 0;
        while !frontier.is_empty() && depth <= 100 {
            let mut next = Vec::new();
            for cur in frontier {
                if !subtree.contains(&cur) {
                    subtree.push(cur);
                }
                for j in store.iter() {
                    if j.parent_job_id == Some(cur) && !subtree.contains(&j.id) {
                        next.push(j.id);
                    }
                }
            }
            frontier = next;
            depth += 1;
        }

        // Cancel each non-terminal row; record (child_id, parent_id, name) for
        // child_done emission + parent resolution. Track whether the root row
        // itself transitioned this call (TS returns the root only if it did —
        // an already-terminal root yields None).
        let now_iso = crate::time::current_utc_iso8601();
        let mut affected: Vec<(i64, i64, String)> = Vec::new();
        let mut root_transitioned = false;
        for cid in &subtree {
            if let Some(j) = store
                .iter_mut()
                .find(|j| j.id == *cid && !j.status.is_terminal())
            {
                j.status = MinionJobStatus::Cancelled;
                j.lock_token = None;
                j.lock_until = None;
                j.finished_at = Some(now_iso.clone());
                j.updated_at = now_iso.clone();
                if j.id == id {
                    root_transitioned = true;
                }
                if let Some(pid) = j.parent_job_id {
                    affected.push((j.id, pid, j.name.clone()));
                }
            }
        }

        // If the root was already terminal (nothing cancelled at the root), the
        // TS contract returns None.
        if !root_transitioned {
            return Ok(None);
        }

        // Emit child_done(cancelled) into each affected parent + resolve.
        let mut parent_ids: Vec<i64> = Vec::new();
        for (child_id, parent_id, name) in affected {
            self.emit_child_done_inmem(
                &store,
                parent_id,
                child_id,
                &name,
                serde_json::Value::Null,
                crate::minions::types::ChildOutcome::Cancelled,
                Some("cancelled".to_string()),
            );
            if !parent_ids.contains(&parent_id) {
                parent_ids.push(parent_id);
            }
        }
        for parent_id in parent_ids {
            Self::resolve_parent_inmem(&mut store, parent_id);
        }

        Ok(store.iter().find(|j| j.id == id).cloned())
    }

    async fn send_message(
        &self,
        job_id: i64,
        payload: &serde_json::Value,
        sender: &str,
    ) -> crate::Result<Option<crate::minions::types::InboxMessage>> {
        let store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        // Target must exist and be non-terminal.
        let Some(job) = store.iter().find(|j| j.id == job_id) else {
            return Ok(None);
        };
        if job.status.is_terminal() {
            return Ok(None);
        }
        // Sender must be 'admin' or the job's parent id (as a string).
        let parent_str = job.parent_job_id.map(|p| p.to_string());
        if sender != "admin" && Some(sender.to_string()) != parent_str {
            return Ok(None);
        }
        drop(store);

        let mut inbox = self
            .minion_inbox_store
            .lock()
            .expect("InMemoryEngine minion_inbox_store mutex poisoned");
        let mut next = self
            .next_inbox_id
            .lock()
            .expect("InMemoryEngine next_inbox_id mutex poisoned");
        let msg = crate::minions::types::InboxMessage {
            id: *next,
            job_id,
            sender: sender.to_string(),
            payload: payload.clone(),
            sent_at: crate::time::current_utc_iso8601(),
            read_at: None,
        };
        *next += 1;
        inbox.push(msg.clone());
        Ok(Some(msg))
    }

    async fn read_inbox(
        &self,
        job_id: i64,
        lock_token: &str,
    ) -> crate::Result<Vec<crate::minions::types::InboxMessage>> {
        use crate::minions::types::MinionJobStatus;

        // Token fence: caller must hold the active lease.
        {
            let store = self
                .minion_jobs_store
                .lock()
                .expect("InMemoryEngine minion_jobs_store mutex poisoned");
            let held = store.iter().any(|j| {
                j.id == job_id
                    && j.status == MinionJobStatus::Active
                    && j.lock_token.as_deref() == Some(lock_token)
            });
            if !held {
                return Ok(Vec::new());
            }
        }

        let mut inbox = self
            .minion_inbox_store
            .lock()
            .expect("InMemoryEngine minion_inbox_store mutex poisoned");
        let now_iso = crate::time::current_utc_iso8601();
        let mut out = Vec::new();
        for m in inbox.iter_mut() {
            if m.job_id == job_id && m.read_at.is_none() {
                m.read_at = Some(now_iso.clone());
                out.push(m.clone());
            }
        }
        Ok(out)
    }

    async fn read_child_completions(
        &self,
        parent_id: i64,
        lock_token: &str,
        since_rfc3339: Option<&str>,
    ) -> crate::Result<Vec<crate::minions::types::ChildDoneMessage>> {
        use crate::minions::types::MinionJobStatus;

        // Same token fence as read_inbox.
        {
            let store = self
                .minion_jobs_store
                .lock()
                .expect("InMemoryEngine minion_jobs_store mutex poisoned");
            let held = store.iter().any(|j| {
                j.id == parent_id
                    && j.status == MinionJobStatus::Active
                    && j.lock_token.as_deref() == Some(lock_token)
            });
            if !held {
                return Ok(Vec::new());
            }
        }

        let inbox = self
            .minion_inbox_store
            .lock()
            .expect("InMemoryEngine minion_inbox_store mutex poisoned");
        let mut rows: Vec<&crate::minions::types::InboxMessage> = inbox
            .iter()
            .filter(|m| {
                m.job_id == parent_id
                    && m.payload.get("type").and_then(|v| v.as_str()) == Some("child_done")
                    && since_rfc3339.is_none_or(|since| m.sent_at.as_str() > since)
            })
            .collect();
        // send order (sent_at ASC; ties broken by id for determinism).
        rows.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then(a.id.cmp(&b.id)));
        Ok(rows
            .into_iter()
            .filter_map(|m| serde_json::from_value(m.payload.clone()).ok())
            .collect())
    }

    async fn update_tokens(
        &self,
        id: i64,
        lock_token: &str,
        tokens: &crate::minions::types::TokenUpdate,
    ) -> crate::Result<bool> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && j.status == MinionJobStatus::Active
                && j.lock_token.as_deref() == Some(lock_token)
        }) else {
            return Ok(false);
        };
        job.tokens_input += tokens.input.unwrap_or(0);
        job.tokens_output += tokens.output.unwrap_or(0);
        job.tokens_cache_read += tokens.cache_read.unwrap_or(0);
        job.updated_at = crate::time::current_utc_iso8601();
        Ok(true)
    }

    async fn update_progress(
        &self,
        id: i64,
        lock_token: &str,
        progress: &serde_json::Value,
    ) -> crate::Result<bool> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && j.status == MinionJobStatus::Active
                && j.lock_token.as_deref() == Some(lock_token)
        }) else {
            return Ok(false);
        };
        job.progress = Some(progress.clone());
        job.updated_at = crate::time::current_utc_iso8601();
        Ok(true)
    }

    async fn append_log(&self, id: i64, lock_token: &str, entry: &str) -> crate::Result<bool> {
        use crate::minions::types::MinionJobStatus;

        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");

        let Some(job) = store.iter_mut().find(|j| {
            j.id == id
                && j.status == MinionJobStatus::Active
                && j.lock_token.as_deref() == Some(lock_token)
        }) else {
            return Ok(false);
        };
        job.stacktrace.push(entry.to_string());
        job.updated_at = crate::time::current_utc_iso8601();
        Ok(true)
    }

    async fn is_job_active(&self, id: i64, lock_token: &str) -> crate::Result<bool> {
        use crate::minions::types::MinionJobStatus;

        let store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");
        Ok(store.iter().any(|j| {
            j.id == id
                && j.status == MinionJobStatus::Active
                && j.lock_token.as_deref() == Some(lock_token)
        }))
    }

    async fn remove_child_dependency(&self, child_id: i64) -> crate::Result<()> {
        let mut store = self
            .minion_jobs_store
            .lock()
            .expect("InMemoryEngine minion_jobs_store mutex poisoned");
        if let Some(job) = store.iter_mut().find(|j| j.id == child_id) {
            job.parent_job_id = None;
            job.updated_at = crate::time::current_utc_iso8601();
        }
        Ok(())
    }

    // ─── Minion attachments (1-1-3-2) ────────────────────────────────────────

    async fn insert_attachment(
        &self,
        job_id: i64,
        att: &crate::minions::types::NormalizedAttachment,
    ) -> crate::Result<crate::minions::types::Attachment> {
        // Verify the parent job exists (mirrors the explicit SELECT in TS
        // addAttachment; the DB FK would also enforce this).
        {
            let jobs = self
                .minion_jobs_store
                .lock()
                .expect("InMemoryEngine minion_jobs_store mutex poisoned");
            if !jobs.iter().any(|j| j.id == job_id) {
                return Err(crate::error::StructuredError::new(
                    "NotFound",
                    "not_found",
                    format!("job {job_id} not found"),
                ));
            }
        }

        let mut store = self
            .minion_attachments_store
            .lock()
            .expect("InMemoryEngine minion_attachments_store mutex poisoned");

        // Authoritative duplicate fence mirroring UNIQUE(job_id, filename).
        if store
            .iter()
            .any(|a| a.meta.job_id == job_id && a.meta.filename == att.filename)
        {
            return Err(crate::error::StructuredError::new(
                "Conflict",
                "conflict",
                format!(
                    "duplicate attachment: job {job_id} already has filename {}",
                    att.filename
                ),
            ));
        }

        let mut next = self
            .next_attachment_id
            .lock()
            .expect("InMemoryEngine next_attachment_id mutex poisoned");
        let meta = crate::minions::types::Attachment {
            id: *next,
            job_id,
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            // Faithful to the TS port: inline content only, storage_uri unused.
            // External-storage path registered in docs/plans/KNOWN-GAPS.md (G27).
            storage_uri: None,
            size_bytes: att.size_bytes,
            sha256: att.sha256.clone(),
            created_at: crate::time::current_utc_iso8601(),
        };
        *next += 1;
        store.push(InternalAttachment {
            meta: meta.clone(),
            bytes: att.bytes.clone(),
        });
        Ok(meta)
    }

    async fn list_attachment_filenames(&self, job_id: i64) -> crate::Result<Vec<String>> {
        let store = self
            .minion_attachments_store
            .lock()
            .expect("InMemoryEngine minion_attachments_store mutex poisoned");
        Ok(store
            .iter()
            .filter(|a| a.meta.job_id == job_id)
            .map(|a| a.meta.filename.clone())
            .collect())
    }

    async fn list_attachments(
        &self,
        job_id: i64,
    ) -> crate::Result<Vec<crate::minions::types::Attachment>> {
        let store = self
            .minion_attachments_store
            .lock()
            .expect("InMemoryEngine minion_attachments_store mutex poisoned");
        let mut out: Vec<crate::minions::types::Attachment> = store
            .iter()
            .filter(|a| a.meta.job_id == job_id)
            .map(|a| a.meta.clone())
            .collect();
        // ORDER BY created_at ASC, id ASC. Insertion order tracks created_at, but
        // sort by id for a stable tiebreak matching the SQL contract.
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    async fn get_attachment(
        &self,
        job_id: i64,
        filename: &str,
    ) -> crate::Result<Option<(crate::minions::types::Attachment, Vec<u8>)>> {
        let store = self
            .minion_attachments_store
            .lock()
            .expect("InMemoryEngine minion_attachments_store mutex poisoned");
        Ok(store
            .iter()
            .find(|a| a.meta.job_id == job_id && a.meta.filename == filename)
            .map(|a| (a.meta.clone(), a.bytes.clone())))
    }

    async fn delete_attachment(&self, job_id: i64, filename: &str) -> crate::Result<bool> {
        let mut store = self
            .minion_attachments_store
            .lock()
            .expect("InMemoryEngine minion_attachments_store mutex poisoned");
        let before = store.len();
        store.retain(|a| !(a.meta.job_id == job_id && a.meta.filename == filename));
        Ok(store.len() != before)
    }

    async fn upsert_chunks(
        &self,
        slug: &str,
        chunks: &[crate::import::ChunkInput],
    ) -> crate::Result<()> {
        if let Some(error) = self
            .chunk_upsert_error
            .lock()
            .expect("InMemoryEngine chunk_upsert_error mutex poisoned")
            .clone()
        {
            return Err(error);
        }

        let mut store = self.chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");
        
        // 直接存储 ChunkInput（InMemory 简化版，不转 ChunkOutput）
        let chunk_inputs: Vec<crate::import::ChunkInput> = chunks.iter().cloned().collect();
        store.insert(slug.to_string(), chunk_inputs);
        Ok(())
    }

    async fn delete_chunks(&self, slug: &str) -> crate::Result<()> {
        let mut store = self.chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");
        store.remove(slug);
        Ok(())
    }

    async fn get_chunks_for_page(&self, slug: &str) -> crate::Result<Vec<crate::import::ChunkInput>> {
        let store = self.chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");
        let mut chunks = store.get(slug).cloned().unwrap_or_default();
        chunks.sort_by_key(|chunk| chunk.chunk_index);
        Ok(chunks)
    }

    async fn add_code_edges(
        &self,
        edges: &[crate::import::CodeEdgeInput],
    ) -> crate::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let mut store = self
            .code_edges_store
            .lock()
            .expect("InMemoryEngine code_edges_store mutex poisoned");
        let mut next_id = self
            .next_code_edge_id
            .lock()
            .expect("InMemoryEngine next_code_edge_id mutex poisoned");

        for e in edges {
            let resolved = e.to_chunk_id.is_some();
            // Mirror TS ON CONFLICT DO NOTHING: skip duplicate keys.
            let dup = store.iter().any(|row| {
                if resolved {
                    row.resolved
                        && row.from_chunk_id == e.from_chunk_id
                        && row.to_chunk_id == e.to_chunk_id
                        && row.edge_type == e.edge_type
                } else {
                    !row.resolved
                        && row.from_chunk_id == e.from_chunk_id
                        && row.to_symbol_qualified == e.to_symbol_qualified
                        && row.edge_type == e.edge_type
                }
            });
            if dup {
                continue;
            }
            let id = *next_id;
            *next_id += 1;
            store.push(InternalCodeEdge {
                id,
                from_chunk_id: e.from_chunk_id,
                to_chunk_id: e.to_chunk_id,
                from_symbol_qualified: e.from_symbol_qualified.clone(),
                to_symbol_qualified: e.to_symbol_qualified.clone(),
                edge_type: e.edge_type.clone(),
                edge_metadata: e.edge_metadata.clone(),
                source_id: e.source_id.clone(),
                resolved,
            });
        }
        Ok(())
    }

    async fn delete_code_edges_for_chunks(
        &self,
        chunk_ids: &[i64],
    ) -> crate::Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        let targets: std::collections::HashSet<i64> = chunk_ids.iter().copied().collect();
        let mut store = self
            .code_edges_store
            .lock()
            .expect("InMemoryEngine code_edges_store mutex poisoned");
        store.retain(|row| {
            // code_edges_chunk: match either endpoint.
            if targets.contains(&row.from_chunk_id) {
                return false;
            }
            if row.resolved {
                if let Some(t) = row.to_chunk_id {
                    if targets.contains(&t) {
                        return false;
                    }
                }
            }
            // code_edges_symbol has no to_chunk_id to match; only from matters
            // (already handled above).
            true
        });
        Ok(())
    }

    // ── 1-6-7-10-2: code-graph query methods (InMemory) ──────────────────

    async fn get_callers_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
        let store = self
            .code_edges_store
            .lock()
            .expect("InMemoryEngine code_edges_store mutex poisoned");
        let mut out: Vec<crate::import::CodeEdgeResult> = store
            .iter()
            .filter(|row| row.to_symbol_qualified == qualified_name)
            .filter(|row| edge_source_match_inmem(row, opts))
            .map(edge_row_to_result)
            .collect();
        apply_edge_limit(&mut out, opts.limit);
        Ok(out)
    }

    async fn get_callees_of(
        &self,
        qualified_name: &str,
        opts: &crate::import::CodeGraphQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
        let store = self
            .code_edges_store
            .lock()
            .expect("InMemoryEngine code_edges_store mutex poisoned");
        let mut out: Vec<crate::import::CodeEdgeResult> = store
            .iter()
            .filter(|row| row.from_symbol_qualified == qualified_name)
            .filter(|row| edge_source_match_inmem(row, opts))
            .map(edge_row_to_result)
            .collect();
        apply_edge_limit(&mut out, opts.limit);
        Ok(out)
    }

    async fn get_edges_by_chunk(
        &self,
        chunk_id: i64,
        opts: &crate::import::CodeEdgeByChunkOpts,
    ) -> crate::Result<Vec<crate::import::CodeEdgeResult>> {
        let store = self
            .code_edges_store
            .lock()
            .expect("InMemoryEngine code_edges_store mutex poisoned");
        let mut out: Vec<crate::import::CodeEdgeResult> = store
            .iter()
            .filter(|row| match opts.direction {
                crate::import::CodeEdgeDirection::In => {
                    row.resolved && row.to_chunk_id == Some(chunk_id)
                }
                crate::import::CodeEdgeDirection::Out => row.from_chunk_id == chunk_id,
                crate::import::CodeEdgeDirection::Both => {
                    row.from_chunk_id == chunk_id
                        || (row.resolved && row.to_chunk_id == Some(chunk_id))
                }
            })
            .filter(|row| match &opts.edge_type {
                Some(et) => &row.edge_type == et,
                None => true,
            })
            .map(edge_row_to_result)
            .collect();
        let cap = opts.limit.unwrap_or(50).min(200);
        if out.len() > cap {
            out.truncate(cap);
        }
        Ok(out)
    }

    async fn find_code_def(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeDefResult>> {
        let cap = (opts.limit.unwrap_or(20) as usize).min(500);
        let pages = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let chunks = self
            .chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");
        let mut out: Vec<crate::import::CodeDefResult> = Vec::new();
        for page in pages.iter() {
            if page.page_kind != crate::types::PageKind::Code {
                continue;
            }
            let Some(page_chunks) = chunks.get(&page.slug) else {
                continue;
            };
            for ci in page_chunks.iter() {
                let Some(st) = ci.symbol_type.as_deref() else {
                    continue;
                };
                if ci.symbol_name.as_deref() != Some(symbol) || !is_def_type(st) {
                    continue;
                }
                if let Some(lang) = &opts.language {
                    if ci.language.as_deref() != Some(lang.as_str()) {
                        continue;
                    }
                }
                out.push(crate::import::CodeDefResult {
                    slug: page.slug.clone(),
                    file: page
                        .frontmatter
                        .get("file")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    language: ci.language.clone(),
                    symbol_type: ci.symbol_type.clone(),
                    start_line: ci.start_line.map(|v| v as i64),
                    end_line: ci.end_line.map(|v| v as i64),
                    snippet: ci.chunk_text.chars().take(500).collect(),
                });
            }
        }
        /// Define sort rank for definition types (closer to definition is higher rank)
        fn def_rank(ty: Option<&str>) -> u8 {
            match ty {
                Some("type") => 4,
                Some("enum") => 5,
                Some("struct") => 6,
                Some("trait") => 7,
                Some("function") => 1,
                Some("class") => 2,
                Some("interface") => 3,
                Some("module") => 8,
                Some("contract") => 9,
                Some("table") => 10,
                Some("view") => 11,
                Some("index") => 12,
                Some("procedure") => 13,
                Some("schema") => 14,
                Some("database") => 15,
                Some("trigger") => 16,
                Some("export statement") => 17,
                _ => 0,
            }
        }
        out.sort_by(|a, b| {
            def_rank(a.symbol_type.as_deref())
                .cmp(&def_rank(b.symbol_type.as_deref()))
                .then_with(|| a.slug.cmp(&b.slug))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        if out.len() > cap {
            out.truncate(cap);
        }
        Ok(out)
    }

    async fn find_code_refs(
        &self,
        symbol: &str,
        opts: &crate::import::CodeSymbolQueryOpts,
    ) -> crate::Result<Vec<crate::import::CodeRefResult>> {
        let cap = (opts.limit.unwrap_or(50) as usize).min(500);
        let pages = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let chunks = self
            .chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");
        let needle = symbol.to_lowercase();
        let mut out: Vec<crate::import::CodeRefResult> = Vec::new();
        for page in pages.iter() {
            if page.page_kind != crate::types::PageKind::Code {
                continue;
            }
            let Some(page_chunks) = chunks.get(&page.slug) else {
                continue;
            };
            for ci in page_chunks.iter() {
                if !ci.chunk_text.to_lowercase().contains(&needle) {
                    continue;
                }
                if let Some(lang) = &opts.language {
                    if ci.language.as_deref() != Some(lang.as_str()) {
                        continue;
                    }
                }
                out.push(crate::import::CodeRefResult {
                    slug: page.slug.clone(),
                    file: page
                        .frontmatter
                        .get("file")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    language: ci.language.clone(),
                    symbol_name: ci.symbol_name.clone(),
                    symbol_type: ci.symbol_type.clone(),
                    start_line: ci.start_line.map(|v| v as i64),
                    end_line: ci.end_line.map(|v| v as i64),
                    snippet: ci.chunk_text.chars().take(500).collect(),
                });
            }
        }
        out.sort_by(|a, b| a.slug.cmp(&b.slug).then_with(|| a.start_line.cmp(&b.start_line)));
        if out.len() > cap {
            out.truncate(cap);
        }
        Ok(out)
    }

    async fn get_health(&self) -> crate::Result<crate::autopilot::brain_score::BrainHealth> {
        use crate::autopilot::brain_score::{BrainHealth, MostConnectedEntry};

        let store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let chunk_store = self.chunk_store.lock().expect("InMemoryEngine chunk_store mutex poisoned");
        let links_store = self.links_store.lock().expect("InMemoryEngine links_store mutex poisoned");

        // Live pages (not soft-deleted)
        let live_pages: Vec<&Page> = store.iter().filter(|p| p.deleted_at.is_none()).collect();
        let page_count = live_pages.len();

        // Build id→slug map for link resolution
        let id_to_slug: std::collections::HashMap<u64, &str> = live_pages
            .iter()
            .map(|p| (p.id, p.slug.as_str()))
            .collect();
        let live_ids: std::collections::HashSet<u64> = live_pages.iter().map(|p| p.id).collect();

        // ── Chunk / embedding metrics ──────────────────────────────────
        let mut total_chunks = 0usize;
        let mut missing_embeddings = 0usize;
        for chunks in chunk_store.values() {
            for c in chunks.iter() {
                total_chunks += 1;
                if c.embedding.is_none() {
                    missing_embeddings += 1;
                }
            }
        }
        let embed_coverage = if total_chunks > 0 {
            (total_chunks - missing_embeddings) as f64 / total_chunks as f64
        } else {
            1.0 // no chunks → full coverage (nothing missing)
        };

        // ── Link metrics ───────────────────────────────────────────────
        let link_count = links_store.len();
        let dead_links = links_store
            .iter()
            .filter(|l| !live_ids.contains(&l.to_page_id))
            .count();

        // Orphan pages: no inbound AND no outbound links (islanded)
        let has_inbound: std::collections::HashSet<u64> = links_store
            .iter()
            .map(|l| l.to_page_id)
            .filter(|id| live_ids.contains(id))
            .collect();
        let has_outbound: std::collections::HashSet<u64> = links_store
            .iter()
            .map(|l| l.from_page_id)
            .filter(|id| live_ids.contains(id))
            .collect();
        let orphan_pages = live_pages
            .iter()
            .filter(|p| !has_inbound.contains(&p.id) && !has_outbound.contains(&p.id))
            .count();

        // ── Timeline metrics ───────────────────────────────────────────
        // Timeline is stored as a JSON string on Page. Non-empty array = has timeline.
        let pages_with_timeline = live_pages
            .iter()
            .filter(|p| {
                if let Ok(arr) = serde_json::from_str::<serde_json::Value>(&p.timeline) {
                    arr.is_array() && !arr.as_array().unwrap().is_empty()
                } else {
                    false
                }
            })
            .count();

        // stale_pages: pages where updated_at < max timeline entry date.
        // InMemory stores timeline as JSON on the page, not a separate table
        // with created_at. Conservative: 0 stale (TS uses a separate table).
        let stale_pages = 0usize;

        // ── Entity pages (person/company) ──────────────────────────────
        let entity_pages: Vec<&Page> = live_pages
            .iter()
            .copied()
            .filter(|p| p.page_type == "person" || p.page_type == "company")
            .collect();
        let entity_count = entity_pages.len();

        // link_coverage: entity pages with ≥1 inbound link
        let link_coverage = if entity_count > 0 {
            let entities_with_inbound = entity_pages
                .iter()
                .filter(|p| has_inbound.contains(&p.id))
                .count();
            entities_with_inbound as f64 / entity_count as f64
        } else {
            0.0
        };

        // timeline_coverage: entity pages with timeline
        let timeline_coverage = if entity_count > 0 {
            let entities_with_timeline = entity_pages
                .iter()
                .filter(|p| {
                    if let Ok(arr) = serde_json::from_str::<serde_json::Value>(&p.timeline) {
                        arr.is_array() && !arr.as_array().unwrap().is_empty()
                    } else {
                        false
                    }
                })
                .count();
            entities_with_timeline as f64 / entity_count as f64
        } else {
            0.0
        };

        // ── most_connected: top 5 entities by total link count ─────────
        let mut link_counts: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
        for l in links_store.iter() {
            if live_ids.contains(&l.from_page_id) {
                *link_counts.entry(l.from_page_id).or_insert(0) += 1;
            }
            if live_ids.contains(&l.to_page_id) {
                *link_counts.entry(l.to_page_id).or_insert(0) += 1;
            }
        }
        let mut connected: Vec<(u64, usize)> = entity_pages
            .iter()
            .filter_map(|p| link_counts.get(&p.id).map(|&c| (p.id, c)))
            .collect();
        connected.sort_by(|a, b| b.1.cmp(&a.1));
        let most_connected: Vec<MostConnectedEntry> = connected
            .iter()
            .take(5)
            .filter_map(|(id, count)| {
                id_to_slug.get(id).map(|slug| MostConnectedEntry {
                    slug: slug.to_string(),
                    link_count: *count,
                })
            })
            .collect();

        // ── Score computation ──────────────────────────────────────────
        // v0.37.10.0: empty brains (pageCount === 0) get FULL marks (100/100)
        let (embed_coverage_score, link_density_score, timeline_coverage_score,
             no_orphans_score, no_dead_links_score) = if page_count == 0 {
            (35u32, 25u32, 15u32, 15u32, 10u32)
        } else {
            let link_density = (link_count as f64 / page_count as f64).min(1.0);
            let timeline_density = (pages_with_timeline as f64 / page_count as f64).min(1.0);
            let no_orphans = 1.0 - (orphan_pages as f64 / page_count as f64);
            let no_dead = 1.0 - (dead_links as f64 / page_count as f64).min(1.0);
            (
                (embed_coverage * 35.0).round() as u32,
                (link_density * 25.0).round() as u32,
                (timeline_density * 15.0).round() as u32,
                (no_orphans * 15.0).round() as u32,
                (no_dead * 10.0).round() as u32,
            )
        };
        let brain_score = BrainHealth::compute_brain_score(
            embed_coverage_score,
            link_density_score,
            timeline_coverage_score,
            no_orphans_score,
            no_dead_links_score,
        );

        Ok(BrainHealth {
            page_count,
            embed_coverage,
            stale_pages,
            orphan_pages,
            missing_embeddings,
            brain_score,
            dead_links,
            link_coverage,
            timeline_coverage,
            most_connected,
            embed_coverage_score,
            link_density_score,
            timeline_coverage_score,
            no_orphans_score,
            no_dead_links_score,
        })
    }

    async fn get_brain_stats(&self) -> crate::Result<crate::admin_queries::BrainStats> {
        use crate::admin_queries::BrainStats;

        let store = self.store.lock().expect("InMemoryEngine store mutex poisoned");
        let chunk_store = self
            .chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");
        let links_store = self
            .links_store
            .lock()
            .expect("InMemoryEngine links_store mutex poisoned");

        let live_pages: Vec<&Page> = store.iter().filter(|p| p.deleted_at.is_none()).collect();
        let page_count = live_pages.len() as i64;

        // InMemory has a real chunk store, so it counts actual chunks and
        // actual per-chunk embeddings (higher fidelity than the libsql/postgres
        // page-level proxy — see BrainStats docs).
        let mut chunk_count = 0i64;
        let mut embedded_count = 0i64;
        for chunks in chunk_store.values() {
            for c in chunks.iter() {
                chunk_count += 1;
                if c.embedding.is_some() {
                    embedded_count += 1;
                }
            }
        }

        let link_count = links_store.len() as i64;

        // Distinct tags across live pages (tags live in frontmatter).
        let mut tags: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in &live_pages {
            for t in page_tags(p) {
                tags.insert(t);
            }
        }
        let tag_count = tags.len() as i64;

        // timeline is a JSON-array string per page → sum of array lengths.
        let mut timeline_entry_count = 0i64;
        for p in &live_pages {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&p.timeline) {
                timeline_entry_count += arr.len() as i64;
            }
        }

        // pages_by_type mirrors TS: grouped over ALL pages (no soft-delete
        // filter), unlike page_count which excludes soft-deleted.
        let mut pages_by_type: std::collections::BTreeMap<String, i64> =
            std::collections::BTreeMap::new();
        for p in store.iter() {
            *pages_by_type.entry(p.page_type.clone()).or_insert(0) += 1;
        }

        Ok(BrainStats {
            page_count,
            chunk_count,
            embedded_count,
            link_count,
            tag_count,
            timeline_entry_count,
            pages_by_type,
        })
    }

    // The code-graph related methods: 1-6-7-10-4 disambiguate_symbol and 1-6-7-10-5 recursive_walk
    async fn disambiguate_symbol(
        &self,
        bare: &str,
        source_id: &str,
    ) -> crate::Result<crate::import::SymbolDisambiguation> {
        // Scope chunks by source: chunk_store is keyed by slug, so resolve each
        // chunk's owning source via its page before applying the rules (mirrors
        // the libsql JOIN pages p ON p.id = cc.page_id WHERE p.source_id = ?1).
        let slug_source: std::collections::HashMap<String, String> = {
            let pages = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");
            pages
                .iter()
                .filter(|p| p.deleted_at.is_none())
                .map(|p| (p.slug.clone(), p.source_id.clone()))
                .collect()
        };

        let chunks = self
            .chunk_store
            .lock()
            .expect("InMemoryEngine chunk_store mutex poisoned");

        let lower_bare = bare.to_lowercase();
        let mut matches: Vec<String> = Vec::new();
        let mut suggestions: Vec<String> = Vec::new();

        for (slug, chs) in chunks.iter() {
            let src = slug_source.get(slug).map(|s| s.as_str()).unwrap_or("");
            if src != source_id {
                continue;
            }
            for ch in chs {
                let sym = ch.symbol_name.as_deref().unwrap_or("");
                let qual = ch.symbol_name_qualified.as_deref().unwrap_or("");
                if qual.is_empty() {
                    // mirrors `symbol_name_qualified IS NOT NULL`
                    continue;
                }
                // Exact: symbol_name = bare OR symbol_name_qualified = bare.
                let is_exact = sym == bare || qual == bare;
                if is_exact {
                    if !matches.contains(&qual.to_string()) {
                        matches.push(qual.to_string());
                    }
                } else if qual.to_lowercase().contains(&lower_bare) {
                    // Fuzzy did-you-mean suggestion (ILIKE '%bare%'); exact
                    // hits are excluded from suggestions, matching libsql.
                    if !suggestions.contains(&qual.to_string()) {
                        suggestions.push(qual.to_string());
                    }
                }
            }
        }

        matches.sort();
        suggestions.sort();
        matches.truncate(25);
        suggestions.truncate(5);

        Ok(crate::import::SymbolDisambiguation {
            matches,
            suggestions,
        })
    }

    async fn recursive_walk(
            &self,
            symbol: &str,
            opts: &crate::import::RecursiveWalkOpts,
        ) -> crate::Result<crate::import::RecursiveWalkResult> {
            use crate::import::{
                DepthGroup, RecursiveWalkResult, RecursiveWalkNode, WalkDirection, WalkFreshness,
                WalkTruncation,
            };
            use std::collections::HashSet;

            let depth_cap = opts.depth_cap.unwrap_or(match opts.direction {
                WalkDirection::Callers => 5,
                WalkDirection::Callees => 8,
            });
            let max_nodes = opts.max_nodes.unwrap_or(200);
            let source_id = opts.source_id.as_str();

            // Step 1: disambiguate starting symbol if not exact
            let (qualified_start, start_chunk_id, start_lang) = if opts.exact.unwrap_or_default() {
                (symbol.to_string(), None::<i64>, None::<String>)
            } else {
                let disambig = self.disambiguate_symbol(symbol, source_id).await?;
                if disambig.matches.is_empty() {
                    // No exact matches → convert suggestions to did_you_mean candidates
                    let did_you_mean = disambig.suggestions
                        .into_iter()
                        .map(|s| crate::import::DidYouMeanCandidate {
                            symbol_qualified: s,
                            score: 1.0,
                        })
                        .collect();
                    return Ok(RecursiveWalkResult::NotFound { did_you_mean });
                }
                if disambig.matches.len() > 1 {
                    // Multiple exact matches → ambiguous
                    let candidates = disambig.matches
                        .into_iter()
                        .map(|s| crate::import::AmbiguousCandidate {
                            symbol_qualified: s,
                            lang: None,
                            file: None,
                            lines: None,
                        })
                        .collect();
                    return Ok(RecursiveWalkResult::Ambiguous { candidates });
                }
                // Single exact match → resolved
                let only = disambig.matches.first().unwrap();
                (only.clone(), None, None)
            };

            // Step 2: get language for starting symbol from its chunk
            let start_lang: Option<String> = if let Some(_chunk_id) = start_chunk_id {
                // TODO: get chunk page to get language from page
                // For in-memory, we don't have chunk -> page mapping yet, so leave as None
                None
            } else {
                None
            };

            // Check if starting symbol exists in any case
            let mut visited = HashSet::new();
            visited.insert(qualified_start.clone());

            let mut total_nodes = 1;
            let mut cycles_detected = false;
            let mut truncation = WalkTruncation::None;
            let mut terminal_nodes = Vec::new();
            let mut depth_groups = Vec::new();

            let mut current_depth = 0;
            let mut frontier = Vec::new();
            frontier.push(qualified_start.clone());

            while !frontier.is_empty() && current_depth < depth_cap {
                let mut next_frontier = Vec::new();
                let mut nodes_this_depth = Vec::new();

                'frontier_loop: for sym in frontier.iter() {
                    // Get edges using existing code-edge query: for Callers we use get_callers_of, for Callees get_callees_of
                    // Note: we don't apply a limit here because we need to detect truncation
                    // (total_nodes >= max_nodes) inside the loop. The limit would prevent
                    // us from seeing enough edges to trigger the check.
                    let (edges_result, next_sym_extractor): (
                        crate::Result<Vec<crate::import::CodeEdgeResult>>,
                        Box<dyn Fn(&crate::import::CodeEdgeResult) -> Option<&String> + Send + Sync>,
                    ) = match opts.direction {
                        WalkDirection::Callers => {
                            // callers of sym = who calls sym → next is from_symbol_qualified
                            let res = self.get_callers_of(sym, &crate::import::CodeGraphQueryOpts {
                                source_id: Some(source_id.to_string()),
                                all_sources: false,
                                limit: None,
                                ..Default::default()
                            }).await;
                            (res, Box::new(|e| Some(&e.from_symbol_qualified)))
                        }
                        WalkDirection::Callees => {
                            // callees of sym = whom sym calls → next is to_symbol_qualified
                            let res = self.get_callees_of(sym, &crate::import::CodeGraphQueryOpts {
                                source_id: Some(source_id.to_string()),
                                all_sources: false,
                                limit: None,
                                ..Default::default()
                            }).await;
                            (res, Box::new(|e| Some(&e.to_symbol_qualified)))
                        }
                    };

                    let mut edges = match edges_result {
                        Ok(edges) => edges,
                        Err(e) => return Err(e),
                    };

                    edges.retain(|e| edge_source_match(e, &crate::import::CodeGraphQueryOpts {
                        source_id: Some(source_id.to_string()),
                        all_sources: false,
                        ..Default::default()
                    }));

                    for e in edges {
                        let next_sym = next_sym_extractor(&e);
                        let Some(next_sym_str) = next_sym else {
                            continue;
                        };
                        if next_sym_str == sym {
                            continue; // self-loop skip
                        }
                        if visited.contains(next_sym_str) {
                            cycles_detected = true;
                            continue;
                        }
                        if total_nodes >= max_nodes {
                            truncation = match truncation {
                                WalkTruncation::None => WalkTruncation::MaxNodes,
                                WalkTruncation::DepthCap => WalkTruncation::Both,
                                _ => truncation,
                            };
                            break 'frontier_loop;
                        }
                        visited.insert(next_sym_str.clone());
                        total_nodes += 1;

                        let from_chunk_id = match opts.direction {
                            WalkDirection::Callers => e.from_chunk_id,
                            WalkDirection::Callees => e.from_chunk_id,
                        };

                        let mut node = RecursiveWalkNode {
                            symbol: next_sym_str.clone(),
                            chunk_id: Some(from_chunk_id),
                            sink_kind: None,
                        };

                        // classify sink for callees direction when we have start language
                        if matches!(opts.direction, WalkDirection::Callees) && start_lang.is_some() {
                            if let Some(kind) = crate::code_intel::classify_sink(
                                next_sym_str,
                                start_lang.as_deref().unwrap_or(""),
                            ) {
                                node.sink_kind = Some(kind.as_str().to_string());
                                terminal_nodes.push(crate::import::TerminalNode {
                                    symbol: next_sym_str.clone(),
                                    sink_kind: kind.as_str().to_string(),
                                });
                            }
                        }

                        nodes_this_depth.push(node);
                        next_frontier.push(next_sym_str.clone());
                    }
                }

                if !nodes_this_depth.is_empty() {
                    let confidence = crate::engine::clamp_confidence(current_depth + 1);
                    depth_groups.push(DepthGroup {
                        depth: current_depth + 1,
                        nodes: nodes_this_depth,
                        confidence,
                    });
                }

                frontier = next_frontier;
                current_depth += 1;

                if current_depth >= depth_cap && !frontier.is_empty() {
                    truncation = match truncation {
                        WalkTruncation::None => WalkTruncation::DepthCap,
                        WalkTruncation::MaxNodes => WalkTruncation::Both,
                        _ => truncation,
                    };
                }
            }

            Ok(RecursiveWalkResult::Ok {
                depth_groups,
                cycles_detected,
                truncation,
                freshness: WalkFreshness::Fresh,
                terminal_nodes: Some(terminal_nodes),
            })
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PageKind;

    fn test_page(slug: &str, title: &str, content: &str) -> Page {
        Page {
            id: 0,
            slug: slug.to_string(),
            page_type: "note".to_string(),
            page_kind: PageKind::Markdown,
            title: title.to_string(),
            compiled_truth: content.to_string(),
            timeline: "[]".to_string(),
            frontmatter: serde_json::json!({}),
            content_hash: None,
            emotional_weight: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
            last_retrieved_at: None,
            effective_date: None,
            effective_date_source: None,
            import_filename: None,
            salience_touched_at: None,
            salience_score: None,
            generation: 1,
            embedding: None,
            chunker_version: 1,
            source_path: None,
            source_id: "default".to_string(),
            source_kind: None,
            source_uri: None,
            ingested_via: None,
            ingested_at: None,
            contextual_retrieval_mode: Some(CRMode::None),
            corpus_generation: None,
        }
    }

    #[tokio::test]
    async fn config_get_set_unset_roundtrip() {
        let engine = InMemoryEngine::new();
        assert!(engine.get_config("k").await.unwrap().is_none());
        engine.set_config("k", "v").await.unwrap();
        assert_eq!(engine.get_config("k").await.unwrap().as_deref(), Some("v"));
        assert_eq!(engine.unset_config("k").await.unwrap(), 1);
        assert!(engine.get_config("k").await.unwrap().is_none());
        // unset a missing key → 0 affected rows
        assert_eq!(engine.unset_config("missing").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn collect_child_put_page_slugs_reads_executions() {
        let engine = InMemoryEngine::new();
        // The write path (minion `brain_put_page` tool) is a tracked KNOWN-GAP;
        // inject rows directly into the private store to exercise the read path.
        {
            let mut store = engine
                .subagent_tool_executions_store
                .lock()
                .expect("poisoned");
            store.push(InternalSubagentToolExecution {
                job_id: 1,
                tool_name: "brain_put_page".to_string(),
                status: "complete".to_string(),
                input: serde_json::json!({ "slug": "a" }),
            });
            store.push(InternalSubagentToolExecution {
                job_id: 1,
                tool_name: "brain_put_page".to_string(),
                status: "complete".to_string(),
                input: serde_json::json!({ "input": { "slug": "b" } }),
            });
            // incomplete status → excluded
            store.push(InternalSubagentToolExecution {
                job_id: 2,
                tool_name: "brain_put_page".to_string(),
                status: "pending".to_string(),
                input: serde_json::json!({ "slug": "c" }),
            });
            // wrong tool → excluded
            store.push(InternalSubagentToolExecution {
                job_id: 9,
                tool_name: "other_tool".to_string(),
                status: "complete".to_string(),
                input: serde_json::json!({ "slug": "d" }),
            });
        }
        let mut out = engine.collect_child_put_page_slugs(&[1]).await.unwrap();
        out.sort();
        assert_eq!(
            out,
            vec![
                ("a".to_string(), "default".to_string()),
                ("b".to_string(), "default".to_string()),
            ]
        );
        // empty child_ids → empty
        assert!(engine.collect_child_put_page_slugs(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_pages_finds_keyword_in_title() {
        let engine = InMemoryEngine::default();
        // Put a page with known content
        engine.put_page(
            "rust-guide",
            Some("default"),
            &PageInput {
                page_type: "guide".to_string(),
                title: "Rust Programming Guide".to_string(),
                compiled_truth: "Learn Rust with examples.".to_string(),
                timeline: None,
                frontmatter: None,
                content_hash: None,
                page_kind: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                chunker_version: None,
                source_path: None,
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                last_retrieved_at: None,
                embedding: None,
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.slug, "rust-guide");
        assert!(results[0].score > 0.0);
    }

    #[tokio::test]
    async fn search_pages_finds_keyword_in_content() {
        let engine = InMemoryEngine::default();
        engine.put_page(
            "ai-notes",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Random Notes".to_string(),
                compiled_truth: "Contains discussion about machine learning and AI.".to_string(),
                timeline: None,
                frontmatter: None,
                content_hash: None,
                page_kind: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                chunker_version: None,
                source_path: None,
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                last_retrieved_at: None,
                embedding: None,
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["machine learning".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.slug, "ai-notes");
    }

    #[tokio::test]
    async fn search_pages_returns_empty_for_no_match() {
        let engine = InMemoryEngine::default();
        engine.put_page(
            "page1",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Test Page".to_string(),
                compiled_truth: "Some content here.".to_string(),
                timeline: None,
                frontmatter: None,
                content_hash: None,
                page_kind: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                chunker_version: None,
                source_path: None,
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                last_retrieved_at: None,
                embedding: None,
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["xyznotfound".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn search_pages_respects_limit() {
        let engine = InMemoryEngine::default();
        for i in 1..=5 {
            engine.put_page(
                &format!("page-{i}"),
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: format!("Rust Page {i}").to_string(),
                    compiled_truth: format!("Content about Rust {i}").to_string(),
                    timeline: None,
                    frontmatter: None,
                    content_hash: None,
                    page_kind: None,
                    effective_date: None,
                    effective_date_source: None,
                    import_filename: None,
                    chunker_version: None,
                    source_path: None,
                    source_kind: None,
                    source_uri: None,
                    ingested_via: None,
                    ingested_at: None,
                    last_retrieved_at: None,
                    embedding: None,
                },
            ).await.unwrap();
        }

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: Some(2),
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn search_pages_filters_by_source() {
        let engine = InMemoryEngine::default();
        engine.put_page(
            "page-1",
            Some("source-a"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Rust in Source A".to_string(),
                compiled_truth: "Content".to_string(),
                timeline: None,
                frontmatter: None,
                content_hash: None,
                page_kind: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                chunker_version: None,
                source_path: None,
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                last_retrieved_at: None,
                embedding: None,
            },
        ).await.unwrap();
        engine.put_page(
            "page-2",
            Some("source-b"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Rust in Source B".to_string(),
                compiled_truth: "Content".to_string(),
                timeline: None,
                frontmatter: None,
                content_hash: None,
                page_kind: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                chunker_version: None,
                source_path: None,
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                last_retrieved_at: None,
                embedding: None,
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: Some("source-a".to_string()),
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.source_id, "source-a");
    }

    #[tokio::test]
    async fn search_pages_sorts_by_score() {
        let engine = InMemoryEngine::default();
        // Page 1: Rust in title AND content (higher score)
        engine.put_page(
            "high-score",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Rust Guide".to_string(),  // title match
                compiled_truth: "Rust is a systems language.".to_string(), // content match
                timeline: None,
                frontmatter: None,
                content_hash: None,
                page_kind: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                chunker_version: None,
                source_path: None,
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                last_retrieved_at: None,
                embedding: None,
            },
        ).await.unwrap();
        // Page 2: Rust in content only (lower score)
        engine.put_page(
            "low-score",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Other Title".to_string(),
                compiled_truth: "Has Rust content.".to_string(), // content match only
                timeline: None,
                frontmatter: None,
                content_hash: None,
                page_kind: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                chunker_version: None,
                source_path: None,
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                last_retrieved_at: None,
                embedding: None,
            },
        ).await.unwrap();

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 2);
        // Higher score page should come first
        assert!(results[0].score > results[1].score);
    }

    /// Salience boost tracer bullet: a page with a higher `emotional_weight`
    /// gets its fused score multiplied by `1 + 0.15*ln(1 + salience_score)`
    /// (salience_score = emotional_weight * 5 in InMemory), reordering it above
    /// an equally-matched page with no emotional weight, and the applied factor
    /// is stamped onto `salience_boost`. Mirrors TS `applySalienceBoost`
    /// (src/core/search/hybrid.ts:153, strength 'on' => k=0.15).
    #[tokio::test]
    async fn search_pages_salience_boost_reorders_and_stamps() {
        let engine = InMemoryEngine::default();
        // Two pages with the IDENTICAL lexical match profile (keyword in
        // content only) so their fused base_score is equal and salience is the
        // sole tie-breaker. The `concepts/` prefix is evergreen in
        // DEFAULT_RECENCY_DECAY (halflife 0), so the always-on recency stage is
        // a no-op here and can't perturb the exact salience-factor assertions.
        for slug in ["concepts/salient", "concepts/plain"] {
            engine.put_page(
                slug,
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: "Untitled".to_string(),
                    compiled_truth: "Has rust content.".to_string(),
                    timeline: None,
                    frontmatter: None,
                    content_hash: None,
                    page_kind: None,
                    effective_date: None,
                    effective_date_source: None,
                    import_filename: None,
                    chunker_version: None,
                    source_path: None,
                    source_kind: None,
                    source_uri: None,
                    ingested_via: None,
                    ingested_at: None,
                    last_retrieved_at: None,
                    embedding: None,
                },
            ).await.unwrap();
        }
        // salient: emotional_weight 2.0 => salience_score = 10.0
        engine.set_emotional_weight_for_tests("concepts/salient", "default", 2.0);
        // plain: leave emotional_weight None => salience_score 0 => no boost

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 2);
        // salient must now rank first (base scores equal, salience lifts it).
        assert_eq!(results[0].page.slug, "concepts/salient");
        assert_eq!(results[1].page.slug, "concepts/plain");

        // factor = 1 + 0.15 * ln(1 + 10.0)
        let expected_factor = 1.0 + 0.15 * (1.0_f64 + 10.0).ln();
        let stamp = results[0].salience_boost.expect("salient row stamped");
        assert!(
            (stamp - expected_factor).abs() < 1e-9,
            "salience_boost stamp {stamp} != expected {expected_factor}"
        );
        // score = base_score * factor (base_score preserved as the pre-boost value).
        assert!(
            (results[0].score - results[0].base_score * expected_factor).abs() < 1e-9,
            "boosted score must equal base_score * factor"
        );
        // plain row got no boost => stamp stays None, score == base_score.
        assert_eq!(results[1].salience_boost, None, "unboosted row not stamped");
        assert!((results[1].score - results[1].base_score).abs() < 1e-9);
    }

    /// Floor-threshold gate: with `floor_ratio` set, a result whose fused score
    /// is below `topScore * floor_ratio` is SKIPPED by the salience stage (no
    /// mutation, no stamp), so a weak-overlap tail page can't leapfrog via
    /// metadata boost. Mirrors TS `computeFloorThreshold` +
    /// gate in `applySalienceBoost` (src/core/search/hybrid.ts:162).
    #[tokio::test]
    async fn search_pages_salience_boost_respects_floor_gate() {
        let engine = InMemoryEngine::default();
        // strong: keyword in title + content + frontmatter => uniquely highest
        // lexical weight (0.4+0.4+0.2), so it deterministically fuses to the
        // normalized top score 1.0.
        engine.put_page(
            "strong",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Rust Guide".to_string(),
                compiled_truth: "Rust everywhere.".to_string(),
                timeline: None,
                frontmatter: Some(serde_json::json!({ "tag": "rust" })),
                content_hash: None,
                page_kind: None, effective_date: None, effective_date_source: None,
                import_filename: None, chunker_version: None, source_path: None,
                source_kind: None, source_uri: None, ingested_via: None,
                ingested_at: None, last_retrieved_at: None, embedding: None,
            },
        ).await.unwrap();
        // Three filler pages with a title+content match (weight 0.8) so the weak
        // page is pushed down to fusion rank 4. At RRF_K=60 the normalized score
        // there is 60/64 ~= 0.9375, below the 0.95 floor, while every filler
        // (ranks 1..3 => >= 60/63 ~= 0.952) clears it.
        for slug in ["filler-a", "filler-b", "filler-c"] {
            engine.put_page(
                slug,
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: "Rust Notes".to_string(),
                    compiled_truth: "Rust content.".to_string(),
                    timeline: None, frontmatter: None, content_hash: None,
                    page_kind: None, effective_date: None, effective_date_source: None,
                    import_filename: None, chunker_version: None, source_path: None,
                    source_kind: None, source_uri: None, ingested_via: None,
                    ingested_at: None, last_retrieved_at: None, embedding: None,
                },
            ).await.unwrap();
        }
        // weak: keyword in content only => lowest lexical weight (0.4), fuses to
        // the tail, below the floor.
        engine.put_page(
            "weak",
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: "Untitled".to_string(),
                compiled_truth: "Has rust content.".to_string(),
                timeline: None, frontmatter: None, content_hash: None,
                page_kind: None, effective_date: None, effective_date_source: None,
                import_filename: None, chunker_version: None, source_path: None,
                source_kind: None, source_uri: None, ingested_via: None,
                ingested_at: None, last_retrieved_at: None, embedding: None,
            },
        ).await.unwrap();
        // Give the WEAK page a large emotional_weight — without the gate it
        // would leapfrog via salience boost.
        engine.set_emotional_weight_for_tests("weak", "default", 5.0);

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: Some(0.95),
            recency_decay: None,
            recency_fallback: None,
            ..Default::default()
        }).await.unwrap();

        let weak = results.iter().find(|r| r.page.slug == "weak").unwrap();
        // Gated out: no boost applied, stamp stays None, score == base_score.
        assert_eq!(weak.salience_boost, None, "gated row must not be stamped");
        assert!((weak.score - weak.base_score).abs() < 1e-9, "gated row score unchanged");
        // strong still ranks first (unique top lexical weight).
        assert_eq!(results[0].page.slug, "strong");
    }

    /// Recency boost wiring: two pages with the identical lexical match profile
    /// (keyword in content only) fuse to an equal base_score, so the recency
    /// axis is the sole tie-breaker. Both slugs share the `daily/` prefix
    /// (hl=14, coef=1.5); the fresh page (effective_date = now) gets the larger
    /// factor and reorders above the stale page (effective_date ~2 years old),
    /// and both rows carry a `recency_boost` stamp. Proves the engine resolves
    /// dates via get_effective_dates + DEFAULT_RECENCY_DECAY and applies the
    /// pure `apply_recency_boost` stage.
    #[tokio::test]
    async fn search_pages_recency_boost_reorders_and_stamps() {
        let engine = InMemoryEngine::default();
        for slug in ["daily/fresh", "daily/stale"] {
            engine.put_page(
                slug,
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: "Untitled".to_string(),
                    compiled_truth: "Has rust content.".to_string(),
                    timeline: None,
                    frontmatter: None,
                    content_hash: None,
                    page_kind: None,
                    effective_date: None,
                    effective_date_source: None,
                    import_filename: None,
                    chunker_version: None,
                    source_path: None,
                    source_kind: None,
                    source_uri: None,
                    ingested_via: None,
                    ingested_at: None,
                    last_retrieved_at: None,
                    embedding: None,
                },
            ).await.unwrap();
        }
        // fresh: near now => large recency factor.
        // stale: ~2 years ago => days_old >> halflife => factor near 1.0.
        engine.set_effective_date_for_tests(
            "daily/fresh",
            "default",
            &crate::time::current_utc_iso8601(),
        );
        engine.set_effective_date_for_tests("daily/stale", "default", "2024-01-01T00:00:00Z");

        let results = engine.search_pages(&SearchOpts {
            keywords: vec!["rust".to_string()],
            limit: None,
            min_score: None,
            source_id: None,
            query_embedding: None,
            floor_ratio: None,
            recency_decay: None,    // engine falls back to DEFAULT_RECENCY_DECAY
            recency_fallback: None, // engine falls back to DEFAULT_FALLBACK
            ..Default::default()
        }).await.unwrap();

        assert_eq!(results.len(), 2);
        // Fresh page reorders to the top (equal base scores, recency lifts it).
        assert_eq!(results[0].page.slug, "daily/fresh");
        assert_eq!(results[1].page.slug, "daily/stale");
        // Both rows got a recency stamp (both have a date + non-evergreen prefix).
        let fresh_factor = results[0].recency_boost.expect("fresh row stamped");
        let stale_factor = results[1].recency_boost.expect("stale row stamped");
        // Fresh (days_old ~0) approaches 1 + coef = 2.5; stale is far smaller.
        assert!(
            fresh_factor > stale_factor,
            "fresh factor {fresh_factor} must exceed stale factor {stale_factor}"
        );
        // Boosted score == base_score * factor (base_score preserved pre-boost).
        assert!(
            (results[0].score - results[0].base_score * fresh_factor).abs() < 1e-9,
            "boosted score must equal base_score * recency factor"
        );
    }

    /// Encode an f32 vector to the little-endian byte layout used by the
    /// `Page::embedding` column (mirrors the TS Voyage f32-LE decode at
    /// `src/core/ai/gateway.ts:864`).
    fn f32_le_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[tokio::test]
    async fn search_pages_finds_vector_match_without_keyword() {
        let engine = InMemoryEngine::default();
        // Page has NO lexical overlap with the query keyword ("quantum"),
        // but its stored embedding is colinear with the query embedding.
        // Lexical-only search returns nothing; the vector path must surface it.
        engine
            .put_page(
                "semantic-only",
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: "Feline companions".to_string(),
                    compiled_truth: "Domestic cats and their behaviour.".to_string(),
                    timeline: None,
                    frontmatter: None,
                    content_hash: None,
                    page_kind: None,
                    effective_date: None,
                    effective_date_source: None,
                    import_filename: None,
                    chunker_version: None,
                    source_path: None,
                    source_kind: None,
                    source_uri: None,
                    ingested_via: None,
                    ingested_at: None,
                    last_retrieved_at: None,
                    embedding: Some(f32_le_bytes(&[1.0, 0.0, 0.0])),
                },
            )
            .await
            .unwrap();

        let results = engine
            .search_pages(&SearchOpts {
                keywords: vec!["quantum".to_string()],
                limit: None,
                min_score: None,
                source_id: None,
                // Colinear with the page embedding → cosine ≈ 1.0.
                query_embedding: Some(vec![1.0, 0.0, 0.0]),
                floor_ratio: None,
                recency_decay: None,
                recency_fallback: None,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1, "vector path should surface the page");
        assert_eq!(results[0].page.slug, "semantic-only");
        assert!(
            results[0].base_score > 0.0,
            "fusion must populate base_score"
        );
    }

    // ── Facts engine tests ──────────────────────────────────────────────

    fn test_fact(
        id: i64,
        source_id: &str,
        entity_slug: &str,
        fact: &str,
        kind: FactKind,
    ) -> FactRow {
        FactRow {
            id,
            source_id: source_id.to_string(),
            entity_slug: Some(entity_slug.to_string()),
            fact: fact.to_string(),
            kind,
            visibility: FactVisibility::Private,
            notability: "medium".to_string(),
            context: None,
            valid_from: Some("2026-01-01T00:00:00Z".to_string()),
            valid_until: None,
            expired_at: None,
            superseded_by: None,
            consolidated_at: None,
            consolidated_into: None,
            source: "test".to_string(),
            source_session: None,
            confidence: 1.0,
            created_at: Some("2026-07-09T00:00:00Z".to_string()),
            row_num: None,
            source_markdown_slug: None,
        }
    }

    #[tokio::test]
    async fn insert_fact_basic() {
        let engine = InMemoryEngine::new();
        let status = engine
            .insert_fact(
                "test-source",
                "alice",
                &NewFact {
                    fact: "likes coffee".to_string(),
                    kind: Some(FactKind::Preference),
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(status, FactInsertStatus::Inserted);

        let rows = engine
            .list_facts_by_entity("test-source", "alice", &FactListOpts::default())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fact, "likes coffee");
        assert_eq!(rows[0].kind, FactKind::Preference);
    }

    #[tokio::test]
    async fn insert_fact_duplicate_detection() {
        let engine = InMemoryEngine::new();
        let new_fact = || NewFact {
            fact: "likes coffee".to_string(),
            kind: Some(FactKind::Preference),
            entity_slug: None,
            visibility: None,
            context: None,
            valid_from: None,
            valid_until: None,
            source: "test".to_string(),
            source_session: None,
            confidence: None,
            notability: None,
            claim_metric: None,
            claim_value: None,
            claim_unit: None,
            claim_period: None,
            event_type: None,
            row_num: None,
            source_markdown_slug: None,
        };

        let s1 = engine.insert_fact("test-source", "alice", &new_fact()).await.unwrap();
        assert_eq!(s1, FactInsertStatus::Inserted);

        // Same fact again → duplicate
        let s2 = engine.insert_fact("test-source", "alice", &new_fact()).await.unwrap();
        assert_eq!(s2, FactInsertStatus::Duplicate);

        // Different entity → still inserted
        let s3 = engine.insert_fact("test-source", "bob", &new_fact()).await.unwrap();
        assert_eq!(s3, FactInsertStatus::Inserted);
    }

    #[tokio::test]
    async fn insert_fact_supersede_high_confidence() {
        let engine = InMemoryEngine::new();
        // First fact: likes coffee, confidence 0.8 (below threshold)
        let s1 = engine
            .insert_fact(
                "test-source",
                "alice",
                &NewFact {
                    fact: "likes coffee".to_string(),
                    kind: Some(FactKind::Preference),
                    confidence: Some(0.8),
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(s1, FactInsertStatus::Inserted);

        // Second fact: same entity same kind, confidence 0.95 (above threshold)
        // Should supersede the first.
        let s2 = engine
            .insert_fact(
                "test-source",
                "alice",
                &NewFact {
                    fact: "loves espresso".to_string(),
                    kind: Some(FactKind::Preference),
                    confidence: Some(0.95),
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(s2, FactInsertStatus::Superseded);

        // Check that the old fact was superseded
        let rows = engine
            .list_facts_by_entity(
                "test-source",
                "alice",
                &FactListOpts {
                    active_only: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        let old = rows.iter().find(|r| r.fact == "likes coffee").unwrap();
        assert!(old.superseded_by.is_some());
        // newest fact gets the higher id (auto-increment), superseded_by points to it
        let newest_id = rows.iter().map(|r| r.id).max().unwrap();
        assert_eq!(old.superseded_by.unwrap(), newest_id);
    }

    #[tokio::test]
    async fn list_facts_active_only() {
        let engine = InMemoryEngine::new();
        // Insert two facts of different kinds (same kind+entity+high confidence
        // triggers supersede by design — see the 1-2-8 grill-me decision).
        engine
            .insert_fact(
                "test-source",
                "alice",
                &NewFact {
                    fact: "active fact".to_string(),
                    kind: Some(FactKind::Preference),
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();
        engine
            .insert_fact(
                "test-source",
                "alice",
                &NewFact {
                    fact: "expired fact".to_string(),
                    kind: Some(FactKind::Belief),
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();

        // Expire the second fact
        let expired = engine.expire_fact("test-source", 2).await.unwrap();
        assert!(expired);

        let active = engine
            .list_facts_by_entity(
                "test-source",
                "alice",
                &FactListOpts {
                    active_only: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].fact, "active fact");
    }

    #[tokio::test]
    async fn list_facts_wrong_source_returns_empty() {
        let engine = InMemoryEngine::new();
        engine
            .insert_fact(
                "source-a",
                "alice",
                &NewFact {
                    fact: "only in source-a".to_string(),
                    kind: None,
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();

        let rows = engine
            .list_facts_by_entity("source-b", "alice", &FactListOpts::default())
            .await
            .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn get_facts_health_counts() {
        let engine = InMemoryEngine::new();
        // Use different kinds to avoid supersede kick-in
        engine
            .insert_fact(
                "s1",
                "alice",
                &NewFact {
                    fact: "fact 1".to_string(),
                    kind: Some(FactKind::Event),
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();
        engine
            .insert_fact(
                "s1",
                "alice",
                &NewFact {
                    fact: "fact 2".to_string(),
                    kind: Some(FactKind::Preference),
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();

        let health = engine.get_facts_health("s1").await.unwrap();
        assert_eq!(health.source_id, "s1");
        assert_eq!(health.total_active, 2);
        assert!(health.total_expired == 0);
        assert_eq!(health.top_entities.len(), 1);
        assert_eq!(health.top_entities[0].entity_slug, "alice");
        assert_eq!(health.top_entities[0].count, 2);
    }

    #[tokio::test]
    async fn expire_fact_returns_false_for_already_expired() {
        let engine = InMemoryEngine::new();
        engine
            .insert_fact(
                "test-source",
                "alice",
                &NewFact {
                    fact: "will expire".to_string(),
                    kind: None,
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();

        let ok = engine.expire_fact("test-source", 1).await.unwrap();
        assert!(ok);

        // Second expire → false (already expired)
        let ok2 = engine.expire_fact("test-source", 1).await.unwrap();
        assert!(!ok2);
    }

    #[tokio::test]
    async fn expire_fact_wrong_source_returns_false() {
        let engine = InMemoryEngine::new();
        engine
            .insert_fact(
                "source-a",
                "alice",
                &NewFact {
                    fact: "fact".to_string(),
                    kind: None,
                    entity_slug: None,
                    visibility: None,
                    context: None,
                    valid_from: None,
                    valid_until: None,
                    source: "test".to_string(),
                    source_session: None,
                    confidence: None,
                    notability: None,
                    claim_metric: None,
                    claim_value: None,
                    claim_unit: None,
                    claim_period: None,
                    event_type: None,
                    row_num: None,
                    source_markdown_slug: None,
                },
            )
            .await
            .unwrap();

        let ok = engine.expire_fact("source-b", 1).await.unwrap();
        assert!(!ok);
    }

    // ─── Minion job queue (Phase 9, slice 1-1-1 A+B) ────────────────────────
    //
    // These exercise the trait contract through the InMemoryEngine — the
    // backend-blind half of the queue. The libsql/postgres backends replay the
    // same behaviors in their own integration suites.

    use crate::minions::types::{
        BackoffType, FailOutcome, JobFilters, MinionJobInput, MinionJobStatus,
    };

    fn job_input(name: &str) -> MinionJobInput {
        MinionJobInput {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn enqueue_job_applies_schema_defaults() {
        // new() seeds the id counter at 1 (Default derive would start at 0).
        let engine = InMemoryEngine::new();
        let job = engine.enqueue_job(&job_input("build")).await.unwrap();

        assert_eq!(job.id, 1);
        assert_eq!(job.name, "build");
        assert_eq!(job.queue, "default");
        assert_eq!(job.status, MinionJobStatus::Waiting);
        assert_eq!(job.priority, 0);
        assert_eq!(job.data, serde_json::json!({}));
        assert_eq!(job.max_attempts, 3);
        assert_eq!(job.attempts_made, 0);
        assert_eq!(job.attempts_started, 0);
        assert_eq!(job.backoff_type, BackoffType::Exponential);
        assert_eq!(job.backoff_delay, 1000);
        assert!((job.backoff_jitter - 0.2).abs() < 1e-9);
        assert_eq!(job.max_stalled, 5);
        assert!(job.lock_token.is_none());
        assert!(job.lock_until.is_none());
        assert!(job.delay_until.is_none());
        assert!(!job.remove_on_complete);
        assert!(!job.remove_on_fail);
    }

    #[tokio::test]
    async fn enqueue_job_delay_sets_delayed_status() {
        let engine = InMemoryEngine::default();
        let before = crate::time::now_epoch_ms();
        let input = MinionJobInput {
            delay: Some(60_000),
            ..job_input("later")
        };
        let job = engine.enqueue_job(&input).await.unwrap();

        assert_eq!(job.status, MinionJobStatus::Delayed);
        let due = job.delay_until.expect("delayed job has delay_until");
        assert!(due >= before + 60_000, "delay_until must be ~now + delay");
    }

    #[tokio::test]
    async fn enqueue_job_clamps_max_stalled() {
        let engine = InMemoryEngine::default();
        let high = engine
            .enqueue_job(&MinionJobInput {
                max_stalled: Some(999),
                ..job_input("a")
            })
            .await
            .unwrap();
        assert_eq!(high.max_stalled, 100);

        let low = engine
            .enqueue_job(&MinionJobInput {
                max_stalled: Some(0),
                ..job_input("b")
            })
            .await
            .unwrap();
        assert_eq!(low.max_stalled, 1);
    }

    #[tokio::test]
    async fn enqueue_job_idempotency_returns_existing_row() {
        let engine = InMemoryEngine::default();
        let first = engine
            .enqueue_job(&MinionJobInput {
                idempotency_key: Some("dedup-key".to_string()),
                ..job_input("once")
            })
            .await
            .unwrap();
        let second = engine
            .enqueue_job(&MinionJobInput {
                idempotency_key: Some("dedup-key".to_string()),
                ..job_input("once")
            })
            .await
            .unwrap();

        assert_eq!(first.id, second.id, "same key returns the same row");
        let all = engine.get_jobs(&JobFilters::default()).await.unwrap();
        assert_eq!(all.len(), 1, "no second row inserted");
    }

    #[tokio::test]
    async fn get_job_returns_none_for_missing() {
        let engine = InMemoryEngine::default();
        assert!(engine.get_job(999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_jobs_filters_and_orders_newest_first() {
        // new() seeds the id counter at 1 so the id assertions below are stable.
        let engine = InMemoryEngine::new();
        engine.enqueue_job(&job_input("a")).await.unwrap();
        engine.enqueue_job(&job_input("b")).await.unwrap();
        engine.enqueue_job(&job_input("a")).await.unwrap();

        // Filter by name.
        let only_a = engine
            .get_jobs(&JobFilters {
                name: Some("a".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(only_a.len(), 2);
        // Newest first: id 3 before id 1.
        assert_eq!(only_a[0].id, 3);
        assert_eq!(only_a[1].id, 1);

        // Limit + offset.
        let page = engine
            .get_jobs(&JobFilters {
                limit: Some(1),
                offset: Some(1),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, 2, "offset 1 over [3,2,1] -> id 2");
    }

    #[tokio::test]
    async fn claim_job_is_exclusive_and_priority_ordered() {
        let engine = InMemoryEngine::default();
        // Higher priority number = later; lower runs first.
        engine
            .enqueue_job(&MinionJobInput {
                priority: Some(5),
                ..job_input("worker")
            })
            .await
            .unwrap();
        let hot = engine
            .enqueue_job(&MinionJobInput {
                priority: Some(0),
                ..job_input("worker")
            })
            .await
            .unwrap();

        let names = vec!["worker".to_string()];
        let claimed = engine
            .claim_job("tok-1", 30_000, "default", &names)
            .await
            .unwrap()
            .expect("a waiting job is claimable");
        assert_eq!(claimed.id, hot.id, "priority 0 claimed before priority 5");
        assert_eq!(claimed.status, MinionJobStatus::Active);
        assert_eq!(claimed.lock_token.as_deref(), Some("tok-1"));
        assert_eq!(claimed.attempts_started, 1);
        assert!(claimed.started_at.is_some());
        assert!(claimed.lock_until.is_some());

        // Second worker claims the remaining job; the active one is not reclaimed.
        let second = engine
            .claim_job("tok-2", 30_000, "default", &names)
            .await
            .unwrap()
            .expect("second waiting job");
        assert_ne!(second.id, claimed.id);

        // Nothing left to claim.
        assert!(engine
            .claim_job("tok-3", 30_000, "default", &names)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn claim_job_respects_queue_and_registered_names() {
        let engine = InMemoryEngine::default();
        engine
            .enqueue_job(&MinionJobInput {
                queue: Some("shell".to_string()),
                ..job_input("run")
            })
            .await
            .unwrap();

        // Wrong queue.
        assert!(engine
            .claim_job("t", 1000, "default", &["run".to_string()])
            .await
            .unwrap()
            .is_none());
        // Unregistered name.
        assert!(engine
            .claim_job("t", 1000, "shell", &["other".to_string()])
            .await
            .unwrap()
            .is_none());
        // Empty registered names claims nothing.
        assert!(engine
            .claim_job("t", 1000, "shell", &[])
            .await
            .unwrap()
            .is_none());
        // Correct queue + name.
        assert!(engine
            .claim_job("t", 1000, "shell", &["run".to_string()])
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn complete_job_is_token_fenced() {
        let engine = InMemoryEngine::default();
        engine.enqueue_job(&job_input("w")).await.unwrap();
        let names = vec!["w".to_string()];
        let claimed = engine
            .claim_job("good", 30_000, "default", &names)
            .await
            .unwrap()
            .unwrap();

        // Wrong token -> None, no transition.
        assert!(engine
            .complete_job(claimed.id, "bad", None)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            engine.get_job(claimed.id).await.unwrap().unwrap().status,
            MinionJobStatus::Active
        );

        // Right token -> completed with result.
        let done = engine
            .complete_job(claimed.id, "good", Some(&serde_json::json!({"ok": true})))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.status, MinionJobStatus::Completed);
        assert_eq!(done.result, Some(serde_json::json!({"ok": true})));
        assert!(done.finished_at.is_some());
        assert!(done.lock_token.is_none());
    }

    #[tokio::test]
    async fn complete_job_remove_on_complete_drops_row() {
        let engine = InMemoryEngine::default();
        engine
            .enqueue_job(&MinionJobInput {
                remove_on_complete: Some(true),
                ..job_input("w")
            })
            .await
            .unwrap();
        let names = vec!["w".to_string()];
        let claimed = engine
            .claim_job("tok", 30_000, "default", &names)
            .await
            .unwrap()
            .unwrap();
        let done = engine
            .complete_job(claimed.id, "tok", None)
            .await
            .unwrap()
            .expect("returns the completed job even though the row is dropped");
        assert_eq!(done.status, MinionJobStatus::Completed);
        assert!(
            engine.get_job(claimed.id).await.unwrap().is_none(),
            "row removed after completion"
        );
    }

    #[tokio::test]
    async fn fail_job_delayed_sets_backoff() {
        let engine = InMemoryEngine::default();
        engine.enqueue_job(&job_input("w")).await.unwrap();
        let names = vec!["w".to_string()];
        let claimed = engine
            .claim_job("tok", 30_000, "default", &names)
            .await
            .unwrap()
            .unwrap();

        let before = crate::time::now_epoch_ms();
        let failed = engine
            .fail_job(claimed.id, "tok", "boom", FailOutcome::Delayed, 5_000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, MinionJobStatus::Delayed);
        assert_eq!(failed.attempts_made, 1);
        assert_eq!(failed.error_text.as_deref(), Some("boom"));
        assert_eq!(failed.stacktrace, vec!["boom".to_string()]);
        assert!(failed.finished_at.is_none(), "delayed retry is not terminal");
        assert!(failed.delay_until.unwrap() >= before + 5_000);
    }

    #[tokio::test]
    async fn fail_job_wrong_token_is_noop() {
        let engine = InMemoryEngine::default();
        engine.enqueue_job(&job_input("w")).await.unwrap();
        let names = vec!["w".to_string()];
        let claimed = engine
            .claim_job("tok", 30_000, "default", &names)
            .await
            .unwrap()
            .unwrap();
        assert!(engine
            .fail_job(claimed.id, "wrong", "x", FailOutcome::Failed, 0)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            engine.get_job(claimed.id).await.unwrap().unwrap().status,
            MinionJobStatus::Active
        );
    }

    #[tokio::test]
    async fn fail_job_terminal_then_retry_requeues() {
        let engine = InMemoryEngine::default();
        engine.enqueue_job(&job_input("w")).await.unwrap();
        let names = vec!["w".to_string()];
        let claimed = engine
            .claim_job("tok", 30_000, "default", &names)
            .await
            .unwrap()
            .unwrap();
        let failed = engine
            .fail_job(claimed.id, "tok", "nope", FailOutcome::Failed, 0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, MinionJobStatus::Failed);
        assert!(failed.finished_at.is_some());

        // retry_job requeues a failed job back to waiting, clearing state.
        let requeued = engine
            .retry_job(claimed.id)
            .await
            .unwrap()
            .expect("failed job is retryable");
        assert_eq!(requeued.status, MinionJobStatus::Waiting);
        assert!(requeued.error_text.is_none());
        assert!(requeued.finished_at.is_none());
        assert!(requeued.delay_until.is_none());
        assert!(requeued.lock_token.is_none());

        // A waiting job is not retryable (only failed/dead).
        assert!(engine.retry_job(claimed.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn renew_job_lock_extends_active_lease_only() {
        let engine = InMemoryEngine::default();
        engine.enqueue_job(&job_input("w")).await.unwrap();
        let names = vec!["w".to_string()];
        let claimed = engine
            .claim_job("tok", 1_000, "default", &names)
            .await
            .unwrap()
            .unwrap();

        // Wrong token -> false.
        assert!(!engine
            .renew_job_lock(claimed.id, "bad", 30_000)
            .await
            .unwrap());
        // Right token -> true, lock_until extended.
        assert!(engine
            .renew_job_lock(claimed.id, "tok", 30_000)
            .await
            .unwrap());
        let renewed = engine.get_job(claimed.id).await.unwrap().unwrap();
        assert!(renewed.lock_until.unwrap() >= crate::time::now_epoch_ms() + 29_000);
    }
}

#[async_trait]
impl crate::admin_queries::AdminQueries for InMemoryEngine {
    async fn get_stats(&self) -> crate::error::Result<crate::admin_queries::Stats> {
        use crate::admin_queries::Stats;
        Ok(Stats {
            connected_agents: 0,
            active_tokens: 0,
            active_api_keys: 0,
            requests_today: 0,
        })
    }

    async fn get_full_stats(&self) -> crate::error::Result<crate::admin_queries::FullStats> {
        use crate::admin_queries::FullStats;
        Ok(FullStats {
            page_count: 0,
            chunk_count: 0,
            engine_ok: true,
        })
    }

    async fn check_health_indicators(&self) -> crate::error::Result<crate::admin_queries::HealthIndicators> {
        use crate::admin_queries::HealthIndicators;
        Ok(HealthIndicators {
            expiring_soon: 0,
            error_rate: 0.0,
        })
    }

    async fn list_agents(&self) -> crate::error::Result<Vec<crate::admin_queries::AgentInfo>> {
        Ok(vec![])
    }

    async fn list_api_keys(&self) -> crate::error::Result<Vec<crate::admin_queries::ApiKey>> {
        Ok(vec![])
    }

    async fn create_api_key(&self, _name: &str) -> crate::error::Result<crate::admin_queries::ApiKey> {
        Err(crate::error::Error::engine("not implemented"))
    }

    async fn revoke_api_key(&self, _name: &str) -> crate::error::Result<()> {
        Err(crate::error::Error::engine("not implemented"))
    }

    async fn list_requests(
        &self,
        _filters: &crate::admin_queries::RequestLogFilters,
    ) -> crate::error::Result<crate::admin_queries::Paginated<crate::admin_queries::RequestLogEntry>> {
        use crate::admin_queries::{Paginated, RequestLogEntry};
        Ok(Paginated {
            items: vec![],
            total: 0,
            page: 1,
            limit: 50,
        })
    }

    async fn list_agent_client_spend(&self) -> crate::error::Result<Vec<crate::admin_queries::AgentClientSpend>> {
        Ok(vec![])
    }

    async fn get_watch_snapshot(&self) -> crate::error::Result<crate::admin_queries::WatchSnapshot> {
        use crate::admin_queries::{QueueHealth, WatchSnapshot};
        Ok(WatchSnapshot {
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
            by_type: vec![],
            queue_health: QueueHealth { waiting: 0, active: 0, stalled: 0 },
            lease_pressure_1h: 0,
            top_errors: vec![],
            budget_owners: vec![],
        })
    }
}

/// In-memory wave reversal — no wave tables in the in-memory store, so every
/// step is a no-op returning 0. Real behavior is exercised via the libsql /
/// postgres backends (see `undo_wave` tests).
#[async_trait]
impl CalibrationWaveQueries for InMemoryEngine {
    async fn revert_wave_resolutions(
        &self,
        _wave_version: &str,
        _resolved_by: &str,
        _dry_run: bool,
    ) -> crate::Result<u64> {
        Ok(0)
    }

    async fn unapply_wave_grade_cache(&self, _wave_version: &str, _dry_run: bool) -> crate::Result<u64> {
        Ok(0)
    }

    async fn delete_calibration_profiles_for_wave(
        &self,
        _wave_version: &str,
        _dry_run: bool,
    ) -> crate::Result<u64> {
        Ok(0)
    }

    async fn purge_nudge_log_for_wave(&self, _wave_version: &str, _dry_run: bool) -> crate::Result<u64> {
        Ok(0)
    }
}

/// In-memory calibration queries — computed from in-memory takes store.
#[async_trait]
impl CalibrationQueries for InMemoryEngine {
    async fn get_scorecard(
        &self,
        query: &ScorecardQuery<'_>,
    ) -> crate::error::Result<TakesScorecard> {
        // Domain scoping mirrors the TS `EXISTS(pages p WHERE p.id =
        // takes.page_id AND p.slug LIKE prefix%)` — build a page_id → slug map
        // from the pages store, then prefix-match. Only needed when a
        // domain_prefix is requested.
        let page_slugs: Option<std::collections::HashMap<u64, String>> =
            if query.domain_prefix.is_some() {
                let pages = self
                    .store
                    .lock()
                    .expect("InMemoryEngine store mutex poisoned");
                Some(pages.iter().map(|p| (p.id, p.slug.clone())).collect())
            } else {
                None
            };
        let store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let rows = store
            .iter()
            .filter(|t| {
                // Optional single-holder filter (canonical: omitted when None).
                if let Some(h) = query.holder {
                    if t.holder != h {
                        return false;
                    }
                }
                // Allow-list membership (canonical `AND holder = ANY($list)`,
                // D4 defense-in-depth). Applied per row so it composes with an
                // absent holder filter (all-holders-within-allow-list).
                if let Some(list) = query.holders_allow_list {
                    if !list.iter().any(|h| h == &t.holder) {
                        return false;
                    }
                }
                if let Some(prefix) = query.domain_prefix {
                    let matches = page_slugs
                        .as_ref()
                        .and_then(|m| m.get(&t.page_id))
                        .map(|slug| slug.starts_with(prefix))
                        .unwrap_or(false);
                    if !matches {
                        return false;
                    }
                }
                // `since_date >= since` / `<= until`; NULL since_date fails a
                // present bound (SQL `NULL >= x` is NULL ⇒ excluded).
                if let Some(since) = query.since {
                    match &t.since_date {
                        Some(d) if d.as_str() >= since => {}
                        _ => return false,
                    }
                }
                if let Some(until) = query.until {
                    match &t.since_date {
                        Some(d) if d.as_str() <= until => {}
                        _ => return false,
                    }
                }
                true
            })
            .map(|t| ScorecardRow {
                kind: t.kind.clone(),
                weight: t.weight,
                resolved_quality: t.resolved_quality.clone(),
            });
        Ok(aggregate_scorecard(rows))
    }

    async fn insert_calibration_profile(
        &self,
        row: &crate::calibration_queries::CalibrationProfileInsert<'_>,
    ) -> crate::error::Result<i64> {
        crate::calibration_queries::CalibrationQueries::insert_calibration_profile(self, row).await
    }

    async fn get_calibration_curve(
        &self,
        query: &CalibrationCurveQuery<'_>,
    ) -> crate::error::Result<Vec<CalibrationBucket>> {
        let store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let rows = store
            .iter()
            .filter(|t| {
                if let Some(h) = query.holder {
                    if t.holder != h {
                        return false;
                    }
                }
                // Allow-list membership (canonical `AND holder = ANY($list)`,
                // D4 defense-in-depth). Applied per row so it composes with an
                // absent holder filter (all-holders-within-allow-list).
                if let Some(list) = query.holders_allow_list {
                    if !list.iter().any(|h| h == &t.holder) {
                        return false;
                    }
                }
                true
            })
            .map(|t| CalibrationRow {
                weight: t.weight,
                resolved_quality: t.resolved_quality.clone(),
            });
        Ok(aggregate_calibration_curve(
            rows,
            query.bucket_size.unwrap_or(0.1),
        ))
    }

    async fn get_latest_profile(
        &self,
        _holder: &str,
        _source_id: Option<&str>,
        _source_ids: Option<&[String]>,
    ) -> crate::error::Result<Option<CalibrationProfileRow>> {
        Ok(None)
    }

    async fn get_pattern_detail(
        &self,
        _holder: &str,
        _pattern_index: usize,
    ) -> crate::error::Result<Option<PatternDetail>> {
        Ok(None)
    }
}

// ── OAuthQueries InMemory stubs ───────────────────────────────────────

#[async_trait]
impl OAuthQueries for InMemoryEngine {
    async fn register_client(
        &self,
        _req: RegisterClientRequest,
    ) -> crate::error::Result<RegisterClientResponse> {
        Ok(RegisterClientResponse {
            client_id: "test-client-id".into(),
            client_secret: "test-client-secret".into(),
        })
    }

    async fn update_client_ttl(
        &self,
        _client_id: &str,
        ttl: Option<i64>,
    ) -> crate::error::Result<UpdateClientTtlResponse> {
        Ok(UpdateClientTtlResponse {
            updated: true,
            token_ttl: ttl,
        })
    }

    async fn revoke_client(
        &self,
        _client_id: &str,
    ) -> crate::error::Result<RevokeClientResponse> {
        Ok(RevokeClientResponse { revoked: true })
    }

    async fn get_client(
        &self,
        client_id: &str,
    ) -> crate::error::Result<Option<OAuthClientInfo>> {
        // Stub: return a fixed client for any non-empty client_id.
        if client_id.is_empty() {
            return Ok(None);
        }
        Ok(Some(OAuthClientInfo {
            client_id: client_id.to_string(),
            client_secret_hash: Some(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            ),
            client_name: "Test Client".to_string(),
            redirect_uris: vec![],
            grant_types: vec!["client_credentials".to_string()],
            scope: Some("read write".to_string()),
            token_endpoint_auth_method: Some("client_secret_post".to_string()),
            client_id_issued_at: Some(1_700_000_000),
            client_secret_expires_at: None,
            token_ttl: None,
        }))
    }

    async fn exchange_client_credentials(
        &self,
        client_id: &str,
        _client_secret: &str,
        requested_scope: Option<&str>,
    ) -> crate::error::Result<ExchangeTokens> {
        let scope = requested_scope.unwrap_or("read").to_string();
        let scopes: Vec<String> = scope
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        let access_token = format!("test_at_{}", client_id);
        self.token_scopes
            .lock()
            .unwrap()
            .insert(access_token.clone(), scopes);
        Ok(ExchangeTokens {
            access_token,
            token_type: "bearer".to_string(),
            expires_in: 3600,
            scope,
            refresh_token: None,
        })
    }

    async fn verify_confidential_client_secret(
        &self,
        client_id: &str,
        _presented_secret: &str,
    ) -> crate::error::Result<OAuthClientInfo> {
        self.get_client(client_id).await?.ok_or_else(|| {
            crate::error::Error::engine("client not found")
        })
    }

    async fn exchange_authorization_code(
        &self,
        client_id: &str,
        _authorization_code: &str,
        _redirect_uri: Option<&str>,
    ) -> crate::error::Result<ExchangeTokens> {
        Ok(ExchangeTokens {
            access_token: format!("test_at_{}", client_id),
            token_type: "bearer".to_string(),
            expires_in: 3600,
            scope: "read write".to_string(),
            refresh_token: Some(format!("test_rt_{}", client_id)),
        })
    }

    async fn exchange_refresh_token(
        &self,
        client_id: &str,
        _refresh_token: &str,
        _requested_scopes: Option<&[String]>,
    ) -> crate::error::Result<ExchangeTokens> {
        Ok(ExchangeTokens {
            access_token: format!("test_at_{}", client_id),
            token_type: "bearer".to_string(),
            expires_in: 3600,
            scope: "read write".to_string(),
            refresh_token: Some(format!("test_rt_{}", client_id)),
        })
    }

    async fn sweep_expired_tokens(&self) -> crate::error::Result<u64> {
        // InMemory is stateless for OAuth — no sweeping needed.
        Ok(0)
    }
}

// ── TokenQueries InMemory stub ────────────────────────────────────────────────

#[async_trait]
impl crate::token_queries::TokenQueries for InMemoryEngine {
    async fn verify_access_token(
        &self,
        token: &str,
    ) -> std::result::Result<crate::token_queries::AuthInfo, crate::token_queries::TokenError> {
        if token.is_empty() {
            return Err(crate::token_queries::TokenError::Invalid);
        }
        let scopes = self
            .token_scopes
            .lock()
            .unwrap()
            .get(token)
            .cloned()
            .unwrap_or_else(|| vec!["read".to_string(), "write".to_string()]);
        Ok(crate::token_queries::AuthInfo {
            token: token.to_string(),
            client_id: "test-client".to_string(),
            client_name: Some("Test Client".to_string()),
            scopes,
            expires_at: i64::MAX,
            source_id: None,
            resource: None,
            allowed_sources: None,
        })
    }
}


// ─── Budget management tests (1-3-2) ────────────────────────────────────────

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[tokio::test]
    async fn inmem_budget_reserve_unsupported() {
        let engine = InMemoryEngine::new();
        let result = engine.reserve_budget(1, 100, "test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn inmem_budget_refund_unsupported() {
        let engine = InMemoryEngine::new();
        let result = engine.refund_budget(1, 100, "test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn inmem_budget_set_owner_unsupported() {
        let engine = InMemoryEngine::new();
        let result = engine.set_owner_budget(1, 1000).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn inmem_budget_halt_subtree_unsupported() {
        let engine = InMemoryEngine::new();
        let result = engine.halt_budget_subtree(1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn inmem_budget_inherit_unsupported() {
        let engine = InMemoryEngine::new();
        let result = engine.inherit_budget_owner(1, 2).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn inmem_budget_get_owner_unsupported() {
        let engine = InMemoryEngine::new();
        let result = engine.get_budget_owner(1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }
}

