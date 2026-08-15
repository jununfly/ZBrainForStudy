//! Resumable symbol-edge resolution backfill (G77 / 1-6-3).
//!
//! Ports TS `resolveSymbolEdgesIncremental` + `processChunkBatch` from
//! `src/core/chunkers/symbol-resolver.ts`. The symbol edges themselves are
//! emitted into `code_edges_symbol` by the edge-extractor during sync
//! (`add_code_edges` in libsql.rs / postgres.rs). This module is the
//! second-pass resolver: for each unresolved edge it looks up chunks in the
//! same page whose `symbol_name_qualified` matches the edge's
//! `to_symbol_qualified`, and records the outcome in `edge_metadata`
//! (`resolved_chunk_id` / `ambiguous` + `candidates`).
//!
//! ## Dialect neutrality
//!
//! All SQL uses `$N` positional placeholders — libsql binds them
//! positionally, so `$N` works on both engines. Integer id lists are
//! embedded as `IN (...)` literals (the ids come from trusted DB query
//! results, never user input). The watermark timestamp is passed as an
//! ISO-8601 string parameter (no engine-specific `NOW()`), and the
//! `edge_metadata` merge is performed in Rust (no `|| jsonb` operator), so
//! the same code runs unchanged against postgres (`JSONB`) and libsql
//! (`TEXT` json).

use crate::engine::BrainEngine;
use crate::Result;
use chrono::Utc;
use erased_serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Bump whenever the extractor or resolver shape changes. Rows resolved by
/// an OLDER watermark are re-walked on the next resolver pass. Mirrors TS
/// `EDGE_EXTRACTOR_VERSION_TS`.
pub const EDGE_EXTRACTOR_VERSION: &str = "2026-05-14T01:00:00Z";

/// Chunks per transaction; one batch is the atomic unit. Mirrors TS `BATCH_SIZE`.
pub const BATCH_SIZE: usize = 200;

/// Counters returned by [`resolve_symbol_edges_incremental`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResolverStats {
    pub chunks_walked: u64,
    pub edges_examined: u64,
    pub edges_resolved: u64,
    pub edges_ambiguous: u64,
    pub edges_unmatched: u64,
    pub batches: u64,
    pub ms: u64,
}

/// Options for [`resolve_symbol_edges_incremental`].
#[derive(Debug, Clone)]
pub struct ResolverOpts {
    /// Required: scope resolution to one source.
    pub source_id: String,
    /// Cap on chunks walked per call. Default: [`BATCH_SIZE`] * 10.
    pub max_chunks: Option<usize>,
}

#[derive(Debug, Clone)]
struct UnresolvedEdgeRow {
    id: i64,
    from_chunk_id: i64,
    to_symbol_qualified: String,
    edge_type: String,
    edge_metadata: Value,
}

#[derive(Debug, Clone)]
struct ChunkRow {
    id: i64,
    page_id: i64,
}

/// Outcome of resolving a single edge, as recorded in `edge_metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeResolution {
    Resolved(i64),
    Ambiguous(Vec<i64>),
    Unresolved,
}

/// Resolve unresolved edges for chunks whose `edges_backfilled_at` is stale
/// or null. Returns stats; updates the DB in [`BATCH_SIZE`]-chunk
/// transactions. Crashes lose at most one batch (the watermark is only
/// bumped after a batch's edges are persisted).
pub async fn resolve_symbol_edges_incremental(
    engine: &dyn BrainEngine,
    opts: &ResolverOpts,
) -> Result<ResolverStats> {
    let start = Utc::now();
    let max_chunks = opts.max_chunks.unwrap_or(BATCH_SIZE * 10);
    let mut stats = ResolverStats::default();
    let now_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut processed: usize = 0;
    while processed < max_chunks {
        let remaining = max_chunks - processed;
        let batch_size = (remaining.min(BATCH_SIZE)) as i64;

        // 1) Find chunks that need walking, scoped to source.
        let chunks = find_chunks_needing_walk(engine, &opts.source_id, batch_size).await?;
        if chunks.is_empty() {
            break;
        }

        process_chunk_batch(engine, &opts.source_id, &chunks, &now_ts, &mut stats).await?;

        stats.batches += 1;
        processed += chunks.len();
    }

    stats.ms = (Utc::now() - start)
        .num_milliseconds()
        .max(0) as u64;
    Ok(stats)
}

