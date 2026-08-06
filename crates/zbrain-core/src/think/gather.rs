//! GATHER phase for `zbrain think` (Rust port of `src/core/think/gather.ts`).
//!
//! Runs four retrievers in parallel and fuses them:
//!   1. `hybrid`    — page-grain hybrid search (vector + keyword + RRF).
//!   2. `takes_kw`  — keyword search across active takes.
//!   3. `takes_vec` — vector search across active takes. **Blocked (G71):**
//!      the Rust `takes` table has no embedding column yet (migration
//!      `0012_takes_full_columns.sql` defers it: "needs pgvector setup") and
//!      `BrainEngine::search_takes_vector` is not implemented, so this stream
//!      is plumbed (gated on `question_embedding`) but currently returns empty.
//!   4. `graph`     — anchor-entity subgraph traversal (skipped when no anchor).
//!
//! Each retriever is wrapped in a `catch` so a single stream failure can't
//! crash the whole pipeline — synthesis with partial gather results is more
//! useful than no synthesis at all (mirrors the TS `runGather` contract).

use crate::engine::{BrainEngine, SearchResult};
use crate::embedding::EmbeddingClient;
use crate::search::engine::{hybrid_search, HybridSearchOpts};
use crate::think::fusion::fuse_ranked;
use crate::types::{SearchTakesOpts, TakeHit};
use std::collections::HashSet;
use std::sync::Arc;

/// Default soft cap on total page results across all streams.
pub const GATHER_LIMIT_DEFAULT: usize = 40;
/// Default soft cap on take results.
pub const TAKES_LIMIT_DEFAULT: usize = 30;
/// Default graph traversal depth when an anchor is set.
pub const GRAPH_DEPTH_DEFAULT: u32 = 2;
/// Default excerpt length (chars) for `render_pages_block`.
pub const PAGE_EXCERPT_LEN: usize = 600;

/// Options for [`run_gather`].
///
/// Faithful port of TS `ThinkGatherOpts`. Note `question_embedding` and
/// `embedding_client` are both optional: stream 1 (hybrid) needs the embedding
/// client to embed the query; stream 3 (vector takes) needs a pre-computed
/// `question_embedding` — and is currently a no-op until G71 lands.
#[derive(Clone, Default)]
pub struct ThinkGatherOpts {
    pub question: String,
    /// Anchor entity slug. When set, the graph stream activates.
    pub anchor: Option<String>,
    /// Soft cap on total page results. Default [`GATHER_LIMIT_DEFAULT`].
    pub gather_limit: Option<usize>,
    /// Soft cap on take results. Default [`TAKES_LIMIT_DEFAULT`].
    pub takes_limit: Option<usize>,
    /// Graph traversal depth when anchor is set. Default [`GRAPH_DEPTH_DEFAULT`].
    pub graph_depth: Option<u32>,
    /// Pre-computed question embedding. Enables stream 3 (vector takes).
    /// **G71**: currently consumed only to flag the blocked path; the vector
    /// stream still returns empty until `BrainEngine::search_takes_vector` exists.
    pub question_embedding: Option<Vec<f32>>,
    /// Embedding client for stream 1 (hybrid page search) vector-retrieval path.
    pub embedding_client: Option<Arc<EmbeddingClient>>,
    /// Per-token allow-list for the `holder` field on take search.
    pub takes_holders_allow_list: Option<Vec<String>>,
}

/// Diagnostics for telemetry / `--explain` path.
#[derive(Debug, Clone, Default)]
pub struct ThinkGatherDiagnostics {
    pub pages_from_hybrid: usize,
    pub takes_from_keyword: usize,
    pub takes_from_vector: usize,
    pub graph_hits: usize,
    /// `'expansion'` when the question was rewritten for prompt safety, else `'none'`.
    /// Rust think gather does not run query-expansion sanitization, so this is
    /// always `'none'` for now (the expansion path is a separate node).
    pub question_sanitized_for: &'static str,
}

/// Result of [`run_gather`] — page hits and take hits as separate lists so the
/// synth step can render them into distinct `<pages>` / `<takes>` blocks.
#[derive(Debug, Clone, Default)]
pub struct ThinkGatherResult {
    /// Page hits, ranked by RRF-fused score.
    pub pages: Vec<SearchResult>,
    /// Take hits, ranked + dedup'd by `(slug, row)`.
    pub takes: Vec<TakeHit>,
    /// Graph nodes — slugs reachable from anchor within graph depth. Empty when no anchor.
    pub graph_slugs: Vec<String>,
    /// Diagnostics for telemetry / `--explain`.
    pub diagnostics: ThinkGatherDiagnostics,
}

