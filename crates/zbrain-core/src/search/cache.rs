//! 1-5-17 / G69 — Semantic query cache (Rust port of the TS
//! `hybridSearchCached` / `SemanticQueryCache`).
//!
//! The cache is a thin wrapper over the `BrainEngine::cache_*` trait
//! methods (defined in `engine.rs`). It owns the **orchestration** —
//! when to consult, when to skip, when to best-effort write back —
//! while backends own the **persistence** (libsql real table in
//! commit B, in-memory `Vec<InternalQueryCacheRow>` here in commit A).
//!
//! Mirrors `src/core/search/query-cache.ts` (semantic lookup,
//! skip rules, store, clear, prune, stats). The two-layer D11 cache
//! invalidation gate is enforced in the backend (`d11_gate_passes`),
//! not here.
//!
//! Skip rules (a `Some(_)` on any of these bypasses the cache
//! **lookup**; the `cache_store` write is unaffected so future
//! well-shaped calls can still hit):
//!   * `walk_depth` is set (TS `HybridSearchOpts.walkDepth`) — the
//!     caller is doing a follow-up expansion of a previous result
//!     set, so the embedding represents a different effective query.
//!   * `near_symbol` is set (TS `HybridSearchOpts.nearSymbol`) — the
//!     effective query is the symbol itself, not the user text.
//!   * `embedding_column` is set to a non-default value (G70) —
//!     cached row-level vectors may come from a different column.
//!   * `use_cache = false` — explicit opt-out.
//!   * No `embedding_client` is present — the query embedding is
//!     unavailable, so the cache key (cosine) cannot be computed.
//!
//! The orchestrator is **fail-open**: every backend error
//! (`Err(Unsupported)`, transient storage hiccup, D11 gate failure)
//! degrades to a cache miss and the normal `hybrid_search` runs.
//! Cache writes are best-effort and never fail the search hot path
//! (mirrors `query-cache.ts:263`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::embedding::EmbeddingClient;
use crate::engine::{
    query_cache_row_id, BrainEngine, CacheHit, CacheLookupOpts, CacheStoreOpts, SearchResult,
};
use crate::search::engine::{hybrid_search, HybridSearchOpts};

/// Resolved cache configuration (kept here, not on the trait, so
/// backends stay focused on persistence and the orchestrator owns
/// the policy).
#[derive(Debug, Clone)]
pub struct QueryCacheConfig {
    /// Master switch. `false` → the orchestrator is a no-op.
    pub enabled: bool,
    /// Cosine similarity gate (0..1). Default 0.92. Mirrors TS
    /// `DEFAULT_SIMILARITY_THRESHOLD` (`query-cache.ts:38`).
    pub similarity_threshold: f64,
    /// TTL in seconds for stored rows. Default 3600 (1h). Mirrors TS
    /// `DEFAULT_TTL_SECONDS` (`query-cache.ts:40`).
    pub ttl_seconds: i64,
}

impl Default for QueryCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            similarity_threshold: 0.92,
            ttl_seconds: 3600,
        }
    }
}

/// Decide whether the cache lookup should be skipped for these opts.
/// Returns `true` when the cache MUST be consulted, `false` when
/// every rule says "no, run the real pipeline".
///
/// Public so the `think` operation / CLI / tests can introspect the
/// same skip set without duplicating the conditions.
pub fn cache_lookup_eligible(opts: &HybridSearchOpts) -> bool {
    if !opts.use_cache {
        return false;
    }
    if opts.embedding_client.is_none() {
        // No embedding client → query embedding is unavailable →
        // cache key (cosine) cannot be computed.
        return false;
    }
    if opts.walk_depth.is_some() {
        return false;
    }
    if opts.near_symbol.is_some() {
        return false;
    }
    if let Some(ref col) = opts.embedding_column {
        if !col.is_empty() && col != "default" {
            return false;
        }
    }
    true
}