async fn find_chunks_needing_walk(
    engine: &dyn BrainEngine,
    source_id: &str,
    batch_size: i64,
) -> Result<Vec<ChunkRow>> {
    let sql = "\
SELECT cc.id AS id, cc.page_id AS page_id \
  FROM content_chunks cc \
  JOIN pages p ON p.id = cc.page_id \
 WHERE p.source_id = $1 \
   AND (cc.edges_backfilled_at IS NULL OR cc.edges_backfilled_at < $2) \
 ORDER BY cc.id \
 LIMIT $3";
    let p_source: &(dyn Serialize + Sync) = &source_id;
    let p_version: &(dyn Serialize + Sync) = &EDGE_EXTRACTOR_VERSION;
    let p_size: &(dyn Serialize + Sync) = &batch_size;
    let params: &[&(dyn Serialize + Sync)] = &[p_source, p_version, p_size];
    let rows = engine.execute_raw(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let id = r.get("id").and_then(|v| v.as_i64());
        let page_id = r.get("page_id").and_then(|v| v.as_i64());
        match (id, page_id) {
            (Some(id), Some(page_id)) => out.push(ChunkRow { id, page_id }),
            _ => continue,
        }
    }
    Ok(out)
}

async fn process_chunk_batch(
    engine: &dyn BrainEngine,
    source_id: &str,
    chunks: &[ChunkRow],
    now_ts: &str,
    stats: &mut ResolverStats,
) -> Result<()> {
    let chunk_ids: Vec<i64> = chunks.iter().map(|c| c.id).collect();
    let page_by_chunk: HashMap<i64, i64> = chunks.iter().map(|c| (c.id, c.page_id)).collect();

    // 2) Load unresolved edges for the batch (IN list literal: no array param,
    //    works on both dialects).
    let edges = load_edges(engine, source_id, &chunk_ids).await?;

    // 3) Candidate chunks with a qualified symbol name, per page.
    let mut page_ids: Vec<i64> = chunks.iter().map(|c| c.page_id).collect();
    page_ids.sort_unstable();
    page_ids.dedup();
    let candidates = load_candidates(engine, &page_ids).await?;

    // 4) Resolve (pure).
    let (to_resolve, to_ambiguous, delta) = resolve_edges(&edges, &candidates, &page_by_chunk);
    stats.edges_examined += delta.edges_examined;
    stats.edges_resolved += delta.edges_resolved;
    stats.edges_ambiguous += delta.edges_ambiguous;
    stats.edges_unmatched += delta.edges_unmatched;

    // 5) Persist metadata (merge in Rust, dialect-neutral).
    for (edge_id, chunk_id) in &to_resolve {
        let existing = edges.iter().find(|e| e.id == *edge_id).map(|e| &e.edge_metadata);
        let mut patch = Map::new();
        patch.insert("resolved_chunk_id".to_string(), json!(chunk_id));
        let merged = merge_metadata(existing, patch);
        update_edge_metadata(engine, *edge_id, &merged).await?;
    }
    for (edge_id, cand) in &to_ambiguous {
        let existing = edges.iter().find(|e| e.id == *edge_id).map(|e| &e.edge_metadata);
        let mut patch = Map::new();
        patch.insert("ambiguous".to_string(), Value::Bool(true));
        patch.insert("candidates".to_string(), json!(cand));
        let merged = merge_metadata(existing, patch);
        update_edge_metadata(engine, *edge_id, &merged).await?;
    }

    // 6) Bump watermark for the whole batch — regardless of whether any edges
    //    resolved (a zero-edge chunk still must be marked or it re-walks forever).
    update_watermark(engine, &chunk_ids, now_ts).await?;

    stats.chunks_walked += chunks.len() as u64;
    Ok(())
}

