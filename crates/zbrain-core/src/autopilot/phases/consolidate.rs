//! 1-6-5: consolidate dream phase — faithful Rust port of
//! `src/core/cycle/phases/consolidate.ts` (v0.35.4).
//!
//! Deterministic (no LLM): for each `(source_id, entity_slug)` bucket of
//! unconsolidated active facts, age-gate (24 h) → greedy cosine clustering
//! (threshold 0.85; the `embedding` column is read out-of-band via
//! `execute_raw` because `FactRow` does not carry it) → promote the
//! highest-confidence fact's text to a `takes(kind=fact)` row (semantic
//! upsert keyed on `(page_id, claim, since_date)`) → mark contributing facts
//! `consolidated_at` (never delete) → bitemporal `valid_until` writeback.
//!
//! Engines without raw-SQL support (e.g. `InMemoryEngine`) skip cleanly;
//! facts without an embedding cluster as singletons (no take written),
//! matching TS behaviour (cycle-inserted facts usually have `embedding = NULL`).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use erased_serde::Serialize;
use serde_json::Value;

use crate::autopilot::cycle::{PhaseResult, PhaseStatus};
use crate::engine::BrainEngine;
use crate::error::StructuredError;
use crate::types::TakeInput;

const DEFAULT_CLUSTER_THRESHOLD: f64 = 0.85;
const DEFAULT_MIN_FACTS_PER_BUCKET: usize = 3;
const DEFAULT_MIN_OLDEST_AGE_MS: u64 = 24 * 60 * 60 * 1000;
const SOURCE_MAX_LEN: usize = 200;

#[derive(Debug, Clone)]
pub struct ConsolidatePhaseOpts {
    pub dry_run: bool,
    pub cluster_threshold: f64,
    pub min_facts_per_bucket: usize,
    pub min_oldest_age_ms: u64,
    pub signal: Option<Arc<AtomicBool>>,
}

impl Default for ConsolidatePhaseOpts {
    fn default() -> Self {
        Self {
            dry_run: false,
            cluster_threshold: DEFAULT_CLUSTER_THRESHOLD,
            min_facts_per_bucket: DEFAULT_MIN_FACTS_PER_BUCKET,
            min_oldest_age_ms: DEFAULT_MIN_OLDEST_AGE_MS,
            signal: None,
        }
    }
}

pub struct ConsolidatePhase;

/// Lightweight fact projection read via raw SQL. `embedding` is intentionally
/// absent from `FactRow`, so we pull it out-of-band here.
#[derive(Debug, Clone)]
struct FactView {
    id: i64,
    fact: String,
    confidence: f64,
    valid_from: Option<String>,
    source: String,
    source_session: Option<String>,
    embedding: Option<Vec<f64>>,
}