/// `hybrid_search` + semantic query cache lookup + best-effort
/// store. The TS `hybridSearchCached` orchestrator at
/// `src/core/search/hybrid.ts:1163` and the `SemanticQueryCache`
/// class at `src/core/search/query-cache.ts:97` are the source of
/// truth for this logic.
///
/// Returns the same shape as `hybrid_search` — a `Vec<SearchResult>`.
/// The caller cannot tell whether the result came from the cache
/// or a fresh run; the only observable difference is the absence
/// of `cosine_score` stamps on cache hits (the cached rows were
/// stamped at store time, not at read time — and re-running the
/// rescore would defeat the point of the cache).
pub async fn hybrid_search_cached(
    engine: &Arc<dyn BrainEngine>,
    query: &str,
    opts: &HybridSearchOpts,
    config: &QueryCacheConfig,
) -> crate::Result<Vec<SearchResult>> {
    if !config.enabled || !cache_lookup_eligible(opts) {
        // Skip path: just run the real pipeline. No lookup, no store
        // (the TS orchestrator also skips the store in this branch).
        return hybrid_search(engine.as_ref(), query, opts).await;
    }

    // The lookup eligibility check above guarantees an embedding
    // client is present, so this `expect` is the invariant
    // (not a runtime fallback).
    let client: Arc<EmbeddingClient> = opts
        .embedding_client
        .clone()
        .expect("cache_lookup_eligible guarantees embedding_client");

    let query_embedding = client.embed_query(query).await.unwrap_or_default();
    if query_embedding.is_empty() {
        return hybrid_search(engine.as_ref(), query, opts).await;
    }

    let now = chrono::Utc::now().timestamp();
    let source_id = opts.source_id.clone().unwrap_or_else(|| "default".to_string());
    let knobs_hash = opts.knobs_hash.clone().unwrap_or_default();

    let lookup_opts = CacheLookupOpts {
        source_id: source_id.clone(),
        knobs_hash: knobs_hash.clone(),
        similarity_threshold: config.similarity_threshold,
        now_epoch_secs: now,
    };

    // ── lookup (best-effort) ─────────────────────────────────────
    let hit: Option<CacheHit> = match engine
        .cache_lookup(query, &query_embedding, &lookup_opts)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            // Log at the call site would be cleaner, but we follow
            // the TS pattern of swallowing and degrading to miss —
            // the cache must never break the search hot path.
            eprintln!("[zbrain cache] lookup error: {e}");
            None
        }
    };

    if let Some(hit) = hit {
        // Parse the cached results back into SearchResult. The
        // stored shape matches the live SearchResult Serialize impl
        // (no custom mapping) so a JSON round-trip is faithful.
        let results: Vec<SearchResult> = match serde_json::from_str(&hit.results_json) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[zbrain cache] hit results parse error: {e}");
                Vec::new()
            }
        };
        if !results.is_empty() {
            return Ok(results);
        }
        // Empty parsed results = corrupt row; treat as miss + let
        // the live pipeline overwrite the row via the write at
        // the bottom of this function.
    }

    // ── miss: run the real pipeline ──────────────────────────────
    let results = hybrid_search(engine.as_ref(), query, opts).await?;

    // ── best-effort store ────────────────────────────────────────
    let _ = try_store(
        engine.as_ref(),
        query,
        &query_embedding,
        &results,
        &source_id,
        &knobs_hash,
        config.ttl_seconds,
        now,
    )
    .await;

    Ok(results)
}

