//! Doc↔impl edge reconciliation (G77 / 1-6-2).
//!
//! Ports TS `runReconcileLinks` from `src/commands/reconcile-links.ts`.
//! Walks every markdown page, extracts code-path references from
//! `compiled_truth` + `timeline` (`extract_code_refs`), and creates
//! bidirectional `documents` / `documented_by` edges to the matching code
//! page (slugified via `slugify_code_path`). Idempotent via
//! `add_links_batch`'s `INSERT OR IGNORE` plus the inner `pages` JOIN that
//! silently drops edges to pages that don't exist yet.
//!
//! ## Slug parity
//!
//! `slugify_code_path` is a faithful port of TS `slugifyCodePath` (sync.ts),
//! which is also the slug a code page receives at import time. Reconcile
//! edges only resolve when the produced slug matches an existing code page,
//! so this must stay byte-for-byte equivalent to the import-time scheme.
//!
//! ## Engine-agnostic reads
//!
//! Reconcile reads pages through the public `BrainEngine` API
//! (`list_all_page_refs` + `get_page`) instead of `execute_raw`. This keeps
//! the module testable on `InMemoryEngine`, where `execute_raw` is
//! unsupported. Soft-deleted pages are skipped (mirrors the default
//! `get_page` behaviour); targets must be live code pages for an edge to be
//! written.

use crate::engine::{BrainEngine, GetPageOpts};
use crate::types::{LinkBatchInput, PageKind};
use crate::Result;
use std::collections::HashSet;
use std::sync::OnceLock;

// ── code-reference extraction ──────────────────────────────────────────

/// A code-path reference found in markdown prose (e.g. `src/core/sync.ts:42`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRef {
    /// Raw matched path (e.g. `src/core/sync.ts`).
    pub path: String,
    /// Optional line number from `src/foo.ts:42`.
    pub line: Option<u32>,
    /// Index of the match in the source string.
    pub index: usize,
}

// Mirrors TS `CODE_REF_REGEX` in `src/core/link-extraction.ts`. Anchored
// against the common zbrain repo layout directories so arbitrary prose like
// "in foo/bar.js" doesn't generate false positives; the extension list is
// aligned with `detectCodeLanguage` so only paths that could have a code
// page are matched.
const CODE_REF_REGEX: &str = r"\b((?:src|lib|app|test|tests|scripts|docs|packages|internal|cmd|examples)/[\w\-./]+\.(?:ts|tsx|mts|cts|js|jsx|mjs|cjs|py|rb|go|rs|java|cs|cpp|cc|hpp|c|h|php|swift|kt|scala|lua|ex|exs|elm|ml|dart|zig|sol|sh|bash|css|html|vue|json|yaml|yml|toml))(?::(\d+))?\b";

static CODE_REF_RE: OnceLock<regex::Regex> = OnceLock::new();

fn code_ref_regex() -> &'static regex::Regex {
    CODE_REF_RE.get_or_init(|| regex::Regex::new(CODE_REF_REGEX).expect("valid code-ref regex"))
}

/// Extract code-path references (e.g. `src/core/sync.ts:42`) from markdown
/// prose. Deduped by path. Mirrors TS `extractCodeRefs`.
pub fn extract_code_refs(content: &str) -> Vec<CodeRef> {
    let re = code_ref_regex();
    let mut seen: HashSet<String> = HashSet::new();
    let mut refs = Vec::new();
    for cap in re.captures_iter(content) {
        let path = match cap.get(1) {
            Some(m) => m.as_str().to_string(),
            None => continue,
        };
        if seen.contains(&path) {
            continue;
        }
        seen.insert(path.clone());
        let line = cap.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
        let index = cap.get(0).map(|m| m.start()).unwrap_or(0);
        refs.push(CodeRef { path, line, index });
    }
    refs
}

// ── code-path slugification ────────────────────────────────────────────