async fn load_edges(
    engine: &dyn BrainEngine,
    source_id: &str,
    chunk_ids: &[i64],
) -> Result<Vec<UnresolvedEdgeRow>> {
    if chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT id, from_chunk_id, to_symbol_qualified, edge_type, edge_metadata \
           FROM code_edges_symbol \
          WHERE from_chunk_id IN ({}) AND source_id = $1",
        in_list(chunk_ids)
    );
    let p_source: &(dyn Serialize + Sync) = &source_id;
    let params: &[&(dyn Serialize + Sync)] = &[p_source];
    let rows = engine.execute_raw(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let id = r.get("id").and_then(|v| v.as_i64());
        let from_chunk_id = r.get("from_chunk_id").and_then(|v| v.as_i64());
        let to_symbol = r
            .get("to_symbol_qualified")
            .and_then(|v| v.as_str())
            .map(String::from);
        let edge_type = r.get("edge_type").and_then(|v| v.as_str()).map(String::from);
        let meta = r
            .get("edge_metadata")
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        match (id, from_chunk_id, to_symbol, edge_type) {
            (Some(id), Some(fc), Some(ts), Some(et)) => out.push(UnresolvedEdgeRow {
                id,
                from_chunk_id: fc,
                to_symbol_qualified: ts,
                edge_type: et,
                edge_metadata: meta,
            }),
            _ => continue,
        }
    }
    Ok(out)
}

async fn load_candidates(
    engine: &dyn BrainEngine,
    page_ids: &[i64],
) -> Result<HashMap<String, Vec<i64>>> {
    let mut map: HashMap<String, Vec<i64>> = HashMap::new();
    if page_ids.is_empty() {
        return Ok(map);
    }
    let sql = format!(
        "SELECT id, page_id, symbol_name_qualified \
           FROM content_chunks \
          WHERE page_id IN ({}) AND symbol_name_qualified IS NOT NULL",
        in_list(page_ids)
    );
    let rows = engine.execute_raw(&sql, &[]).await?;
    for r in &rows {
        let id = r.get("id").and_then(|v| v.as_i64());
        let page_id = r.get("page_id").and_then(|v| v.as_i64());
        let sym = r.get("symbol_name_qualified").and_then(|v| v.as_str());
        if let (Some(id), Some(page_id), Some(sym)) = (id, page_id, sym) {
            let key = format!("{} {}", page_id, sym);
            map.entry(key).or_default().push(id);
        }
    }
    Ok(map)
}

async fn update_edge_metadata(engine: &dyn BrainEngine, edge_id: i64, merged_json: &str) -> Result<()> {
    let sql = "UPDATE code_edges_symbol SET edge_metadata = $1 WHERE id = $2";
    let p_meta: &(dyn Serialize + Sync) = &merged_json.to_string();
    let p_id: &(dyn Serialize + Sync) = &edge_id;
    let params: &[&(dyn Serialize + Sync)] = &[p_meta, p_id];
    let _ = engine.execute_raw(sql, params).await?;
    Ok(())
}

async fn update_watermark(engine: &dyn BrainEngine, chunk_ids: &[i64], now_ts: &str) -> Result<()> {
    if chunk_ids.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "UPDATE content_chunks SET edges_backfilled_at = $1 WHERE id IN ({})",
        in_list(chunk_ids)
    );
    let p_ts: &(dyn Serialize + Sync) = &now_ts.to_string();
    let params: &[&(dyn Serialize + Sync)] = &[p_ts];
    let _ = engine.execute_raw(&sql, params).await?;
    Ok(())
}