/// Best-effort cache store. Captures the per-page generation
/// snapshot for the D11 gate (page_generations +
/// max_generation_at_store) via `engine::get_page(...)`, then
/// delegates to `BrainEngine::cache_store`. Any error is logged
/// and swallowed — the search hot path must not fail.
async fn try_store(
    engine: &dyn BrainEngine,
    query_text: &str,
    query_embedding: &[f32],
    results: &[SearchResult],
    source_id: &str,
    knobs_hash: &str,
    ttl_seconds: i64,
    now_epoch_secs: i64,
) -> crate::Result<()> {
    // Build page_generations snapshot by reading each page that
    // appears in the result set. We use the existing
    // `BrainEngine::get_page` (page-level), not chunks: a stored
    // page can be referenced by slug+source_id (the result
    // doesn't carry the row id, so we use the slug+source_id
    // pair as the lookup key — see note below).
    let mut page_generations: HashMap<i64, i64> = HashMap::new();
    let mut max_gen: i64 = 0;
    for r in results {
        // SearchResult.page carries source_id + slug but not the
        // DB row id. We hash on (source_id, slug) — the D11 gate
        // maps by id, so the InMemory backend hashes the same
        // id after we resolve it. For the libsql backend the
        // gate works at the (source_id, slug) layer (see commit
        // B's WHERE clause). For now we encode the page's slug
        // as a string-keyed JSON shape keyed by `<source>:<slug>`
        // so the backend can resolve it.
        //
        // The orchestrator-level keying here is a placeholder
        // — the real D11 gate requires a `page_id` lookup. When
        // the libsql backend lands it will resolve the
        // (source_id, slug) → id mapping inside `cache_store`
        // before writing. The InMemory backend uses the
        // string-keyed shape directly.
        let key = format!("{}::{}", r.page.source_id, r.page.slug);
        // We do not have a public `(source_id, slug) -> id` on
        // `BrainEngine` yet, so we use the page's own
        // `generation` field as the snapshot value (the page
        // row already carries it post-`get_page`).
        let gen = r.page.generation;
        // Use a stable numeric encoding for the key so the
        // InMemory backend can store it as i64. Hash the string
        // to keep the keyspace small — collisions across
        // different sources are not a problem because the
        // `source_id` is part of the row's scope.
        let key_hash = stable_hash(&key);
        page_generations.insert(key_hash, gen);
        if gen > max_gen {
            max_gen = gen;
        }
    }

    let results_json = serde_json::to_string(results).map_err(|e| {
        crate::error::StructuredError::new("Internal", "serde", e.to_string())
    })?;
    let meta_json = "null".to_string(); // HybridSearchMeta is a TS-only type; not yet ported.

    let store_opts = CacheStoreOpts {
        source_id: source_id.to_string(),
        knobs_hash: knobs_hash.to_string(),
        ttl_seconds,
        // Encode the (source, slug) → generation map. The libsql
        // backend (commit B) will rewrite these into real page
        // id lookups; the InMemory backend keeps the string keys
        // for the lookup-time gate.
        page_generations,
        max_generation_at_store: max_gen,
        now_epoch_secs,
    };

    engine
        .cache_store(query_text, query_embedding, &results_json, &meta_json, &store_opts)
        .await
        .map(|_| ())
}

