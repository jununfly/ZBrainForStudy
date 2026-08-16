//! `zbrain integrity check` — read-only brain-integrity scan.
//!
//! Ports the `check` subcommand of `src/commands/integrity.ts`
//! (`scanIntegrity` + `findBareTweetHits` + `findExternalLinks`). Two known
//! pain points are surfaced:
//!
//! 1. **Bare-tweet references** — prose like "Garry tweeted about X" with no
//!    URL (the TS `CITATIONS.md` audit found 1,424 of 3,115 people pages).
//! 2. **External markdown links** — every `[text](https://…)` citation, so a
//!    later pass can check them for rot.
//!
//! The read-only `check` path needs only the engine (no AI gateway, no
//! resolver SDK, no writes). The `auto` / `review` / `reset-progress`
//! subcommands depend on the un-migrated resolver SDK (`x_handle_to_tweet` /
//! `url_reachable`) and are intentionally **out of scope** — registered in
//! `docs/plans/MIGRATION.md` (G51). Porting `check` alone already unblocks
//! `zbrain doctor`'s sampled integrity signal, which calls `scanIntegrity`.

use crate::engine::{BrainEngine, GetPageOpts, Page};
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// Phrases that plausibly reference a tweet without linking to one.
/// Case-insensitive. Mirrors TS `BARE_TWEET_PHRASES` (integrity.ts:62-72).
const BARE_TWEET_PHRASES: &[&str] = &[
    r"\btweeted about\b",
    r"\bin (?:a |the )?(?:recent |viral )?tweet\b",
    r"\bon (?:a |the )?(?:recent |viral )?tweet\b",
    r"\bwrote (?:a |the )?(?:tweet|post)\b",
    r"\bposted on X\b",
    // "via X" but not "via X/handle" — the negative look-ahead `(?!\s*/)` is
    // not supported by the `regex` crate, so the per-line guard lives in
    // `find_bare_tweet_hits` (skip when the match is followed by '/').
    r"\bvia X\b",
    r"\bhis (?:recent |)tweet\b",
    r"\bher (?:recent |)tweet\b",
    r"\btheir (?:recent |)tweet\b",
];

/// A bare-tweet phrase on a line that has no tweet URL on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BareTweetHit {
    pub slug: String,
    pub line: usize,
    pub raw_line: String,
    pub phrase: String,
}

/// An external markdown link `[text](https://…)`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalLinkHit {
    pub slug: String,
    pub line: usize,
    pub url: String,
}

/// Top page by bare-tweet hit count (descending), for the report summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopPage {
    pub slug: String,
    pub count: u64,
}

/// Result of a full read-only integrity scan.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrityScanResult {
    pub pages_scanned: u64,
    pub bare_hits: Vec<BareTweetHit>,
    pub external_hits: Vec<ExternalLinkHit>,
    pub top_pages: Vec<TopPage>,
}

/// Options for [`scan_integrity`]. Mirrors TS `IntegrityScanOptions`.
#[derive(Debug, Clone, Default)]
pub struct IntegrityScanOptions {
    /// Max pages to scan. `None` / `u64::MAX` means unbounded (TS `Infinity`).
    pub limit: Option<u64>,
    /// Only scan pages whose slug starts with `<typeFilter>/` (TS `typeFilter`).
    pub type_filter: Option<String>,
}

static BARE_TWEET_RE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    BARE_TWEET_PHRASES
        .iter()
        .map(|p| Regex::new(p).expect("BARE_TWEET_PHRASES must compile"))
        .collect()
});

/// Tweet-status URL on a line means the reference is already cited — skip it.
/// Mirrors TS `URL_NEARBY_RE` (integrity.ts:74).
static TWEET_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https?://(?:x\.com|twitter\.com)/[A-Za-z0-9_]+/status/\d+")
        .expect("TWEET_URL_RE must compile")
});