/// Convert a repo-relative file path to a ZBrain code-page slug. Faithful
/// port of TS `slugifyCodePath` (sync.ts), matching how code pages are
/// slugified at import time so reconcile edges resolve to existing pages.
pub fn slugify_code_path(file_path: &str) -> String {
    let path = file_path.replace('\\', "/");
    // Strip a single leading "./" or "/" (TS: /^\.?\//).
    let path = if let Some(s) = path.strip_prefix("./") {
        s
    } else if let Some(s) = path.strip_prefix('/') {
        s
    } else {
        &path
    };
    path.split('/')
        .map(|seg| slugify_segment(&seg.replace('.', "-")))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Port of TS `slugifySegment`. Keeps ASCII alphanumerics, `.`, `_`, CJK
/// ranges, and whitespace (later folded to `-`); everything else is dropped.
/// Then spaces → hyphens, hyphen runs collapse, and leading/trailing hyphens
/// are trimmed. `to_ascii_lowercase` is a no-op for CJK, so those survive.
fn slugify_segment(segment: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for c in segment.chars() {
        let lower = c.to_ascii_lowercase();
        let keep = lower.is_ascii_alphanumeric() || matches!(lower, '.' | '_') || is_cjk(c);
        if keep {
            out.push(lower);
            prev_hyphen = false;
        } else if c.is_whitespace() || c == '-' {
            if !prev_hyphen {
                out.push('-');
            }
            prev_hyphen = true;
        }
        // otherwise: drop (matches SLUGIFY_KEEP_RE removing the char)
    }
    out.trim_matches('-').to_string()
}

/// CJK / Kana / Hangul keep-ranges, mirroring TS `CJK_SLUG_CHARS`
/// (`一-鿿぀-ゟ゠-ヿ가-힯`).
fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x4E00..=0x9FFF).contains(&u) // CJK Unified Ideographs
        || (0x3040..=0x309F).contains(&u) // Hiragana
        || (0x30A0..=0x30FF).contains(&u) // Katakana
        || (0xAC00..=0xD7AF).contains(&u) // Hangul Syllables
}

// ── reconciliation ─────────────────────────────────────────────────────

/// Outcome of a [`reconcile_links`] run. Mirrors TS `ReconcileLinksResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileLinksResult {
    /// `"ok"` or `"auto_link_disabled"`.
    pub status: String,
    pub markdown_pages_scanned: usize,
    pub code_refs_found: usize,
    pub edges_attempted: usize,
    pub edges_targets_missing: usize,
}

/// Options for [`reconcile_links`].
#[derive(Debug, Clone, Default)]
pub struct ReconcileLinksOpts {
    /// Scope reconciliation to one source (default `"default"`).
    pub source_id: Option<String>,
    /// Report counts without writing any edges.
    pub dry_run: bool,
}

/// Flush `add_links_batch` calls in chunks (mirrors `LINK_BATCH_SIZE` in
/// `auto_fix.rs`).
const LINK_BATCH_SIZE: usize = 200;