/// Run the four-stream think gather.
///
/// Port of `src/core/think/gather.ts:runGather`. Each stream is fail-open: a
/// retriever error yields an empty list rather than aborting the pipeline.
pub async fn run_gather(engine: &dyn BrainEngine, opts: &ThinkGatherOpts) -> ThinkGatherResult {
    let gather_limit = opts.gather_limit.unwrap_or(GATHER_LIMIT_DEFAULT);
    let takes_limit = opts.takes_limit.unwrap_or(TAKES_LIMIT_DEFAULT);
    let graph_depth = opts.graph_depth.unwrap_or(GRAPH_DEPTH_DEFAULT);

    // Stream 1: hybrid page search (existing primitive).
    let pages = hybrid_search(
        engine,
        &opts.question,
        &HybridSearchOpts {
            limit: Some(gather_limit),
            embedding_client: opts.embedding_client.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|e| {
        eprintln!("[think.gather] hybrid stream failed: {e}");
        Vec::new()
    });

    // Stream 2: keyword search across takes.
    let takes_kw = engine
        .search_takes(
            &opts.question,
            &SearchTakesOpts {
                limit: Some(takes_limit as u32),
                takes_holders_allow_list: opts.takes_holders_allow_list.clone(),
            },
        )
        .await
        .unwrap_or_else(|e| {
            eprintln!("[think.gather] takes-keyword stream failed: {e}");
            Vec::new()
        });

    // Stream 3: vector search across takes.
    //
    // FUTURE(G71): blocked — the Rust `takes` table has no embedding column yet
    // (migration 0012_takes_full_columns.sql:22 defers it: "needs pgvector
    // setup") and `BrainEngine::search_takes_vector` is not implemented. In TS
    // this stream is gated on `questionEmbedding`; the `question_embedding` opt
    // is retained here for API parity. Once G71 lands, replace the empty
    // fallback with:
    //   engine.search_takes_vector(embedding, &SearchTakesOpts {
    //       limit: Some(takes_limit as u32),
    //       takes_holders_allow_list: opts.takes_holders_allow_list.clone(),
    //   }).await.unwrap_or_else(|e| { eprintln!(...); Vec::new() })
    let takes_vec: Vec<TakeHit> = Vec::new();

    // Stream 4: graph walk (anchor only).
    let graph_slugs: Vec<String> = match &opts.anchor {
        Some(anchor) => match engine
            .traverse_paths(anchor, Some(graph_depth), None, Some("both"), None, None)
            .await
        {
            Ok(paths) => {
                let mut slugs: HashSet<String> = HashSet::new();
                slugs.insert(anchor.clone());
                for p in &paths {
                    slugs.insert(p.from_slug.clone());
                    slugs.insert(p.to_slug.clone());
                }
                slugs.into_iter().collect()
            }
            Err(e) => {
                eprintln!("[think.gather] graph stream failed: {e}");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    // Fuse takes streams (keyword + vector). Key by `(page_slug, row_num)`.
    let fused_takes = fuse_ranked(&takes_kw, &takes_vec, |h: &TakeHit| {
        format!("{}#{}", h.page_slug, h.row_num)
    })
    .into_iter()
    .take(takes_limit)
    .collect::<Vec<_>>();

    // Capture counts before moving `pages` / `graph_slugs` into the result.
    let pages_len = pages.len();
    let graph_len = graph_slugs.len();

    ThinkGatherResult {
        pages: pages.into_iter().take(gather_limit).collect(),
        takes: fused_takes,
        graph_slugs,
        diagnostics: ThinkGatherDiagnostics {
            pages_from_hybrid: pages_len,
            takes_from_keyword: takes_kw.len(),
            takes_from_vector: takes_vec.len(),
            graph_hits: graph_len,
            question_sanitized_for: "none",
        },
    }
}

/// Render page hits into the `<page slug="..." rank="...">excerpt</page>` block
/// the prompt builder consumes.
///
/// Port of `src/core/think/gather.ts:renderPagesBlock`. Rust `SearchResult` is
/// page-level (no `chunk_text`), so the excerpt source order is
/// `snippet ?? compiled_truth`. Truncation is char-based (UTF-8 safe), whereas
/// TS slices by UTF-16 code units — semantically equivalent for normal text.
pub fn render_pages_block(pages: &[SearchResult], excerpt_len: usize) -> String {
    pages
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let slug = &p.page.slug;
            let raw = p
                .snippet
                .clone()
                .or_else(|| Some(p.page.compiled_truth.clone()))
                .unwrap_or_default();
            let excerpt: String = raw.chars().take(excerpt_len).collect();
            format!("<page slug=\"{slug}\" rank=\"{}\">\n{excerpt}\n</page>", idx + 1)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Map a `TakeHit` into the prompt shape the synth step renders.
///
/// Port of `src/core/think/gather.ts:takesHitToTakeForPrompt`. Returns the
/// canonical [`crate::think::sanitize::TakeForPrompt`] (the same struct
/// `render_takes_block` consumes) so the gather + sanitize steps share one
/// `TakeForPrompt` type, mirroring the single TS interface. `source` /
/// `since_date` are `None` for a `TakeHit` (they exist only on a full `Take`).
pub fn takes_hit_to_take_for_prompt(h: &TakeHit) -> crate::think::sanitize::TakeForPrompt {
    crate::think::sanitize::TakeForPrompt {
        page_slug: h.page_slug.clone(),
        row_num: h.row_num as i64,
        claim: h.claim.clone(),
        kind: h.kind.clone(),
        holder: h.holder.clone(),
        weight: h.weight,
        source: None,
        since_date: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{InMemoryEngine, Page};

    #[tokio::test]
    async fn empty_brain_yields_empty_gather() {
        let engine = InMemoryEngine::default();
        let result = run_gather(
            &engine,
            &ThinkGatherOpts {
                question: "anything".into(),
                ..Default::default()
            },
        )
        .await;
        assert!(result.pages.is_empty());
        assert!(result.takes.is_empty());
        assert!(result.graph_slugs.is_empty());
        assert_eq!(result.diagnostics.pages_from_hybrid, 0);
        assert_eq!(result.diagnostics.takes_from_keyword, 0);
        assert_eq!(result.diagnostics.question_sanitized_for, "none");
    }

    #[tokio::test]
    async fn anchor_triggers_graph_stream_without_panic() {
        // InMemoryEngine.traverse_paths returns Unsupported -> caught -> empty.
        let engine = InMemoryEngine::default();
        let result = run_gather(
            &engine,
            &ThinkGatherOpts {
                question: "q".into(),
                anchor: Some("people/alice".into()),
                ..Default::default()
            },
        )
        .await;
        // No panic; graph stream failed open to empty.
        assert!(result.graph_slugs.is_empty());
    }

    #[test]
    fn takes_hit_to_take_for_prompt_maps_fields() {
        let hit = TakeHit {
            take_id: 7,
            page_id: 3,
            page_slug: "companies/acme".into(),
            row_num: 2,
            claim: "ACME is profitable".into(),
            kind: "fact".into(),
            holder: "local".into(),
            weight: 0.9,
            score: 0.0,
        };
        let out = takes_hit_to_take_for_prompt(&hit);
        assert_eq!(out.page_slug, "companies/acme");
        assert_eq!(out.row_num, 2i64);
        assert_eq!(out.claim, "ACME is profitable");
        assert_eq!(out.kind, "fact");
        assert_eq!(out.holder, "local");
        assert!((out.weight - 0.9).abs() < 1e-9);
        // TakeHit carries no source/since_date -> None.
        assert!(out.source.is_none());
        assert!(out.since_date.is_none());
    }

    #[test]
    fn render_pages_block_renders_slug_rank_and_excerpt() {
        let mut page = Page::default();
        page.slug = "people/alice".into();
        page.compiled_truth = "Alice founded the lab in 2021.".into();
        let result = SearchResult {
            page,
            score: 0.9,
            base_score: 0.9,
            snippet: Some("Alice founded the lab".into()),
            rerank_score: None,
            reranker_delta: None,
            salience_boost: None,
            recency_boost: None,
        };
        let block = render_pages_block(&[result], PAGE_EXCERPT_LEN);
        assert!(block.contains("<page slug=\"people/alice\" rank=\"1\">"));
        assert!(block.contains("Alice founded the lab"));
        assert!(block.contains("</page>"));
    }

    #[test]
    fn render_pages_block_falls_back_to_compiled_truth() {
        let mut page = Page::default();
        page.slug = "x".into();
        page.compiled_truth = "fallback body".into();
        let result = SearchResult {
            page,
            score: 0.5,
            base_score: 0.5,
            snippet: None,
            rerank_score: None,
            reranker_delta: None,
            salience_boost: None,
            recency_boost: None,
        };
        let block = render_pages_block(&[result], PAGE_EXCERPT_LEN);
        assert!(block.contains("fallback body"));
        assert!(!block.contains("rank=\"2\">"));
    }

    #[test]
    fn render_pages_block_truncates_excerpt_by_chars() {
        let mut page = Page::default();
        page.slug = "x".into();
        page.compiled_truth = "a".repeat(1000);
        let result = SearchResult {
            page,
            score: 0.5,
            base_score: 0.5,
            snippet: None,
            rerank_score: None,
            reranker_delta: None,
            salience_boost: None,
            recency_boost: None,
        };
        let block = render_pages_block(&[result], 10);
        // header + newline + 10 chars + newline + close == contains at most 10 excerpt chars.
        let excerpt = block
            .trim_start_matches("<page slug=\"x\" rank=\"1\">\n")
            .trim_end_matches("\n</page>");
        assert_eq!(excerpt.chars().count(), 10);
    }
}
