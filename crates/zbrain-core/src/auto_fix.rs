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
use crate::markdown_links::extract_markdown_links;
use crate::types::LinkBatchInput;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

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

// ── extract_links ────────────────────────────────────────────────────────
//
// Page-level analog of `zbrain extract links`. The TS path resolves each
// markdown/wikilink target against the set of existing slugs (via
// `resolveSlug`, which joins the link's relative path against the page's
// directory). In the page-level Rust model there are no files/directories,
// so the candidate slug is simply the link target with its `.md` suffix and
// optional `source:` qualifier stripped — equivalent to TS `resolveSlug`
// with an empty `fileDir`. Dangling links (no matching slug) are skipped,
// and self-links are never written.

/// Flush `add_links_batch` calls in chunks to avoid one giant insert (mirrors
/// the TS `BATCH_SIZE` batching in `extract.ts`).
const LINK_BATCH_SIZE: usize = 200;

/// Options for [`extract_links`].
pub struct ExtractLinksOpts {
    /// Process a single page (by slug) instead of every page in the brain.
    pub slug: Option<String>,
}

impl Default for ExtractLinksOpts {
    fn default() -> Self {
        ExtractLinksOpts { slug: None }
    }
}

/// Outcome of an [`extract_links`] run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtractLinksResult {
    /// Pages whose body was scanned.
    pub pages_processed: usize,
    /// Links written (count returned by `add_links_batch`).
    pub links_created: usize,
    /// Links whose target slug does not exist in the brain (skipped).
    pub dangling: usize,
}

/// Resolve a markdown/wikilink target to a slug in the brain, page-level
/// style: strip an optional `source:` qualifier and the `.md` suffix, then
/// check exact membership in `all_slugs`. Returns `None` for dangling links.
fn resolve_link_slug(rel_target: &str, all_slugs: &HashSet<&str>) -> Option<String> {
    // Optional `source:slug` qualifier (e.g. cross-source wikilinks). The
    // qualifier token has no `/` or `.`; anything else is a literal slug.
    let without_source = match rel_target.split_once(':') {
        Some((prefix, rest)) if !prefix.contains('/') && !prefix.contains('.') => rest,
        _ => rel_target,
    };
    let no_ext = without_source
        .strip_suffix(".md")
        .unwrap_or(without_source);
    if all_slugs.contains(no_ext) {
        Some(no_ext.to_string())
    } else {
        None
    }
}

/// Scan page bodies for markdown/wikilinks, resolve each target against the
/// set of existing page slugs, and write the resulting outgoing links via
/// `add_links_batch`. Page-level analog of the TS `extract links` db-source
/// path.
pub async fn extract_links(
    engine: &dyn BrainEngine,
    opts: &ExtractLinksOpts,
) -> Result<ExtractLinksResult> {
    // 1. Snapshot all slugs (+ their source) for target resolution.
    let refs = engine.list_all_page_refs().await?;
    let slug_source: HashMap<&str, &str> = refs
        .iter()
        .map(|r| (r.slug.as_str(), r.source_id.as_str()))
        .collect();
    let all_slugs: HashSet<&str> = slug_source.keys().copied().collect();

    // 2. Pages to scan.
    let targets: Vec<String> = match &opts.slug {
        Some(s) => vec![s.clone()],
        None => refs.iter().map(|r| r.slug.clone()).collect(),
    };

    let mut result = ExtractLinksResult::default();
    let mut batch: Vec<LinkBatchInput> = Vec::new();

    for slug in &targets {
        let Some(page) = engine.get_page(slug, &Default::default()).await? else {
            continue;
        };
        result.pages_processed += 1;
        let from_source = page.source_id.clone();

        for (name, rel_target) in extract_markdown_links(&page.compiled_truth) {
            match resolve_link_slug(&rel_target, &all_slugs) {
                None => {
                    result.dangling += 1;
                }
                Some(target) if target == slug.as_str() => {
                    // Self-link — never written.
                }
                Some(target) => {
                    let to_source = slug_source
                        .get(target.as_str())
                        .copied()
                        .map(str::to_string);
                    batch.push(LinkBatchInput {
                        from_slug: slug.clone(),
                        to_slug: target,
                        link_type: None,
                        context: Some(format!("markdown link: [{name}]")),
                        link_source: Some("markdown".to_string()),
                        origin_slug: None,
                        origin_field: None,
                        from_source_id: Some(from_source.clone()),
                        to_source_id: to_source,
                        origin_source_id: None,
                    });
                    if batch.len() >= LINK_BATCH_SIZE {
                        result.links_created += engine.add_links_batch(&batch).await?;
                        batch.clear();
                    }
                }
            }
        }
    }

    if !batch.is_empty() {
        result.links_created += engine.add_links_batch(&batch).await?;
    }
    Ok(result)
}