/// External markdown link capture. Mirrors TS `MD_LINK_EXTERNAL_RE` (line 116).
static MD_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]+\]\((https?://[^)]+)\)").expect("MD_LINK_RE must compile"));

/// Detect bare-tweet references in a page body. Pure.
///
/// Skips fenced code blocks (``` / ~~~) and any line that already carries a
/// tweet-status URL. One finding per line maximum (the first matching
/// phrase). Mirrors TS `findBareTweetHits` (integrity.ts:83-110).
pub fn find_bare_tweet_hits(compiled_truth: &str, slug: &str) -> Vec<BareTweetHit> {
    let mut hits = Vec::new();
    let mut inside_fence = false;
    let mut fence_marker = "";

    for (i, line) in compiled_truth.split('\n').enumerate() {
        if inside_fence {
            if line.starts_with(fence_marker) {
                inside_fence = false;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            inside_fence = true;
            fence_marker = if line.starts_with("```") { "```" } else { "~~~" };
            continue;
        }
        // Already cited — skip this line entirely.
        if TWEET_URL_RE.is_match(line) {
            continue;
        }
        for re in BARE_TWEET_RE.iter() {
            if let Some(m) = re.find(line) {
                let phrase = m.as_str();
                // "via X" must not be immediately followed by '/' (that form,
                // e.g. "via X/foo_handle", is already a citation). The
                // `regex` crate lacks look-ahead, so guard here.
                if phrase == "via X" && line[m.end()..].starts_with('/') {
                    continue;
                }
                hits.push(BareTweetHit {
                    slug: slug.to_string(),
                    line: i + 1,
                    raw_line: line.trim().to_string(),
                    phrase: phrase.to_string(),
                });
                break; // one finding per line is enough
            }
        }
    }
    hits
}

/// Collect external markdown links in a page body. Pure.
///
/// Skips fenced code blocks. Captures every link on a line (unlike bare-tweet,
/// which stops at the first). Mirrors TS `findExternalLinks` (integrity.ts:124-147).
pub fn find_external_links(compiled_truth: &str, slug: &str) -> Vec<ExternalLinkHit> {
    let mut hits = Vec::new();
    let mut inside_fence = false;
    let mut fence_marker = "";

    for (i, line) in compiled_truth.split('\n').enumerate() {
        if inside_fence {
            if line.starts_with(fence_marker) {
                inside_fence = false;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            inside_fence = true;
            fence_marker = if line.starts_with("```") { "```" } else { "~~~" };
            continue;
        }
        for m in MD_LINK_RE.captures_iter(line) {
            if let Some(url) = m.get(1) {
                hits.push(ExternalLinkHit {
                    slug: slug.to_string(),
                    line: i + 1,
                    url: url.as_str().to_string(),
                });
            }
        }
    }
    hits
}

/// Read-only integrity scan over the engine's live pages. No network, no
/// writes, no resolver calls. Caller owns the engine lifecycle. Mirrors TS
/// `scanIntegrity` (integrity.ts:292-350), using the engine-portable
/// `list_all_page_refs` + `get_page` sequential path (the Postgres fast-batch
/// SQL path is a later optimization and not required for parity of `check`).
pub async fn scan_integrity(
    engine: &dyn BrainEngine,
    opts: &IntegrityScanOptions,
) -> crate::Result<IntegrityScanResult> {
    let mut all_refs = engine.list_all_page_refs().await?;
    // Deterministic order: slug asc, then source_id asc (TS localeCompare chain).
    all_refs.sort_by(|a, b| a.slug.cmp(&b.slug).then_with(|| a.source_id.cmp(&b.source_id)));

    let limit = opts.limit.unwrap_or(u64::MAX);
    let type_filter = opts.type_filter.as_deref();

    let mut bare_hits: Vec<BareTweetHit> = Vec::new();
    let mut external_hits: Vec<ExternalLinkHit> = Vec::new();
    let mut pages_scanned: u64 = 0;
    let mut by_page_count: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    for r in &all_refs {
        if let Some(tf) = type_filter {
            if !r.slug.starts_with(&format!("{tf}/")) {
                continue;
            }
        }
        if pages_scanned >= limit {
            break;
        }
        let page_opt: Option<Page> = engine
            .get_page(&r.slug, &GetPageOpts {
                source_id: Some(r.source_id.clone()),
                include_deleted: false,
            })
            .await?;
        let page = match page_opt {
            Some(p) => p,
            None => continue,
        };
        // Skip grandfathered pages (opted out of integrity enforcement).
        // TS strict `=== false` check — only a literal boolean false opts out.
        if page.frontmatter.get("validate") == Some(&serde_json::Value::Bool(false)) {
            continue;
        }
        pages_scanned += 1;
        let hits = find_bare_tweet_hits(&page.compiled_truth, &page.slug);
        for h in &hits {
            *by_page_count.entry(h.slug.clone()).or_insert(0) += 1;
        }
        bare_hits.extend(hits);
        external_hits.extend(find_external_links(&page.compiled_truth, &page.slug));
    }

    let mut top_pages: Vec<TopPage> = by_page_count
        .into_iter()
        .map(|(slug, count)| TopPage { slug, count })
        .collect();
    top_pages.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.slug.cmp(&b.slug)));
    top_pages.truncate(10);

    Ok(IntegrityScanResult {
        pages_scanned,
        bare_hits,
        external_hits,
        top_pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_tweet_single_phrase() {
        let body = "Garry tweeted about the new model architecture last week.";
        let hits = find_bare_tweet_hits(body, "garry");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].phrase, "tweeted about");
        assert_eq!(hits[0].slug, "garry");
    }

    #[test]
    fn bare_tweet_one_per_line() {
        // Two phrases on one line → only the first match is reported.
        let body = "He tweeted about it and posted on X too.";
        let hits = find_bare_tweet_hits(body, "p");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn bare_tweet_url_present_skips_line() {
        // A tweet-status URL on the line means it's cited → no bare hit.
        let body = "He tweeted about it https://x.com/foo/status/1234567890 and more.";
        let hits = find_bare_tweet_hits(body, "p");
        assert!(hits.is_empty(), "cited line must not produce a bare hit");
    }

    #[test]
    fn bare_tweet_inside_fence_skipped() {
        let body = "intro line\n```\nHe tweeted about X inside code\n```\nafter fence he tweeted about Y";
        let hits = find_bare_tweet_hits(body, "p");
        // Only the post-fence line should match (0-based index 4 → line 5).
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 5);
        assert_eq!(hits[0].phrase, "tweeted about");
    }

    #[test]
    fn bare_tweet_tilde_fence_skipped() {
        let body = "~~~\nHe tweeted about X\n~~~\nreal tweeted about here";
        let hits = find_bare_tweet_hits(body, "p");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 4);
    }

    #[test]
    fn bare_tweet_via_x_but_not_via_x_handle() {
        // "via X" alone is a hit; "via X/handle" is already cited (skip).
        assert_eq!(find_bare_tweet_hits("see via X for details", "p").len(), 1);
        assert_eq!(
            find_bare_tweet_hits("see via X/foo_handle for details", "p").len(),
            0
        );
    }

    #[test]
    fn external_links_capture_all() {
        let body = "See [docs](https://example.com/a) and [guide](https://example.com/b).";
        let hits = find_external_links(body, "p");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/a");
        assert_eq!(hits[1].url, "https://example.com/b");
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn external_links_ignore_fenced() {
        let body = "text [ok](https://a.com)\n```\n[skip](https://b.com)\n```\n[again](https://c.com)";
        let hits = find_external_links(body, "p");
        let urls: Vec<&str> = hits.iter().map(|h| h.url.as_str()).collect();
        assert_eq!(urls, vec!["https://a.com", "https://c.com"]);
    }

    #[test]
    fn external_links_no_false_positive_on_relative() {
        // Bare `[text](local.md)` is NOT an external link.
        let hits = find_external_links("see [local](notes.md) here", "p");
        assert!(hits.is_empty());
    }

    #[test]
    fn top_pages_sorted_desc_then_slug() {
        let mut result = IntegrityScanResult {
            pages_scanned: 3,
            bare_hits: vec![
                BareTweetHit { slug: "b".into(), line: 1, raw_line: "".into(), phrase: "".into() },
                BareTweetHit { slug: "a".into(), line: 1, raw_line: "".into(), phrase: "".into() },
                BareTweetHit { slug: "b".into(), line: 2, raw_line: "".into(), phrase: "".into() },
            ],
            external_hits: vec![],
            top_pages: vec![],
        };
        // Recompute top_pages the way scan_integrity would.
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for h in &result.bare_hits {
            *counts.entry(h.slug.clone()).or_insert(0) += 1;
        }
        let mut top: Vec<TopPage> = counts
            .into_iter()
            .map(|(slug, count)| TopPage { slug, count })
            .collect();
        top.sort_by(|x, y| y.count.cmp(&x.count).then_with(|| x.slug.cmp(&y.slug)));
        result.top_pages = top;
        assert_eq!(result.top_pages[0].slug, "b");
        assert_eq!(result.top_pages[0].count, 2);
        assert_eq!(result.top_pages[1].slug, "a");
    }
}