fn in_list(ids: &[i64]) -> String {
    let mut s = String::new();
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&id.to_string());
    }
    s
}

/// Pure resolution: for each edge, look up same-page candidates by
/// `to_symbol_qualified`. Returns `(resolved: edge_id->chunk_id,
/// ambiguous: edge_id->candidate_chunk_ids, stats_delta)`.
fn resolve_edges(
    edges: &[UnresolvedEdgeRow],
    candidates_by_key: &HashMap<String, Vec<i64>>,
    page_by_chunk: &HashMap<i64, i64>,
) -> (Vec<(i64, i64)>, Vec<(i64, Vec<i64>)>, ResolverStats) {
    let mut to_resolve = Vec::new();
    let mut to_ambiguous = Vec::new();
    let mut delta = ResolverStats::default();
    for e in edges {
        delta.edges_examined += 1;
        let page_id = match page_by_chunk.get(&e.from_chunk_id) {
            Some(p) => *p,
            None => {
                delta.edges_unmatched += 1;
                continue;
            }
        };
        let key = format!("{} {}", page_id, e.to_symbol_qualified);
        match candidates_by_key.get(&key) {
            Some(c) if c.len() == 1 => {
                to_resolve.push((e.id, c[0]));
                delta.edges_resolved += 1;
            }
            Some(c) if c.len() > 1 => {
                to_ambiguous.push((e.id, c.clone()));
                delta.edges_ambiguous += 1;
            }
            _ => {
                delta.edges_unmatched += 1;
            }
        }
    }
    (to_resolve, to_ambiguous, delta)
}

fn merge_metadata(existing: Option<&Value>, patch: Map<String, Value>) -> String {
    let mut obj = match existing {
        Some(Value::Object(m)) => m.clone(),
        Some(Value::String(s)) => {
            serde_json::from_str::<Map<String, Value>>(s).unwrap_or_else(|_| Map::new())
        }
        _ => Map::new(),
    };
    for (k, v) in patch {
        obj.insert(k, v);
    }
    serde_json::to_string(&Value::Object(obj)).unwrap()
}

