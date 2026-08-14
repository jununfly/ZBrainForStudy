//! v0.37.0 — domain-bank: prefix-stratified far-page retrieval for
//! `zbrain brainstorm` + `zbrain lsd` (D14).
//!
//! Faithful port of `src/core/domain-bank.ts`. Orthogonal to hybridSearch:
//! pulls "far" pages directly from the corpus via two complementary
//! strategies:
//!   1. PRIMARY: prefix-stratified sampling — one page per distinct top-level
//!      slug prefix (`wiki/vc`, `concepts/`, …). The user's own brain
//!      organization IS the domain bank.
//!   2. FALLBACK: corpus-sampling — random-sample extra pages when primary
//!      returns < M (small brain / single-prefix corpus / close-set ate all
//!      prefixes).
//!   3. SPARSE WARNING: when even fallback can't fill M, return what we have
//!      with `short_of_target = true` (D11) — never fall back to LLM-invented
//!      domains.
//!
//! Distance score normalized (codex r2 #9): `1 - clamp(cosine_distance,0,2)/2`.

use crate::engine::{
    BrainEngine, CorpusSampleOpts, DomainBankRow, DomainBankSampleOpts, ListPrefixesOpts,
};
use crate::think::sanitize::sanitize_injection_only;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

/// Default 1-hour TTL for the prefix-enumeration cache (D3).
pub const PREFIX_CACHE_TTL_MS: u64 = 60 * 60 * 1000;
/// Per-far-page content cap before injection into the LLM prompt (D3 trust
/// boundary — same as takes content).
const FAR_CONTENT_LENGTH_CAP: usize = 4000;

/// Close-set ref the orchestrator passes for distance calc + prefix exclusion.
#[derive(Debug, Clone)]
pub struct CloseRef {
    pub slug: String,
    /// Used to exclude this prefix from the primary path so close + far don't overlap.
    pub prefix: Option<String>,
    /// Used as distance reference; None when the close-page has no embedded chunks.
    pub representative_chunk_id: Option<u64>,
}

/// Caller-facing options for the domain-bank orchestrator entry point.
#[derive(Debug, Clone, Default)]
pub struct FetchFarOpts {
    /// Target far-page count. brainstorm=6, lsd=12 (per D14 / plan).
    pub m: usize,
    /// Close-set from hybridSearch (used for exclusion + distance ref).
    pub close_set: Vec<CloseRef>,
    /// Question embedding; used as the distance anchor when close-set is empty (LSD K=0).
    pub question_embedding: Option<Vec<f32>>,
    /// When true (LSD), prefer never-retrieved or stale-by-N-days pages.
    pub stale_bias: bool,
    /// Stale-bias day threshold. Default 90.
    pub stale_threshold_days: i64,
    /// Source scope (canonical scalar).
    pub source_id: Option<String>,
    /// Federated read scope (array).
    pub source_ids: Option<Vec<String>>,
    /// Override the prefix-cache TTL (tests only).
    pub prefix_cache_ttl_ms: Option<u64>,
    /// Override the prefix list (tests — bypasses cache + enumeration).
    pub prefix_list_override: Option<Vec<String>>,
    /// Default embedding column for distance calc + getEmbeddingsByChunkIds lookup.
    pub embedding_column: Option<String>,
    /// Hard cap on distinct prefixes materialized (cost guardrail). Default
    /// `max(m*4, 50)`.
    pub max_far_set: Option<usize>,
    /// Deterministic shuffle seed for the prefix cap (tests only).
    pub prefix_shuffle_seed: Option<u64>,
}

/// One far-page result enriched with distance + provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct FarPage {
    pub slug: String,
    pub source_id: String,
    pub prefix: Option<String>,
    pub page_id: u64,
    pub title: Option<String>,
    /// INJECTION_PATTERNS-sanitized, length-capped. Safe to embed in an LLM prompt.
    pub content: String,
    /// Cosine distance normalized 0-1 (1 = orthogonal/opposite, 0 = identical).
    pub distance_score: f64,
    /// Inbound link count (tiebreaker, exposed for citation transparency).
    pub connection_count: i64,
    /// When this page was last surfaced by a user-facing op. Null = never retrieved.
    pub last_retrieved_at: Option<String>,
    /// Which sampling strategy produced this page.
    pub source: &'static str,
}