// ── extract_timeline ──────────────────────────────────────────────────────
//
// Page-level analog of `zbrain extract timeline`. The TS `extractTimelineFrom
// Content` parses two markdown shapes (a bullet form and a `###` header form)
// into `{date, summary}` entries. We format each as a single `"{date}
// {summary}"` line and append it to the page's `pages.timeline` TEXT column
// via `add_timeline_entry`. Re-runs are de-duplicated against the lines
// already present so reconciliation stays idempotent.

/// A single parsed timeline entry (date + human summary).
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub date: String,
    pub summary: String,
}

/// Bullet form: `- **YYYY-MM-DD** | Source — Summary`.
static BULLET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^-\s+\*\*(\d{4}-\d{2}-\d{2})\*\*\s*\|\s*(.+?)\s*[—–-]\s*(.+)$").unwrap()
});

/// Header form: `### YYYY-MM-DD — Title`.
static HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^###\s+(\d{4}-\d{2}-\d{2})\s*[—–-]\s*(.+)$").unwrap()
});

/// Parse timeline entries from page markdown. Ported from TS
/// `extractTimelineFromContent`: bullet entries first, then header entries
/// (matching TS push order), dropping the `Source`/`detail` fields that don't
/// fit the page-level single-line `pages.timeline` TEXT column.
pub fn extract_timeline_entries(content: &str) -> Vec<TimelineEntry> {
    let mut out = Vec::new();
    for c in BULLET_RE.captures_iter(content) {
        out.push(TimelineEntry {
            date: c[1].to_string(),
            summary: c[3].trim().to_string(),
        });
    }
    for c in HEADER_RE.captures_iter(content) {
        out.push(TimelineEntry {
            date: c[1].to_string(),
            summary: c[2].trim().to_string(),
        });
    }
    out
}

/// Options for [`extract_timeline`].
pub struct ExtractTimelineOpts {
    /// Process a single page (by slug) instead of every page in the brain.
    pub slug: Option<String>,
}

impl Default for ExtractTimelineOpts {
    fn default() -> Self {
        ExtractTimelineOpts { slug: None }
    }
}

/// Outcome of an [`extract_timeline`] run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtractTimelineResult {
    /// Pages whose body was scanned.
    pub pages_processed: usize,
    /// Timeline lines appended (de-duplicated).
    pub entries_added: usize,
}