/// Scan every markdown page for code-path references and create bidirectional
/// `documents` / `documented_by` edges to the matching code page. Idempotent.
/// Respects the `auto_link` config gate (returns `auto_link_disabled` when it
/// is `"false"`). Mirrors TS `runReconcileLinks`.
pub async fn reconcile_links(
    engine: &dyn BrainEngine,
    opts: &ReconcileLinksOpts,
) -> Result<ReconcileLinksResult> {
    // 1. auto_link gate (same gate put_page uses). A user that explicitly
    //    turned off auto-link doesn't want reconcile-links writing edges back.
    let auto_link = engine.get_config("auto_link").await?;
    if auto_link.as_deref() == Some("false") {
        return Ok(ReconcileLinksResult {
            status: "auto_link_disabled".to_string(),
            ..Default::default()
        });
    }

    let eff_source = opts
        .source_id
        .clone()
        .unwrap_or_else(|| "default".to_string());

    // 2. Enumerate pages through the public engine API (engine-agnostic;
    //    works on InMemoryEngine where execute_raw is unsupported). We fetch
    //    each page's full record so we can both scan markdown prose and build
    //    the code-slug presence set in a single pass.
    let refs = engine.list_all_page_refs().await?;
    let get_opts = GetPageOpts {
        source_id: Some(eff_source.clone()),
        ..Default::default()
    };

    let mut page_refs: Vec<(String, Vec<CodeRef>)> = Vec::new();
    let mut code_slugs: HashSet<String> = HashSet::new();
    let mut code_refs_found: usize = 0;
    let mut markdown_pages_scanned: usize = 0;

    for r in &refs {
        // Only inspect pages within the scoped source.
        if r.source_id != eff_source {
            continue;
        }
        let Some(page) = engine.get_page(&r.slug, &get_opts).await? else {
            continue;
        };
        match page.page_kind {
            PageKind::Markdown => {
                let haystack = format!("{}\n{}", page.compiled_truth, page.timeline);
                let crs = extract_code_refs(&haystack);
                code_refs_found += crs.len();
                markdown_pages_scanned += 1;
                page_refs.push((page.slug, crs));
            }
            PageKind::Code => {
                code_slugs.insert(page.slug);
            }
            _ => {}
        }
    }

    if opts.dry_run {
        return Ok(ReconcileLinksResult {
            status: "ok".to_string(),
            markdown_pages_scanned,
            code_refs_found,
            edges_attempted: 0,
            edges_targets_missing: 0,
        });
    }

    // 3. Build the link batch. Forward = guide documents code; reverse =
    //    code is documented_by the guide. Both are idempotent on the
    //    `links` table (INSERT OR IGNORE + inner pages JOIN in
    //    add_links_batch). A target slug that isn't a live code page is
    //    counted as `edges_targets_missing` rather than written.
    let mut links: Vec<LinkBatchInput> = Vec::new();
    let mut edges_attempted: usize = 0;
    let mut edges_targets_missing: usize = 0;
    for (md_slug, refs) in &page_refs {
        for rf in refs {
            edges_attempted += 1;
            let code_slug = slugify_code_path(&rf.path);
            if code_slugs.contains(&code_slug) {
                let ctx = match rf.line {
                    Some(line) => format!("cited at {}:{}", rf.path, line),
                    None => rf.path.clone(),
                };
                links.push(LinkBatchInput {
                    from_slug: md_slug.clone(),
                    to_slug: code_slug.clone(),
                    link_type: Some("documents".to_string()),
                    context: Some(ctx),
                    link_source: Some("markdown".to_string()),
                    origin_slug: Some(md_slug.clone()),
                    origin_field: Some("compiled_truth".to_string()),
                    from_source_id: Some(eff_source.clone()),
                    to_source_id: Some(eff_source.clone()),
                    origin_source_id: Some(eff_source.clone()),
                });
                links.push(LinkBatchInput {
                    from_slug: code_slug.clone(),
                    to_slug: md_slug.clone(),
                    link_type: Some("documented_by".to_string()),
                    context: Some(rf.path.clone()),
                    link_source: Some("markdown".to_string()),
                    origin_slug: Some(md_slug.clone()),
                    origin_field: Some("compiled_truth".to_string()),
                    from_source_id: Some(eff_source.clone()),
                    to_source_id: Some(eff_source.clone()),
                    origin_source_id: Some(eff_source.clone()),
                });
            } else {
                edges_targets_missing += 1;
            }
        }
    }

    // 4. Flush in chunks.
    for chunk in links.chunks(LINK_BATCH_SIZE) {
        engine.add_links_batch(chunk).await?;
    }

    Ok(ReconcileLinksResult {
        status: "ok".to_string(),
        markdown_pages_scanned,
        code_refs_found,
        edges_attempted,
        edges_targets_missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BrainEngine, InMemoryEngine, PageInput};
    use crate::types::PageKind;

    // ── pure function tests ───────────────────────────────────────────

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify_code_path("src/core/sync.ts"), "src-core-sync-ts");
    }

    #[test]
    fn slugify_strips_leading_dot_slash_and_handles_dot_in_segment() {
        assert_eq!(
            slugify_code_path("./internal/x_y.z.rs"),
            "internal-x_y-z-rs"
        );
    }

    #[test]
    fn slugify_handles_tsx_extension() {
        assert_eq!(slugify_code_path("src/foo/Bar.tsx"), "src-foo-bar-tsx");
    }

    #[test]
    fn slugify_handles_backslash_and_leading_slash() {
        assert_eq!(slugify_code_path("/lib\\foo.py"), "lib-foo-py");
    }

    #[test]
    fn extract_finds_refs_and_line() {
        let refs = extract_code_refs("see src/core/sync.ts:42 and lib/foo.py");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].path, "src/core/sync.ts");
        assert_eq!(refs[0].line, Some(42));
        assert_eq!(refs[1].path, "lib/foo.py");
        assert_eq!(refs[1].line, None);
    }

    #[test]
    fn extract_requires_known_directory_prefix() {
        // "in foo/bar.js" must NOT match — only the anchored dirs do.
        assert!(extract_code_refs("in foo/bar.js").is_empty());
        // but "src/foo/bar.js" does.
        assert_eq!(extract_code_refs("src/foo/bar.js").len(), 1);
    }

    #[test]
    fn extract_dedups_by_path() {
        let refs = extract_code_refs("src/a.ts and src/a.ts again");
        assert_eq!(refs.len(), 1);
    }

    // ── integration tests (InMemoryEngine) ────────────────────────────

    async fn put_md(engine: &InMemoryEngine, slug: &str, body: &str) {
        engine
            .put_page(
                slug,
                None,
                &PageInput {
                    title: slug.to_string(),
                    page_kind: Some(PageKind::Markdown),
                    compiled_truth: body.to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    async fn put_code(engine: &InMemoryEngine, slug: &str) {
        engine
            .put_page(
                slug,
                None,
                &PageInput {
                    title: slug.to_string(),
                    page_kind: Some(PageKind::Code),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reconcile_creates_doc_impl_edges() {
        let engine = InMemoryEngine::new();
        put_md(&engine, "guide", "the sync lives in src/core/sync.ts:42").await;
        put_code(&engine, "src-core-sync-ts").await;

        let res = reconcile_links(&engine, &ReconcileLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(res.status, "ok");
        assert_eq!(res.markdown_pages_scanned, 1);
        assert_eq!(res.code_refs_found, 1);
        assert_eq!(res.edges_attempted, 1);
        assert_eq!(res.edges_targets_missing, 0);

        // Forward edge: guide documents the code page.
        let fwd = engine.get_links("guide", Some("default")).await.unwrap();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0].link_type, "documents");
        assert_eq!(fwd[0].to_slug, "src-core-sync-ts");

        // Reverse edge: code page is documented_by the guide. From the code
        // page's perspective this is an *outbound* edge, so it surfaces via
        // get_links (not get_backlinks, which returns inbound edges).
        let back = engine
            .get_links("src-core-sync-ts", Some("default"))
            .await
            .unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].link_type, "documented_by");
        assert_eq!(back[0].from_slug, "src-core-sync-ts");
        assert_eq!(back[0].to_slug, "guide");
    }

    #[tokio::test]
    async fn reconcile_counts_missing_target() {
        let engine = InMemoryEngine::new();
        put_md(&engine, "guide", "see src/core/sync.ts").await;
        // no matching code page

        let res = reconcile_links(&engine, &ReconcileLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(res.edges_attempted, 1);
        assert_eq!(res.edges_targets_missing, 1);
    }

    #[tokio::test]
    async fn reconcile_is_idempotent() {
        let engine = InMemoryEngine::new();
        put_md(&engine, "guide", "see src/core/sync.ts").await;
        put_code(&engine, "src-core-sync-ts").await;

        // First pass writes the edges.
        let first = reconcile_links(&engine, &ReconcileLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(first.edges_attempted, 1);
        // Second pass must not duplicate (INSERT OR IGNORE).
        let second = reconcile_links(&engine, &ReconcileLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(second.edges_attempted, 1);

        // Exactly one forward + one reverse link exist (two distinct edges).
        // The "documented_by" edge is the code page's outbound edge, so read
        // it via get_links on the code slug (get_backlinks would return the
        // inbound "documents" edge — the same Link A as get_links("guide")).
        let fwd = engine.get_links("guide", Some("default")).await.unwrap();
        let back = engine
            .get_links("src-core-sync-ts", Some("default"))
            .await
            .unwrap();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0].link_type, "documents");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].link_type, "documented_by");
    }

    #[tokio::test]
    async fn reconcile_respects_auto_link_disabled() {
        let engine = InMemoryEngine::new();
        engine.set_config("auto_link", "false").await.unwrap();
        put_md(&engine, "guide", "see src/core/sync.ts").await;
        put_code(&engine, "src-core-sync-ts").await;

        let res = reconcile_links(&engine, &ReconcileLinksOpts::default())
            .await
            .unwrap();
        assert_eq!(res.status, "auto_link_disabled");
        assert_eq!(res.markdown_pages_scanned, 0);
    }

    #[tokio::test]
    async fn reconcile_dry_run_writes_nothing() {
        let engine = InMemoryEngine::new();
        put_md(&engine, "guide", "see src/core/sync.ts").await;
        put_code(&engine, "src-core-sync-ts").await;

        let res = reconcile_links(
            &engine,
            &ReconcileLinksOpts {
                dry_run: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(res.edges_attempted, 0);

        // No links of either direction were written.
        let fwd = engine.get_links("guide", Some("default")).await.unwrap();
        let back = engine
            .get_links("src-core-sync-ts", Some("default"))
            .await
            .unwrap();
        assert!(fwd.is_empty());
        assert!(back.is_empty());
    }
}