/// Top-level orchestrator return.
#[derive(Debug, Clone)]
pub struct FetchFarResult {
    pub pages: Vec<FarPage>,
    /// Distinct prefixes available after close-set exclusion.
    pub available_prefixes: usize,
    /// Distinct prefixes total before close-set exclusion.
    pub total_prefixes: usize,
    /// True iff corpus-sampling fallback fired.
    pub used_fallback: bool,
    /// True iff result still short of `m` after fallback (D11 stderr warn).
    pub short_of_target: bool,
}

fn prefix_cache_key(source_id: &Option<String>) -> String {
    format!(
        "brainstorm.domain_bank.prefixes:{}",
        source_id.as_deref().unwrap_or("default")
    )
}

/// Read + validate the cached prefix list. Returns None on cache miss,
/// expired entry, parse failure, or a get_config error (TS wraps in try/catch
/// → null; non-InMemory backends return Unsupported, treated as miss).
async fn read_prefix_cache(
    engine: &dyn BrainEngine,
    source_id: &Option<String>,
    ttl_ms: u64,
) -> Option<Vec<String>> {
    let key = prefix_cache_key(source_id);
    let raw = match engine.get_config(&key).await {
        Ok(Some(v)) => v,
        _ => return None,
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = parsed.as_object()?;
    let prefixes = obj.get("prefixes")?.as_array()?;
    let cached_at = obj.get("cached_at")?.as_u64()?;
    if prefixes.iter().any(|p| !p.is_string()) {
        return None;
    }
    // TTL check (wall clock, mirrors TS Date.now()).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now.saturating_sub(cached_at) > ttl_ms {
        return None;
    }
    Some(
        prefixes
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect(),
    )
}

async fn write_prefix_cache(
    engine: &dyn BrainEngine,
    source_id: &Option<String>,
    prefixes: &[String],
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let entry = serde_json::json!({ "prefixes": prefixes, "cached_at": now });
    // Non-fatal: a set_config error (e.g. Unsupported on a non-InMemory
    // backend) just means the next call re-enumerates.
    let _ = engine
        .set_config(&prefix_cache_key(source_id), &entry.to_string())
        .await;
}

/// Cosine distance normalized to [0,1] where 1 = orthogonal/opposite,
/// 0 = identical. Graceful on dimension mismatch (returns neutral 0.5) rather
/// than panicking — both inputs always come from the same embedding model, so
/// a mismatch signals a caller bug best surfaced as "no signal" rather than a
/// crash in the middle of an async pipeline.
#[must_use]
pub fn normalized_cosine_distance(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.5;
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for i in 0..a.len() {
        let x = f64::from(a[i]);
        let y = f64::from(b[i]);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        return 0.5; // zero-vector edge: neutral distance.
    }
    let cos_sim = dot / denom;
    let cos_dist = 1.0 - cos_sim; // [0,2] for unit-norm; can drift slightly.
    let clamped = cos_dist.clamp(0.0, 2.0); // clamp to [0,2] then halve → [0,1].
    clamped / 2.0
}

/// Distance from `far_embed` to the closest of `ref_embeds`. If `ref_embeds`
/// is empty, fall back to `question_embed`. If both empty, return 0.5
/// (neutral — no reference available).
#[must_use]
pub fn distance_from_closest(
    far_embed: Option<&[f32]>,
    ref_embeds: &[Vec<f32>],
    question_embed: Option<&[f32]>,
) -> f64 {
    let Some(far) = far_embed else {
        return 0.5; // no embedding on far page; can't compute.
    };
    if ref_embeds.is_empty() {
        return match question_embed {
            Some(q) => normalized_cosine_distance(far, q),
            None => 0.5,
        };
    }
    let mut min_dist = f64::MAX;
    for r in ref_embeds {
        let d = normalized_cosine_distance(far, r);
        if d < min_dist {
            min_dist = d;
        }
    }
    min_dist
}

/// Apply INJECTION_PATTERNS (same as takes content per v0.28.8) and cap at
/// FAR_CONTENT_LENGTH_CAP. Far-page content goes into the LLM prompt as
/// "you wrote …", so the same trust boundary applies.
fn sanitize_far_content(raw: &str) -> String {
    let cleaned = sanitize_injection_only(raw).text;
    if cleaned.chars().count() > FAR_CONTENT_LENGTH_CAP {
        let truncated: String = cleaned.chars().take(FAR_CONTENT_LENGTH_CAP - 3).collect();
        format!("{truncated}...")
    } else {
        cleaned
    }
}

