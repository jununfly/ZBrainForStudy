//! Shared helpers for the embedding-family minion handlers (`reindex`,
//! `embed`, `embed-backfill`). Kept tiny and dependency-free so the three
//! handlers can share the little bit of glue they each need without pulling
//! the embedding loop body into a trait.

use crate::Result;

/// Encode an `f32` embedding vector as a little-endian byte blob, matching the
/// `f32-LE BLOB` encoding used by `LibsqlEngine::put_page` /
/// `put_page_embedding`. Mirrors the private `encode_embedding_le` in
/// `zbrain-cli/src/lib.rs`.
#[must_use]
pub(crate) fn encode_embedding_le(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Re-embed `pages` from their `compiled_truth` text via `client` and write the
/// resulting page-level vectors back through `engine.put_page_embedding`.
///
/// Shared by `reindex` (all live pages) and `embed` / `embed-backfill`
/// (stale / missing-embedding pages). `dry_run` lists without writing. Returns
/// the scanned / embedded counts for the handler's JSON result.
#[cfg(feature = "embedding")]
pub(crate) async fn embed_pages(
    engine: &(dyn crate::engine::BrainEngine + 'static),
    client: &crate::embedding::EmbeddingClient,
    pages: Vec<crate::engine::Page>,
    dry_run: bool,
) -> Result<(usize, usize)> {
    let scanned = pages.len();
    if dry_run {
        return Ok((scanned, 0));
    }
    let texts: Vec<String> = pages.iter().map(|p| p.compiled_truth.clone()).collect();
    let vectors = client
        .embed_batch(&texts, None)
        .await
        .map_err(|e| crate::Error::new("EmbeddingError", "embed_pages", &e.to_string()))?;
    let mut embedded = 0usize;
    for (p, vec) in pages.iter().zip(vectors.into_iter()) {
        let bytes = encode_embedding_le(&vec);
        engine
            .put_page_embedding(&p.slug, &p.source_id, bytes)
            .await?;
        embedded += 1;
    }
    Ok((scanned, embedded))
}