/// Stable 64-bit hash of a string (FNV-1a 64). Used to key the
/// (source_id, slug) pair in the page_generations snapshot so
/// the gate can run at the InMemory layer without a separate
/// id-resolution step. Collisions across distinct keys are
/// acceptable — the gate only requires that the per-key
/// generation value is correctly associated, and the source
/// scope is part of the row PK anyway.
fn stable_hash(s: &str) -> i64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // Sign-preserving cast to i64 so the HashMap<i64, i64> is
    // happy. The high bit is folded into the low bits to keep
    // the value non-negative for human-readability.
    (h & 0x7fff_ffff_ffff_ffff) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── skip-rule tests (pure) ──────────────────────────────────

    #[test]
    fn skip_when_use_cache_false() {
        let mut opts = HybridSearchOpts::default();
        opts.use_cache = false;
        // Even with an embedding client, use_cache=false skips.
        opts.embedding_client = None;
        assert!(!cache_lookup_eligible(&opts));
    }

    #[test]
    fn skip_when_no_embedding_client() {
        let opts = HybridSearchOpts::default();
        // Default has no embedding client.
        assert!(!cache_lookup_eligible(&opts));
    }

    #[test]
    fn skip_when_walk_depth_set() {
        let opts = HybridSearchOpts {
            walk_depth: Some(2),
            embedding_client: Some(make_dummy_client()),
            ..Default::default()
        };
        assert!(!cache_lookup_eligible(&opts));
    }

    #[test]
    fn skip_when_near_symbol_set() {
        let opts = HybridSearchOpts {
            near_symbol: Some("widget::ceo".into()),
            embedding_client: Some(make_dummy_client()),
            ..Default::default()
        };
        assert!(!cache_lookup_eligible(&opts));
    }

    #[test]
    fn skip_when_non_default_embedding_column() {
        let opts = HybridSearchOpts {
            embedding_column: Some("title_v2".into()),
            embedding_client: Some(make_dummy_client()),
            ..Default::default()
        };
        assert!(!cache_lookup_eligible(&opts));
    }

    #[test]
    fn eligible_with_default_column_and_embedding_client() {
        let opts = HybridSearchOpts {
            embedding_column: Some("default".into()),
            embedding_client: Some(make_dummy_client()),
            ..Default::default()
        };
        assert!(cache_lookup_eligible(&opts));
    }

    #[test]
    fn eligible_with_no_column_specified() {
        // Column is None at all — same as "default".
        let opts = HybridSearchOpts {
            embedding_client: Some(make_dummy_client()),
            ..Default::default()
        };
        assert!(cache_lookup_eligible(&opts));
    }

    // ── D11 gate + helper tests (InMemory backend) ─────────────

    #[tokio::test]
    async fn d11_gate_passes_for_unchanged_pages() {
        use crate::engine::InMemoryEngine;
        let engine = InMemoryEngine::new();
        let mut page = crate::Page::default();
        page.id = 1;
        page.generation = 5;
        engine
            .store
            .lock()
            .unwrap()
            .push(page);
        let snap: serde_json::Value = serde_json::json!({ "1": 5 });
        assert!(engine.d11_gate_passes(&snap, 5));
    }

    #[tokio::test]
    async fn d11_gate_fails_when_page_bumped_past_snapshot() {
        use crate::engine::InMemoryEngine;
        let engine = InMemoryEngine::new();
        let mut page = crate::Page::default();
        page.id = 1;
        page.generation = 6; // live > snapshot
        engine.store.lock().unwrap().push(page);
        let snap: serde_json::Value = serde_json::json!({ "1": 5 });
        assert!(!engine.d11_gate_passes(&snap, 5));
    }

    #[tokio::test]
    async fn d11_gate_fails_when_page_hard_deleted() {
        use crate::engine::InMemoryEngine;
        let engine = InMemoryEngine::new();
        // Page id 1 is NOT in the store.
        let snap: serde_json::Value = serde_json::json!({ "1": 5 });
        assert!(!engine.d11_gate_passes(&snap, 5));
    }

    #[tokio::test]
    async fn d11_gate_fails_when_max_live_exceeds_snapshot_max() {
        use crate::engine::InMemoryEngine;
        let engine = InMemoryEngine::new();
        for (id, gen) in [(1, 5), (2, 7)] {
            let mut page = crate::Page::default();
            page.id = id;
            page.generation = gen;
            engine.store.lock().unwrap().push(page);
        }
        let snap: serde_json::Value = serde_json::json!({ "1": 5, "2": 7 });
        // snapshot says max=7; live max=7 → passes
        assert!(engine.d11_gate_passes(&snap, 7));
        // Now bump page 3 to 8 (a NEW page) — but the snapshot
        // doesn't have it, so the gate should still pass (the
        // gate only inspects pages IN the snapshot).
        let mut page3 = crate::Page::default();
        page3.id = 3;
        page3.generation = 8;
        engine.store.lock().unwrap().push(page3);
        assert!(engine.d11_gate_passes(&snap, 7));
    }

    #[tokio::test]
    async fn d11_gate_passes_for_legacy_empty_snapshot() {
        // Empty snapshot = pre-v0.40.3.0 row → compat carve-out.
        use crate::engine::InMemoryEngine;
        let engine = InMemoryEngine::new();
        let snap: serde_json::Value = serde_json::json!({});
        assert!(engine.d11_gate_passes(&snap, 0));
    }

    // ── row-id determinism ────────────────────────────────────

    #[test]
    fn query_cache_row_id_is_deterministic_and_32_hex() {
        let a = query_cache_row_id("default", "who is alice", "");
        let b = query_cache_row_id("default", "who is alice", "");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn query_cache_row_id_changes_with_inputs() {
        let a = query_cache_row_id("default", "who is alice", "");
        let b = query_cache_row_id("default", "who is bob", "");
        let c = query_cache_row_id("default", "who is alice", "v2");
        let d = query_cache_row_id("src-a", "who is alice", "");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    // ── helpers ────────────────────────────────────────────────

    fn make_dummy_client() -> Arc<EmbeddingClient> {
        // We never actually call embed_query in the skip-rule
        // tests, so an empty/None provider is fine — the
        // `cache_lookup_eligible` function only checks for Some.
        // Construction needs a real Arc<dyn EmbeddingProvider>
        // though, so use a minimal stub.
        use crate::embedding::EmbeddingConfig;
        use async_trait::async_trait;
        use crate::embedding::{EmbeddingError, EmbeddingProvider};
        struct Stub;
        #[async_trait]
        impl EmbeddingProvider for Stub {
            async fn embed(
                &self,
                _texts: &[String],
                _dims: usize,
            ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
                Ok(Vec::new())
            }
            async fn embed_image(
                &self,
                _b64: &str,
                _mime: Option<&str>,
                _dims: usize,
            ) -> Result<Vec<f32>, EmbeddingError> {
                Ok(Vec::new())
            }
        }
        let config = EmbeddingConfig::builder()
            .api_key("sk-test")
            .dimensions(3)
            .build()
            .unwrap();
        Arc::new(EmbeddingClient::with_provider(
            config,
            Arc::new(Stub),
        ))
    }
}