impl ConsolidatePhase {
    pub async fn run(
        engine: &dyn BrainEngine,
        opts: &ConsolidatePhaseOpts,
    ) -> Result<PhaseResult, StructuredError> {
        let threshold = opts.cluster_threshold;
        let min_per_bucket = opts.min_facts_per_bucket;
        let min_oldest_age_ms = opts.min_oldest_age_ms;

        let mut facts_consolidated: u64 = 0;
        let mut takes_written: u64 = 0;
        let mut buckets_processed: u64 = 0;
        let mut buckets_skipped: u64 = 0;

        // 1) Scan (source_id, entity_slug) buckets of unconsolidated facts.
        //    This also acts as the raw-SQL capability probe: engines without
        //    raw support (InMemory) skip cleanly instead of failing.
        let bucket_sql = format!(
            "SELECT source_id, entity_slug, COUNT(*)::int AS count \
             FROM facts \
             WHERE consolidated_at IS NULL AND expired_at IS NULL AND entity_slug IS NOT NULL \
             GROUP BY source_id, entity_slug \
             HAVING COUNT(*) >= {min_per_bucket}"
        );
        let bucket_params: &[&(dyn Serialize + Sync)] = &[];
        let buckets = match engine.execute_raw(&bucket_sql, bucket_params).await {
            Ok(rows) => rows,
            Err(e) if is_unsupported(&e) => {
                return Ok(PhaseResult {
                    phase: "consolidate".into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: 0,
                    summary: "consolidate skipped (engine lacks raw-SQL support)".into(),
                    details: serde_json::json!({ "reason": "engine_unsupported_no_raw" }),
                    error: None,
                });
            }
            Err(e) => return Err(e),
        };

        for b in &buckets {
            // Yield / abort hook (mirrors TS `yieldDuringPhase`).
            if let Some(sig) = &opts.signal {
                if sig.load(Ordering::Relaxed) {
                    return Ok(PhaseResult {
                        phase: "consolidate".into(),
                        status: PhaseStatus::Skipped,
                        duration_ms: 0,
                        summary: "consolidate aborted (signal)".into(),
                        details: serde_json::json!({ "reason": "aborted" }),
                        error: None,
                    });
                }
            }

            let source_id = match b.get("source_id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    buckets_skipped += 1;
                    continue;
                }
            };
            let entity_slug = match b.get("entity_slug").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    buckets_skipped += 1;
                    continue;
                }
            };

            // 2) Pull unconsolidated facts for this bucket (with embedding).
            let fact_sql = "SELECT id, fact, confidence, valid_from, source, source_session, embedding \
                            FROM facts \
                            WHERE source_id = $1 AND entity_slug = $2 \
                              AND consolidated_at IS NULL AND expired_at IS NULL";
            let fact_params: &[&(dyn Serialize + Sync)] = &[&source_id, &entity_slug];
            let raw_facts = engine.execute_raw(fact_sql, fact_params).await?;
            let facts: Vec<FactView> = raw_facts.iter().filter_map(parse_fact_view).collect();
            if facts.len() < min_per_bucket {
                buckets_skipped += 1;
                continue;
            }

            // 3) Age gate: the oldest fact must be >= min_oldest_age_ms old.
            let oldest = facts
                .iter()
                .map(|f| f.valid_from.as_deref().unwrap_or(""))
                .min()
                .unwrap_or("");
            let oldest_ms = parse_iso_ms(oldest);
            let now_ms = Utc::now().timestamp_millis() as u64;
            if oldest_ms == 0 || now_ms.saturating_sub(oldest_ms) < min_oldest_age_ms {
                buckets_skipped += 1;
                continue;
            }

            buckets_processed += 1;
            let clusters = cluster_facts(&facts, threshold);

            // 4) Resolve entity_slug → page_id (skip cluster if page missing).
            let page_sql = "SELECT id FROM pages WHERE source_id = $1 AND slug = $2 AND deleted_at IS NULL LIMIT 1";
            let page_params: &[&(dyn Serialize + Sync)] = &[&source_id, &entity_slug];
            let page_rows = engine.execute_raw(page_sql, page_params).await?;
            let page_id: u64 = match page_rows
                .first()
                .and_then(|r| r.get("id"))
                .and_then(|v| v.as_i64())
            {
                Some(id) if id > 0 => id as u64,
                _ => continue,
            };

            // 5) Existing row_num max → append after it.
            let rownum_sql = "SELECT COALESCE(MAX(row_num), 0)::int AS max FROM takes WHERE page_id = $1";
            let rownum_params: &[&(dyn Serialize + Sync)] = &[&page_id];
            let rownum_rows = engine.execute_raw(rownum_sql, rownum_params).await?;
            let mut next_row_num: i32 = rownum_rows
                .first()
                .and_then(|r| r.get("max"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32
                + 1;

            for cluster in &clusters {
                if cluster.len() < 2 {
                    continue;
                }
                // Take selection: highest-confidence fact's text as the claim.
                let best = cluster
                    .iter()
                    .max_by(|a, b| {
                        a.confidence
                            .partial_cmp(&b.confidence)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap();
                let avg_weight: f64 =
                    cluster.iter().map(|f| f.confidence).sum::<f64>() / cluster.len() as f64;
                let sources: String = cluster
                    .iter()
                    .filter_map(|f| f.source_session.clone().or_else(|| Some(f.source.clone())))
                    .filter(|s| !s.is_empty())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(",");
                // TS `sources.slice(0, 200)` — cap the aggregated source string.
                let sources = sources.chars().take(SOURCE_MAX_LEN).collect::<String>();
                let since_iso = cluster
                    .iter()
                    .filter_map(|f| f.valid_from.clone())
                    .min()
                    .map(|s| s.chars().take(10).collect::<String>())
                    .unwrap_or_default();

                if opts.dry_run {
                    takes_written += 1;
                    facts_consolidated += cluster.len() as u64;
                    next_row_num += 1;
                    continue;
                }

                // 6) Semantic upsert keyed on (page_id, claim, since_date).
                let existing_sql =
                    "SELECT id FROM takes WHERE page_id = $1 AND claim = $2 AND since_date = $3 LIMIT 1";
                let existing_params: &[&(dyn Serialize + Sync)] =
                    &[&page_id, &best.fact, &since_iso];
                let existing = engine.execute_raw(existing_sql, existing_params).await?;
                let take_id: i64 = if let Some(row) = existing.first() {
                    let id = row.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    // Re-promotion: refresh source aggregation, keep row_num + weight.
                    let upd_sql = "UPDATE takes SET source = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2";
                    let upd_params: &[&(dyn Serialize + Sync)] = &[&sources, &id];
                    let _ = engine.execute_raw(upd_sql, upd_params).await;
                    id
                } else {
                    let input = TakeInput {
                        page_id,
                        row_num: Some(next_row_num),
                        claim: best.fact.clone(),
                        kind: "fact".into(),
                        holder: "self".into(),
                        weight: clamp01(avg_weight),
                        since_date: Some(since_iso.clone()),
                        until_date: None,
                        source: Some(sources.clone()),
                        superseded_by: None,
                        active: Some(true),
                    };
                    let res = engine.add_takes_batch(page_id, &[input]).await?;
                    if res.upserted < 1 {
                        next_row_num += 1;
                        continue;
                    }
                    let id_sql = "SELECT id FROM takes WHERE page_id = $1 AND row_num = $2";
                    let id_params: &[&(dyn Serialize + Sync)] = &[&page_id, &next_row_num];
                    let id_rows = engine.execute_raw(id_sql, id_params).await?;
                    let id = id_rows
                        .first()
                        .and_then(|r| r.get("id"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    next_row_num += 1;
                    takes_written += 1;
                    id
                };

                // 7) Mark all contributing facts consolidated (never delete).
                for f in cluster {
                    let upd_sql =
                        "UPDATE facts SET consolidated_at = CURRENT_TIMESTAMP, consolidated_into = $1 WHERE id = $2";
                    let upd_params: &[&(dyn Serialize + Sync)] = &[&take_id, &f.id];
                    let _ = engine.execute_raw(upd_sql, upd_params).await;
                    facts_consolidated += 1;
                }

                // 8) Bitemporal valid_until writeback (chronological order).
                let mut chronological: Vec<&FactView> = cluster.iter().collect();
                chronological.sort_by(|a, b| {
                    let ta = a.valid_from.as_deref().unwrap_or("");
                    let tb = b.valid_from.as_deref().unwrap_or("");
                    ta.cmp(tb).then_with(|| a.id.cmp(&b.id))
                });
                for i in 0..chronological.len().saturating_sub(1) {
                    let older = chronological[i];
                    let newer = chronological[i + 1];
                    let newer_vf = newer.valid_from.clone().unwrap_or_default();
                    let upd_sql = "UPDATE facts SET valid_until = $1 WHERE id = $2";
                    let upd_params: &[&(dyn Serialize + Sync)] = &[&newer_vf, &older.id];
                    let _ = engine.execute_raw(upd_sql, upd_params).await;
                }
            }
        }

        Ok(PhaseResult {
            phase: "consolidate".into(),
            status: PhaseStatus::Ok,
            duration_ms: 0,
            summary: if opts.dry_run {
                format!(
                    "(dry-run) would promote {facts_consolidated} facts into {takes_written} takes across {buckets_processed} buckets"
                )
            } else {
                format!(
                    "promoted {facts_consolidated} facts into {takes_written} takes across {buckets_processed} buckets"
                )
            },
            details: serde_json::json!({
                "dryRun": opts.dry_run,
                "facts_consolidated": facts_consolidated,
                "takes_written": takes_written,
                "buckets_processed": buckets_processed,
                "buckets_skipped": buckets_skipped,
            }),
            error: None,
        })
    }
}

/// True for engines that report raw-SQL / takes support as "not implemented".
fn is_unsupported(e: &StructuredError) -> bool {
    let m = format!("{} {}", e.class, e.message).to_lowercase();
    m.contains("unsupported") || m.contains("not implemented") || m.contains("not_yet_implemented")
}

/// Parse a fact row returned by raw SQL into a [`FactView`].
fn parse_fact_view(v: &Value) -> Option<FactView> {
    Some(FactView {
        id: v.get("id")?.as_i64()?,
        fact: v.get("fact")?.as_str()?.to_string(),
        confidence: v.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0),
        valid_from: v.get("valid_from").and_then(|x| x.as_str()).map(|s| s.to_string()),
        source: v
            .get("source")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        source_session: v
            .get("source_session")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        embedding: v.get("embedding").and_then(parse_embedding),
    })
}

/// Best-effort parse of a `facts.embedding` cell into an f64 vector.
/// Handles pgvector text forms (`[...]` / `{...}`) and JSON arrays; returns
/// `None` when the cell is null or in an undecodable format (so the fact
/// clusters as a singleton and writes no take — matching TS behaviour).
fn parse_embedding(v: &Value) -> Option<Vec<f64>> {
    match v {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for x in arr {
                out.push(x.as_f64()?);
            }
            Some(out)
        }
        Value::String(s) => {
            let t = s.trim();
            let inner = t
                .trim_start_matches('[')
                .trim_start_matches('{')
                .trim_end_matches(']')
                .trim_end_matches('}');
            if inner.is_empty() {
                return Some(Vec::new());
            }
            let mut out = Vec::new();
            for part in inner.split(',') {
                out.push(part.trim().parse::<f64>().ok()?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Greedy cosine clustering. Facts are visited in `valid_from` DESC order;
/// each fact joins the first cluster whose head is within `threshold` cosine
/// (or starts a new cluster). Facts without an embedding form singletons.
fn cluster_facts(facts: &[FactView], threshold: f64) -> Vec<Vec<FactView>> {
    let mut sorted: Vec<&FactView> = facts.iter().collect();
    sorted.sort_by(|a, b| {
        let ta = a.valid_from.as_deref().unwrap_or("");
        let tb = b.valid_from.as_deref().unwrap_or("");
        tb.cmp(ta)
    });
    let mut clusters: Vec<Vec<FactView>> = Vec::new();
    for f in sorted {
        if f.embedding.is_none() {
            clusters.push(vec![f.clone()]);
            continue;
        }
        let mut placed = false;
        for c in clusters.iter_mut() {
            let head = &c[0];
            if head.embedding.is_none() {
                continue;
            }
            if let (Some(he), Some(fe)) = (&head.embedding, &f.embedding) {
                if cosine_similarity(he, fe) >= threshold {
                    c.push(f.clone());
                    placed = true;
                    break;
                }
            }
        }
        if !placed {
            clusters.push(vec![f.clone()]);
        }
    }
    clusters
}

fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

fn clamp01(x: f64) -> f64 {
    if !x.is_finite() {
        return 0.5;
    }
    x.clamp(0.0, 1.0)
}

fn parse_iso_ms(s: &str) -> u64 {
    if s.is_empty() {
        return 0;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp_millis() as u64;
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(naive) = d.and_hms_opt(0, 0, 0) {
            let dt = chrono::DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc);
            return dt.timestamp_millis() as u64;
        }
    }
    0
}
