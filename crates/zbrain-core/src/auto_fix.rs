//! Auto-fix library functions for `zbrain features --auto-fix`.
//!
//! Each function is a thin, side-effecting library operation that the
//! `features --auto-fix` command dispatches to. They operate over a
//! `&dyn BrainEngine` plus any supporting client (e.g. `EmbeddingClient`),
//! keeping them unit-testable without the CLI.
//!
//! These are the page-level Rust analogs of the TS auto-fix dispatch in
//! `features.ts` `executeAutoFix`, which called `runEmbed` / `runExtract`
//! in-process. Page-level (not chunk-level) modeling is an explicit decision
//! recorded on the Part11 roadmap node 1-6-4-4.

use crate::embedding::{EmbeddingClient, EmbeddingError};
use crate::engine::{BrainEngine, Page};
use crate::error::{StructuredError, Result};

/// Options for [`embed_stale`].
pub struct EmbedStaleOpts {
    /// When true, enumerate + count stale pages but never embed or write.
    pub dry_run: bool,
    /// Optional source scope; only pages from this source are processed.
    pub source_id: Option<String>,
}

impl Default for EmbedStaleOpts {
    fn default() -> Self {
        EmbedStaleOpts {
            dry_run: false,
            source_id: None,
        }
    }
}

/// Outcome of an [`embed_stale`] run. Mirrors the TS `EmbedResult` shape
/// (embedded / would_embed / skipped) adapted to page-level counts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmbedStaleResult {
    /// Total stale pages found (after source filter).
    pub total: usize,
    /// Pages actually embedded (0 in dry-run).
    pub embedded: usize,
    /// Pages that would be embedded if not for dry-run (0 otherwise).
    pub would_embed: usize,
    /// Pages skipped because they had no embeddable text.
    pub skipped: usize,
}

/// Text fed to the embedding model for a page. Mirrors the production
/// embedding path: prefer the rendered `compiled_truth`, fall back to the
/// `title` when the body is empty. Returns `None` when there is nothing to
/// embed (e.g. a stub/placeholder page).
fn page_embed_text(page: &Page) -> Option<String> {
    let body = page.compiled_truth.trim();
    if !body.is_empty() {
        return Some(body.to_string());
    }
    let title = page.title.trim();
    if !title.is_empty() {
        return Some(title.to_string());
    }
    None
}

/// Enumerate stale (null-embedding) pages, embed each via `client`, and write
/// the vector back through [`BrainEngine::put_page_embedding`]. This is the
/// page-level analog of the TS `zbrain embed --stale` chunk loop.
pub async fn embed_stale(
    engine: &dyn BrainEngine,
    client: &EmbeddingClient,
    opts: &EmbedStaleOpts,
) -> Result<EmbedStaleResult> {
    let mut stale = engine.list_stale_pages().await?;
    if let Some(ref src) = opts.source_id {
        stale.retain(|p| &p.source_id == src);
    }
    let total = stale.len();

    let mut result = EmbedStaleResult {
        total,
        ..Default::default()
    };
    for page in &stale {
        let Some(text) = page_embed_text(page) else {
            result.skipped += 1;
            continue;
        };
        if opts.dry_run {
            result.would_embed += 1;
            continue;
        }
        let vec = client.embed(&text).await.map_err(|e| embed_err(&text, e))?;
        let bytes: Vec<u8> = vec.iter().flat_map(|f| f.to_le_bytes()).collect();
        engine
            .put_page_embedding(&page.slug, &page.source_id, bytes)
            .await?;
        result.embedded += 1;
    }
    Ok(result)
}

fn embed_err(text: &str, e: EmbeddingError) -> StructuredError {
    StructuredError::new(
        "EmbeddingFailed",
        "embedding_failed",
        format!(
            "failed to embed page text ({} chars): {e}",
            text.chars().count()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::{EmbeddingClient, EmbeddingConfig, EmbeddingError, EmbeddingProvider};
    use crate::engine::{InMemoryEngine, PageInput};
    use std::sync::Arc;

    /// Deterministic fake provider: every text maps to a constant vector of
    /// length `dims`. Lets us assert on embedded counts + byte lengths without
    /// any network.
    struct ConstProvider;

    #[async_trait::async_trait]
    impl EmbeddingProvider for ConstProvider {
        async fn embed(
            &self,
            texts: &[String],
            dims: usize,
        ) -> std::result::Result<Vec<Vec<f32>>, EmbeddingError> {
            Ok(texts.iter().map(|_| vec![1.5f32; dims]).collect())
        }
    }

    fn client(dims: usize) -> EmbeddingClient {
        EmbeddingClient::with_provider(
            EmbeddingConfig {
                dimensions: dims,
                ..EmbeddingConfig::default()
            },
            Arc::new(ConstProvider),
        )
    }

    async fn put_page(engine: &InMemoryEngine, slug: &str, body: &str) {
        engine
            .put_page(
                slug,
                None,
                &PageInput {
                    title: slug.to_string(),
                    compiled_truth: body.to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    fn vec_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[tokio::test]
    async fn embeds_stale_pages_and_writes_vector() {
        let engine = InMemoryEngine::new();
        put_page(&engine, "a", "body a").await; // stale (no embedding)
        put_page(&engine, "b", "body b").await;
        // Give "b" an embedding so it is NOT stale.
        engine
            .put_page_embedding("b", "default", vec_bytes(&[0.0f32; 4]))
            .await
            .unwrap();

        let res = embed_stale(&engine, &client(4), &EmbedStaleOpts::default())
            .await
            .unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.embedded, 1);
        assert_eq!(res.would_embed, 0);
        assert_eq!(res.skipped, 0);

        let got = engine
            .get_page("a", &Default::default())
            .await
            .unwrap()
            .unwrap();
        // 4 dims * 4 bytes/f32.
        assert_eq!(got.embedding.map(|b| b.len()), Some(16));
    }

    #[tokio::test]
    async fn dry_run_counts_without_writing() {
        let engine = InMemoryEngine::new();
        put_page(&engine, "a", "body a").await;
        let res = embed_stale(
            &engine,
            &client(4),
            &EmbedStaleOpts {
                dry_run: true,
                source_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.embedded, 0);
        assert_eq!(res.would_embed, 1);
        let got = engine
            .get_page("a", &Default::default())
            .await
            .unwrap()
            .unwrap();
        assert!(got.embedding.is_none(), "dry-run must not write embeddings");
    }

    #[tokio::test]
    async fn source_filter_scopes_processing() {
        let engine = InMemoryEngine::new();
        put_page(&engine, "a", "body a").await;
        engine
            .put_page(
                "s2",
                Some("other"),
                &PageInput {
                    title: "s2".into(),
                    compiled_truth: "body s2".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let res = embed_stale(
            &engine,
            &client(4),
            &EmbedStaleOpts {
                dry_run: false,
                source_id: Some("other".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.embedded, 1);
        // "a" (default source) untouched.
        let a = engine
            .get_page("a", &Default::default())
            .await
            .unwrap()
            .unwrap();
        assert!(a.embedding.is_none());
    }

    #[tokio::test]
    async fn skips_pages_with_no_text() {
        let engine = InMemoryEngine::new();
        // Page with neither body nor title -> nothing to embed.
        engine
            .put_page(
                "empty",
                None,
                &PageInput {
                    title: String::new(),
                    compiled_truth: String::new(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let res = embed_stale(&engine, &client(4), &EmbedStaleOpts::default())
            .await
            .unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.skipped, 1);
        assert_eq!(res.embedded, 0);
    }
}
