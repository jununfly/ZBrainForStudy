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
use serde_json::{json, Map, Value};

use crate::{
    calibration_queries::{CalibrationBucket, CalibrationProfileRow, CalibrationQueries,
        PatternDetail, TakeSummary, TakesScorecard},
    oauth_queries::{ExchangeTokens, OAuthClientInfo, OAuthQueries, RegisterClientRequest,
        RegisterClientResponse, RevokeClientResponse, UpdateClientTtlResponse},
    time::current_utc_iso8601, types::PageVersion, types::RawData, types::Take,
    types::TakeInput, types::TakeResolution, types::UpsertTakesResult, CRMode, DuplicatePage,
    EffectiveDateSource, Error, EntityCount, FactInsertStatus, FactKind, FactListOpts, FactRow,
    FactVisibility, FactsHealth, FileRow, FileSpec, FindDuplicatePageOpts, GraphNode, GraphPath,
    Link, LinkBatchInput, NewFact, OrphanPage, PageKind, PageRef, PageType, PurgeResult,
    RefreshPageBodyArgs, UpsertFileResult,
};

// ─── Value types ─────────────────────────────────────────────────────────────

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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone)]
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
fn decode_embedding_le(bytes: &[u8]) -> Option<Vec<f32>> {
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
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
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
fn iso8601_to_unix_ms(s: &str) -> Option<i64> {
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
#[derive(Debug, Clone, serde::Deserialize)]
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
#[async_trait]
pub trait BrainEngine: Send + Sync + std::fmt::Debug {
    // ── Identity ──────────────────────────────────────────────────────────

    /// Returns the backend discriminator. Used for conditional logic in
    /// migrations and diagnostics without `downcast`.
    fn kind(&self) -> EngineKind;

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
    /// implementation per backend.
    async fn search_pages(&self, _opts: &SearchOpts) -> crate::Result<Vec<SearchResult>> {
        Ok(Vec::new())
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

    // ── Takes (Phase 7A) ──────────────────────────────────────────────────

    /// Return all takes for a page, ordered by `row_num` ascending.
    /// Mirrors TS `getTakesForPage(pageId)`.
    async fn get_takes_for_page(&self, page_id: u64) -> crate::Result<Vec<Take>> {
        let _ = page_id;
        Err(crate::error::StructuredError::new(
            "Unsupported",
            "unsupported",
            "get_takes_for_page not yet implemented for this engine",
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

/// In-process engine backed by a `Vec<Page>`. Not persistent, not
/// transactional — its only job is to validate the trait contract in unit
/// tests and integration harnesses.
#[derive(Debug, Default)]
pub struct InMemoryEngine {
    store: Mutex<Vec<Page>>,
    next_id: Mutex<u64>,
    file_store: Mutex<Vec<FileRow>>,
    next_file_id: Mutex<u64>,
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
}

// ─── Tag helpers ─────────────────────────────────────────────────────────────

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

impl InMemoryEngine {
    /// Create a new empty InMemoryEngine for testing.
    pub fn new() -> Self {
        Self {
            store: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
            file_store: Mutex::new(Vec::new()),
            next_file_id: Mutex::new(1),
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
        let keywords_lower: Vec<String> =
            opts.keywords.iter().map(|k| k.to_lowercase()).collect();

        // Build the fused, pre-boost result list under the store lock in a
        // scoped block so the (non-Send) MutexGuard is dropped before the async
        // salience read below — a guard held across an await would make this
        // future non-Send.
        let mut results = {
            let store = self
                .store
                .lock()
                .expect("InMemoryEngine store mutex poisoned");

        // Candidate set after deleted / source filtering, indexed by page id so
        // the two retrieval paths and the fusion step share one lookup.
        let mut candidates: std::collections::HashMap<u64, &Page> =
            std::collections::HashMap::new();
        for page in store.iter() {
            if page.deleted_at.is_some() {
                continue;
            }
            if let Some(source_id) = &opts.source_id {
                if page.source_id != *source_id {
                    continue;
                }
            }
            candidates.insert(page.id, page);
        }

        // ── Lexical path ────────────────────────────────────────────────────
        // Substring match over title / compiled_truth / frontmatter. Produces a
        // rank-ordered list of page ids (higher weighted-hit sum ranks first).
        let mut lexical: Vec<(u64, f64)> = Vec::new();
        for (&id, page) in &candidates {
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
            for (&id, page) in &candidates {
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
            let Some(page) = candidates.get(&id) else {
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

            results
        }; // store lock (and all borrows of it) dropped here

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
            let salience = self.get_salience_scores(&refs).await?;

            for r in &mut results {
                if !r.score.is_finite() || r.score < floor {
                    continue;
                }
                let key = format!("{}::{}", r.page.source_id, r.page.slug);
                let Some(&s) = salience.get(&key) else { continue };
                if s <= 0.0 {
                    continue;
                }
                let factor = 1.0 + SALIENCE_BOOST_COEF_ON * (1.0 + s).ln();
                r.score *= factor;
                r.salience_boost = Some(factor);
            }

            // Recency stage (per-prefix half-life decay). Uses the same
            // once-computed floor as salience so a weak-overlap tail page can't
            // leapfrog the primary hit by stacking recency on top. The decay
            // map is caller-resolved config (defaults + zbrain.yml + env +
            // overrides), passed in via SearchOpts — the engine never reads env
            // itself, staying a pure scoring machine. Dates come from the
            // engine's own get_effective_dates; strength is pinned to 'on'
            // (search-mode system unported — see the salience note above / G13).
            let date_strings = self.get_effective_dates(&refs).await?;
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

        // Sort by score descending (boosts may have reordered the head).
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Apply limit if set
        if let Some(limit) = opts.limit {
            results.truncate(limit);
        }

        Ok(results)
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

    // --- Phase 7A: Takes ---

    async fn get_takes_for_page(&self, page_id: u64) -> crate::Result<Vec<Take>> {
        let store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let mut takes: Vec<_> = store.iter().filter(|t| t.page_id == page_id).cloned().collect();
        takes.sort_by_key(|t| t.row_num);
        Ok(takes)
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
        let mut found = false;
        for take in store.iter_mut() {
            if take.page_id == page_id && take.row_num == row_num {
                take.resolved_at = Some(now.clone());
                take.resolved_quality = resolution.quality.clone();
                take.resolved_outcome = resolution.outcome;
                take.resolved_evidence = resolution.evidence.clone();
                take.resolved_value = resolution.value;
                take.resolved_unit = resolution.unit.clone();
                take.resolved_by = resolution.by.clone();
                take.updated_at = now.clone();
                found = true;
                break;
            }
        }
        if found {
            Ok(())
        } else {
            Err(crate::error::StructuredError::new(
                "Not Found",
                "not_found",
                format!("no take found for page_id={page_id} row_num={row_num}"),
            ))
        }
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
                    if dir == "out" || dir == "both" {
                        l.from_page_id == current_id
                    } else if dir == "in" {
                        l.to_page_id == current_id
                    } else {
                        false
                    }
                })
                .filter(|l| {
                    link_type.is_none() || l.link_type == link_type.unwrap_or("")
                })
                .collect();

            for edge in &edges {
                let neighbor_id = if dir == "in" {
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
            valid_from: input.valid_from.clone(),
            valid_until: input.valid_until.clone(),
            expired_at: None,
            superseded_by: None,
            consolidated_at: None,
            consolidated_into: None,
            source: input.source.clone(),
            source_session: input.source_session.clone(),
            confidence: input.confidence.unwrap_or(1.0),
            created_at: Some(now),
        };
        store.push(row);

        if maybe_supersede.is_some() {
            Ok(FactInsertStatus::Superseded)
        } else {
            Ok(FactInsertStatus::Inserted)
        }
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

    // --- #110: Chunks & Code Edges (slice #110) ---

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
        _edges: &[crate::import::CodeEdgeInput],
    ) -> crate::Result<()> {
        // TODO: implement code edge storage in InMemoryEngine
        Ok(())
    }

    async fn delete_code_edges_for_chunks(
        &self,
        _chunk_ids: &[i64],
    ) -> crate::Result<()> {
        // TODO: implement code edge deletion in InMemoryEngine
        Ok(())
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
                },
            )
            .await
            .unwrap();

        let ok = engine.expire_fact("source-b", 1).await.unwrap();
        assert!(!ok);
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

/// In-memory calibration queries — computed from in-memory takes store.
#[async_trait]
impl CalibrationQueries for InMemoryEngine {
    async fn get_scorecard(&self, holder: &str) -> crate::error::Result<TakesScorecard> {
        let store = self
            .takes_store
            .lock()
            .expect("InMemoryEngine takes_store mutex poisoned");
        let resolved: Vec<_> = store
            .iter()
            .filter(|t| t.holder == holder && t.resolved_outcome.is_some())
            .collect();
        let total = resolved.len() as i64;
        if total == 0 {
            return Ok(TakesScorecard {
                resolved: 0,
                brier: 0.0,
                accuracy: 0.0,
                correct: 0,
                incorrect: 0,
                partial_rate: 0.0,
            });
        }
        let correct = resolved
            .iter()
            .filter(|t| t.resolved_outcome == Some(true))
            .count() as i64;
        let incorrect = total - correct;
        let accuracy = if total > 0 { correct as f64 / total as f64 } else { 0.0 };
        let brier_sum: f64 = resolved
            .iter()
            .map(|t| {
                let pred = t.weight;
                let actual = if t.resolved_outcome == Some(true) { 1.0 } else { 0.0 };
                (pred - actual).powi(2)
            })
            .sum();
        let brier = if total > 0 { brier_sum / total as f64 } else { 0.0 };
        Ok(TakesScorecard {
            resolved: total,
            brier,
            accuracy,
            correct,
            incorrect,
            partial_rate: 0.0,
        })
    }

    async fn get_calibration_curve(
        &self,
        _holder: &str,
    ) -> crate::error::Result<Vec<CalibrationBucket>> {
        Ok(vec![])
    }

    async fn get_latest_profile(
        &self,
        _holder: &str,
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