/// Pull M far pages from the brain's source scope. Returns `pages.length <= m`;
/// caller emits the D11 sparse warning when `short_of_target == true`.
pub async fn fetch_far(
    engine: &dyn BrainEngine,
    opts: FetchFarOpts,
) -> crate::Result<FetchFarResult> {
    let m = opts.m;
    if m == 0 {
        return Ok(FetchFarResult {
            pages: vec![],
            available_prefixes: 0,
            total_prefixes: 0,
            used_fallback: false,
            short_of_target: false,
        });
    }
    let ttl_ms = opts.prefix_cache_ttl_ms.unwrap_or(PREFIX_CACHE_TTL_MS);

    // ---- Step 1: prefix enumeration (cache → DB) ----
    let all_prefixes: Vec<String> = if let Some(override_list) = &opts.prefix_list_override {
        override_list.clone()
    } else {
        let cached = read_prefix_cache(engine, &opts.source_id, ttl_ms).await;
        match cached {
            Some(c) => c,
            None => {
                let prefixes = engine
                    .list_prefixes(ListPrefixesOpts {
                        source_id: opts.source_id.clone(),
                        source_ids: opts.source_ids.clone(),
                    })
                    .await?;
                write_prefix_cache(engine, &opts.source_id, &prefixes).await;
                prefixes
            }
        }
    };
    let total_prefixes = all_prefixes.len();

    // ---- Step 2: filter prefixes that overlap with the close-set ----
    let close_prefix_set: std::collections::HashSet<String> = opts
        .close_set
        .iter()
        .filter_map(|c| c.prefix.clone())
        .collect();
    let candidate_prefixes: Vec<String> = all_prefixes
        .into_iter()
        .filter(|p| !close_prefix_set.contains(p))
        .collect();
    let available_prefixes = candidate_prefixes.len();
    let close_slugs: Vec<String> = opts.close_set.iter().map(|c| c.slug.clone()).collect();

    // ---- Step 2.5: cap the prefix list to `max_far_set` (cost guardrail) ----
    let max_far_set = opts.max_far_set.unwrap_or((m * 4).max(50));
    let mut candidate_prefixes = candidate_prefixes;
    if candidate_prefixes.len() > max_far_set {
        match opts.prefix_shuffle_seed {
            Some(seed) => {
                let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
                candidate_prefixes.shuffle(&mut rng);
            }
            None => {
                let mut rng = rand::thread_rng();
                candidate_prefixes.shuffle(&mut rng);
            }
        }
        candidate_prefixes.truncate(max_far_set);
    }

    // ---- Step 3: primary path — list_prefix_sampled_pages ----
    let mut primary_rows: Vec<DomainBankRow> = Vec::new();
    if !candidate_prefixes.is_empty() {
        primary_rows = engine
            .list_prefix_sampled_pages(DomainBankSampleOpts {
                prefixes: candidate_prefixes,
                exclude_slugs: close_slugs.clone(),
                stale_bias: opts.stale_bias,
                stale_threshold_days: opts.stale_threshold_days,
                source_id: opts.source_id.clone(),
                source_ids: opts.source_ids.clone(),
            })
            .await?;
    }

    // ---- Step 4: fallback if primary didn't fill M ----
    let mut fallback_rows: Vec<DomainBankRow> = Vec::new();
    let mut used_fallback = false;
    if primary_rows.len() < m {
        let need = (m - primary_rows.len()) as i64;
        let exclude_for_fallback: Vec<String> = close_slugs
            .iter()
            .chain(primary_rows.iter().map(|r| &r.slug))
            .cloned()
            .collect();
        fallback_rows = engine
            .list_corpus_sample(CorpusSampleOpts {
                n: need,
                exclude_slugs: exclude_for_fallback,
                seed: None,
                source_id: opts.source_id.clone(),
                source_ids: opts.source_ids.clone(),
            })
            .await?;
        used_fallback = !fallback_rows.is_empty();
    }

    // ---- Step 5: hydrate embeddings for distance calc ----
    let all_rows: Vec<(DomainBankRow, &'static str)> = primary_rows
        .into_iter()
        .map(|r| (r, "prefix-stratified"))
        .chain(fallback_rows.into_iter().map(|r| (r, "corpus-sample")))
        .collect();

    let close_chunk_ids: Vec<u64> = opts
        .close_set
        .iter()
        .filter_map(|c| c.representative_chunk_id)
        .collect();
    let far_chunk_ids: Vec<u64> = all_rows
        .iter()
        .filter_map(|(r, _)| r.representative_chunk_id)
        .collect();
    let mut chunk_ids: Vec<u64> = close_chunk_ids
        .iter()
        .chain(far_chunk_ids.iter())
        .copied()
        .collect();
    chunk_ids.sort_unstable();
    chunk_ids.dedup();

    let embeddings = if chunk_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        engine
            .get_embeddings_by_chunk_ids(&chunk_ids, opts.embedding_column.as_deref())
            .await?
    };

    let ref_embeds: Vec<Vec<f32>> = close_chunk_ids
        .iter()
        .filter_map(|id| embeddings.get(id).cloned())
        .collect();

    // ---- Step 6: build FarPage results with normalized distance ----
    let short_of_target = all_rows.len() < m;
    let mut all_pages: Vec<FarPage> = all_rows
        .into_iter()
        .map(|(row, src)| {
            let far_embed = row
                .representative_chunk_id
                .and_then(|id| embeddings.get(&id).map(|v| v.as_slice()));
            let distance_score =
                distance_from_closest(far_embed, &ref_embeds, opts.question_embedding.as_deref());
            FarPage {
                slug: row.slug,
                source_id: row.source_id,
                prefix: row.prefix,
                page_id: row.page_id,
                title: row.title,
                content: sanitize_far_content(&row.compiled_truth),
                distance_score,
                connection_count: row.connection_count,
                last_retrieved_at: row.last_retrieved_at,
                source: src,
            }
        })
        .collect();

    // ---- Step 6.5: final trim to `m` by distance_score DESC ----
    all_pages.sort_by(|a, b| {
        b.distance_score
            .partial_cmp(&a.distance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_pages.truncate(m);

    Ok(FetchFarResult {
        pages: all_pages,
        available_prefixes,
        total_prefixes,
        used_fallback,
        // Reflects whether the *pre-trim* candidate pool fell short of `m`.
        short_of_target: short_of_target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    #[test]
    fn cosine_same_vector_is_zero() {
        let a = vec![1.0_f32, 0.0, 0.0];
        assert!((normalized_cosine_distance(&a, &a) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn cosine_orthogonal_is_half() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        assert!((normalized_cosine_distance(&a, &b) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn cosine_dim_mismatch_is_neutral() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        assert!((normalized_cosine_distance(&a, &b) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn distance_from_closest_neutral_without_refs() {
        assert_eq!(distance_from_closest(None, &[], None), 0.5);
        let q = vec![1.0_f32, 0.0];
        assert!((distance_from_closest(None, &[], Some(&q)) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn sanitize_caps_long_content() {
        let long = "a".repeat(5000);
        let out = sanitize_far_content(&long);
        assert!(out.chars().count() <= FAR_CONTENT_LENGTH_CAP);
        assert!(out.ends_with("..."));
    }

    #[tokio::test]
    async fn fetch_far_empty_when_m_zero() {
        let engine = InMemoryEngine::new();
        let res = fetch_far(
            &engine,
            FetchFarOpts {
                m: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(res.pages.is_empty());
        assert!(!res.short_of_target);
    }

    #[tokio::test]
    async fn fetch_far_uses_prefix_override_and_truncates() {
        let engine = InMemoryEngine::new();
        // With no pages seeded, the primary + fallback paths return nothing,
        // but the prefix list override still drives total/available counts.
        let res = fetch_far(
            &engine,
            FetchFarOpts {
                m: 6,
                prefix_list_override: Some(vec![
                    "wiki/vc".into(),
                    "people/maria".into(),
                    "concepts/drift".into(),
                ]),
                close_set: vec![CloseRef {
                    slug: "wiki/vc/intro".into(),
                    prefix: Some("wiki/vc".into()),
                    representative_chunk_id: None,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(res.total_prefixes, 3);
        assert_eq!(res.available_prefixes, 2); // wiki/vc excluded by close-set
        assert!(res.short_of_target); // nothing seeded → short
        assert!(res.pages.is_empty());
    }
}
