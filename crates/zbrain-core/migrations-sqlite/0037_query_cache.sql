-- G69-B (1-5-17): semantic query cache persistence for the libsql backend.
-- Mirrors the InMemory `query_cache_store` row shape (`InternalQueryCacheRow`
-- in `engine.rs`) so a round-trip between backends preserves the stored row
-- verbatim. The orchestrator is the only writer (via `BrainEngine::cache_store`),
-- and `cache_lookup` is the only reader; both go through the trait so this
-- table is backend-agnostic JSON-in-TEXT for `results_json` / `meta_json` /
-- `page_generations` (the same wire format the InMemory engine keeps as String).
--
-- `embedding` is a f32 little-endian BLOB — identical encoding to
-- `pages.embedding` (G24) — so `decode_embedding_le` is the single decoder
-- for the cosine scan in `cache_lookup`.
--
-- `page_generations` is a JSON object `{"<stable_hash(source::slug)>": <gen>}`
-- keyed by the orchestrator's `stable_hash` (see `search::cache`). SQLite has
-- no vector / hash op, so `cache_lookup` recomputes those hashes in app code
-- to run the D11 two-layer gate (mirrors `engine.rs::d11_gate_passes`).
CREATE TABLE IF NOT EXISTS query_cache (
    id TEXT PRIMARY KEY,            -- sha256(sourceId::queryText::knobsHash)[:32]
    query_text TEXT NOT NULL,
    source_id TEXT NOT NULL,
    knobs_hash TEXT NOT NULL,
    embedding BLOB,                 -- f32 LE bytes (NULL only for malformed rows)
    results_json TEXT NOT NULL,     -- serialized Vec<SearchResult>
    meta_json TEXT NOT NULL,        -- serialized HybridSearchMeta (may be "null")
    ttl_seconds INTEGER NOT NULL,
    page_generations TEXT NOT NULL, -- JSON: { "<hash>": <generation> }
    max_generation_at_store INTEGER NOT NULL,
    created_at_epoch INTEGER NOT NULL,
    last_hit_at_epoch INTEGER,      -- NULL until first hit
    hit_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_query_cache_src_knobs
    ON query_cache (source_id, knobs_hash);