/// Read the resolution outcome from a single edge's `edge_metadata`, if any.
/// Returns [`EdgeResolution::Unresolved`] when the edge hasn't been processed
/// by the resolver yet. Public helper for downstream code (two-pass walk,
/// code_blast op) that wants the resolver's output without parsing
/// `edge_metadata` JSON directly.
pub fn read_edge_resolution(metadata: Option<&Value>) -> EdgeResolution {
    match metadata {
        None => EdgeResolution::Unresolved,
        Some(v) => {
            if let Some(cid) = v.get("resolved_chunk_id").and_then(|x| x.as_i64()) {
                return EdgeResolution::Resolved(cid);
            }
            if v.get("ambiguous").and_then(|x| x.as_bool()) == Some(true) {
                if let Some(arr) = v.get("candidates").and_then(|x| x.as_array()) {
                    let cands: Vec<i64> = arr.iter().filter_map(|x| x.as_i64()).collect();
                    if !cands.is_empty() {
                        return EdgeResolution::Ambiguous(cands);
                    }
                }
            }
            EdgeResolution::Unresolved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn edge(id: i64, from: i64, to: &str) -> UnresolvedEdgeRow {
        UnresolvedEdgeRow {
            id,
            from_chunk_id: from,
            to_symbol_qualified: to.to_string(),
            edge_type: "call".to_string(),
            edge_metadata: Value::Object(Map::new()),
        }
    }

    #[test]
    fn resolve_single_candidate() {
        let edges = vec![edge(10, 1, "Class::foo")];
        let mut cand = HashMap::new();
        cand.insert("5 Class::foo".to_string(), vec![99]);
        let mut page_by = HashMap::new();
        page_by.insert(1, 5);
        let (resolved, ambiguous, delta) = resolve_edges(&edges, &cand, &page_by);
        assert_eq!(delta.edges_examined, 1);
        assert_eq!(delta.edges_resolved, 1);
        assert_eq!(resolved, vec![(10, 99)]);
        assert!(ambiguous.is_empty());
    }

    #[test]
    fn resolve_ambiguous_when_two_candidates() {
        let edges = vec![edge(11, 1, "Class::bar")];
        let mut cand = HashMap::new();
        cand.insert("5 Class::bar".to_string(), vec![7, 8]);
        let mut page_by = HashMap::new();
        page_by.insert(1, 5);
        let (resolved, ambiguous, delta) = resolve_edges(&edges, &cand, &page_by);
        assert_eq!(delta.edges_ambiguous, 1);
        assert!(resolved.is_empty());
        assert_eq!(ambiguous, vec![(11, vec![7, 8])]);
    }

    #[test]
    fn resolve_unmatched_when_no_candidate() {
        let edges = vec![edge(12, 1, "Missing::x")];
        let cand = HashMap::new();
        let mut page_by = HashMap::new();
        page_by.insert(1, 5);
        let (resolved, ambiguous, delta) = resolve_edges(&edges, &cand, &page_by);
        assert_eq!(delta.edges_unmatched, 1);
        assert!(resolved.is_empty());
        assert!(ambiguous.is_empty());
    }

    #[test]
    fn resolve_unmatched_when_chunk_page_unknown() {
        let edges = vec![edge(13, 999, "Class::foo")];
        let mut cand = HashMap::new();
        cand.insert("5 Class::foo".to_string(), vec![99]);
        let page_by = HashMap::new();
        let (resolved, ambiguous, delta) = resolve_edges(&edges, &cand, &page_by);
        assert_eq!(delta.edges_unmatched, 1);
        assert!(resolved.is_empty());
        assert!(ambiguous.is_empty());
    }

    #[test]
    fn read_edge_resolution_variants() {
        assert_eq!(read_edge_resolution(None), EdgeResolution::Unresolved);
        assert_eq!(
            read_edge_resolution(Some(&json!({}))),
            EdgeResolution::Unresolved
        );
        assert_eq!(
            read_edge_resolution(Some(&json!({"resolved_chunk_id": 42}))),
            EdgeResolution::Resolved(42)
        );
        assert_eq!(
            read_edge_resolution(Some(&json!({"ambiguous": true, "candidates": [1, 2, 3]}))),
            EdgeResolution::Ambiguous(vec![1, 2, 3])
        );
        // ambiguous flag true but no candidates -> unresolved
        assert_eq!(
            read_edge_resolution(Some(&json!({"ambiguous": true}))),
            EdgeResolution::Unresolved
        );
    }

    #[test]
    fn in_list_formatting() {
        assert_eq!(in_list(&[1, 2, 3]), "1,2,3");
        assert_eq!(in_list(&[7]), "7");
        assert_eq!(in_list(&[]), "");
    }

    #[test]
    fn merge_metadata_preserves_existing() {
        let existing = json!({"foo": "bar"});
        let mut patch = Map::new();
        patch.insert("resolved_chunk_id".to_string(), json!(5));
        let merged = merge_metadata(Some(&existing), patch);
        let parsed: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(parsed.get("foo").and_then(|v| v.as_str()), Some("bar"));
        assert_eq!(
            parsed.get("resolved_chunk_id").and_then(|v| v.as_i64()),
            Some(5)
        );
    }

    #[test]
    fn merge_metadata_parses_sqlite_text() {
        let existing = Value::String("{\"foo\":\"bar\"}".to_string());
        let mut patch = Map::new();
        patch.insert("ambiguous".to_string(), Value::Bool(true));
        let merged = merge_metadata(Some(&existing), patch);
        let parsed: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(parsed.get("foo").and_then(|v| v.as_str()), Some("bar"));
        assert_eq!(parsed.get("ambiguous").and_then(|v| v.as_bool()), Some(true));
    }
}