/// Scan page bodies for dated timeline entries, format each as a
/// `"{date} {summary}"` line, and append it to the page's `pages.timeline`
/// column (skipping lines already present). Page-level analog of the TS
/// `extract timeline` db-source path.
pub async fn extract_timeline(
    engine: &dyn BrainEngine,
    opts: &ExtractTimelineOpts,
) -> Result<ExtractTimelineResult> {
    let refs = engine.list_all_page_refs().await?;
    let targets: Vec<(String, String)> = match &opts.slug {
        Some(s) => refs
            .into_iter()
            .filter(|r| &r.slug == s)
            .map(|r| (r.slug, r.source_id))
            .collect(),
        None => refs.into_iter().map(|r| (r.slug, r.source_id)).collect(),
    };

    let mut result = ExtractTimelineResult::default();
    for (slug, source_id) in targets {
        let Some(page) = engine.get_page(&slug, &Default::default()).await? else {
            continue;
        };
        result.pages_processed += 1;
        let existing: HashSet<String> = page
            .timeline
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        for entry in extract_timeline_entries(&page.compiled_truth) {
            let line = format!("{} {}", entry.date, entry.summary);
            if existing.contains(&line) {
                continue;
            }
            engine
                .add_timeline_entry(&slug, &source_id, &line)
                .await?;
            result.entries_added += 1;
        }
    }
    Ok(result)
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

    // ── extract_links ─────────────────────────────────────────────────────

    async fn put_with_body(engine: &InMemoryEngine, slug: &str, body: &str) {
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

    #[tokio::test]
    async fn extract_links_resolves_existing_slug() {
        let engine = InMemoryEngine::new();
        put_with_body(&engine, "alice", "see [[bob]] for details").await;
        put_with_body(&engine, "bob", "i am bob").await;

        let res = extract_links(&engine, &ExtractLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(res.pages_processed, 2);
        assert_eq!(res.links_created, 1);
        assert_eq!(res.dangling, 0);
    }

    #[tokio::test]
    async fn extract_links_skips_dangling_target() {
        let engine = InMemoryEngine::new();
        put_with_body(&engine, "alice", "see [[ghost]]").await;

        let res = extract_links(&engine, &ExtractLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(res.links_created, 0);
        assert_eq!(res.dangling, 1);
    }

    #[tokio::test]
    async fn extract_links_skips_self_link() {
        let engine = InMemoryEngine::new();
        put_with_body(&engine, "alice", "about [[alice]]").await;

        let res = extract_links(&engine, &ExtractLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(res.links_created, 0);
        assert_eq!(res.dangling, 0);
    }

    #[tokio::test]
    async fn extract_links_respects_single_slug_scope() {
        let engine = InMemoryEngine::new();
        put_with_body(&engine, "alice", "see [[bob]]").await;
        put_with_body(&engine, "bob", "back to [[alice]]").await;

        let res = extract_links(
            &engine,
            &ExtractLinksOpts {
                slug: Some("alice".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(res.pages_processed, 1);
        assert_eq!(res.links_created, 1);
    }

    #[tokio::test]
    async fn extract_links_handles_markdown_syntax() {
        let engine = InMemoryEngine::new();
        put_with_body(&engine, "alice", "see [Bob](bob.md)").await;
        put_with_body(&engine, "bob", "i am bob").await;

        let res = extract_links(&engine, &ExtractLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(res.links_created, 1);
        assert_eq!(res.dangling, 0);
    }

    // ── extract_timeline ──────────────────────────────────────────────────

    #[tokio::test]
    async fn extract_timeline_finds_bullet_entries() {
        let engine = InMemoryEngine::new();
        put_with_body(
            &engine,
            "p",
            "- **2024-01-01** | Source — First event\n- **2024-06-15** | Other — Second event",
        )
        .await;

        let res = extract_timeline(&engine, &ExtractTimelineOpts::default())
            .await
            .unwrap();
        assert_eq!(res.pages_processed, 1);
        assert_eq!(res.entries_added, 2);

        let timeline = engine
            .get_page("p", &Default::default())
            .await
            .unwrap()
            .unwrap()
            .timeline;
        assert!(timeline.contains("2024-01-01 First event"));
        assert!(timeline.contains("2024-06-15 Second event"));
    }

    #[tokio::test]
    async fn extract_timeline_finds_header_entries() {
        let engine = InMemoryEngine::new();
        put_with_body(
            &engine,
            "p",
            "### 2024-03-15 — Launched the project\n\nbody text\n\n### 2024-09-02 — Shipped v1",
        )
        .await;

        let res = extract_timeline(&engine, &ExtractTimelineOpts::default())
            .await
            .unwrap();
        assert_eq!(res.entries_added, 2);

        let timeline = engine
            .get_page("p", &Default::default())
            .await
            .unwrap()
            .unwrap()
            .timeline;
        assert!(timeline.contains("2024-03-15 Launched the project"));
        assert!(timeline.contains("2024-09-02 Shipped v1"));
    }

    #[tokio::test]
    async fn extract_timeline_is_idempotent_on_rerun() {
        let engine = InMemoryEngine::new();
        put_with_body(&engine, "p", "- **2024-01-01** | Source — First event").await;

        let first = extract_timeline(&engine, &ExtractTimelineOpts::default())
            .await
            .unwrap();
        assert_eq!(first.entries_added, 1);
        let second = extract_timeline(&engine, &ExtractTimelineOpts::default())
            .await
            .unwrap();
        assert_eq!(second.entries_added, 0, "re-run must not duplicate");
    }

    #[tokio::test]
    async fn extract_timeline_single_slug_scope() {
        let engine = InMemoryEngine::new();
        put_with_body(&engine, "a", "- **2024-01-01** | Source — Event A").await;
        put_with_body(&engine, "b", "- **2024-02-02** | Source — Event B").await;

        let res = extract_timeline(
            &engine,
            &ExtractTimelineOpts {
                slug: Some("a".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(res.pages_processed, 1);
        assert_eq!(res.entries_added, 1);
    }
}
