//! Pure markdown link extraction — ported from TS `extractMarkdownLinks`
//! (`src/commands/extract.ts`).
//!
//! Produces `(name, rel_target)` pairs from two syntaxes:
//!   1. Standard markdown:  `[text](relative/path.md)`
//!   2. Wikilinks:          `[[relative/path]]` or `[[relative/path|Display]]`
//!
//! External URLs (containing `://`) are skipped. Wikilinks get a `.md` suffix
//! appended if missing, section anchors (`#heading`) stripped, and the `|…`
//! display portion used as the link `name` when present.
//!
//! This is the foundation primitive for `zbrain extract links`; the
//! sub-node (`1-6-4-4-2`) will resolve these relative targets to slugs and
//! feed `add_links_batch`.

use regex::Regex;
use std::sync::LazyLock;

static MD_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+\.md)\)").unwrap());

static WIKI_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^|\]]+?)(?:\|[^\]]*?)?\]\]").unwrap());

/// Extract `(name, rel_target)` markdown/wikilink pairs from `content`.
/// Order follows source order (markdown links first, then wikilinks).
pub fn extract_markdown_links(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();

    for cap in MD_PATTERN.captures_iter(content) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let target = cap.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
        if target.contains("://") {
            continue;
        }
        out.push((name, target));
    }

    for cap in WIKI_PATTERN.captures_iter(content) {
        let full = cap.get(0).map(|m| m.as_str()).unwrap_or("");
        let raw_path = cap
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if raw_path.contains("://") {
            continue;
        }
        let hash_idx = raw_path.find('#');
        let page_path = match hash_idx {
            Some(i) => &raw_path[..i],
            None => &raw_path,
        };
        if page_path.is_empty() {
            continue;
        }
        let rel_target = if page_path.ends_with(".md") {
            page_path.to_string()
        } else {
            format!("{page_path}.md")
        };
        // Display name: text after `|`, if present, else the raw path.
        let display = match full.find('|') {
            Some(pipe) => {
                let after = &full[pipe + 1..];
                // strip the trailing "]]"
                let trimmed = after.strip_suffix("]]").unwrap_or(after);
                trimmed.trim().to_string()
            }
            None => raw_path.clone(),
        };
        out.push((display, rel_target));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_standard_markdown_link() {
        let links = extract_markdown_links("See [notes](other/notes.md) here.");
        assert_eq!(links, vec![("notes".to_string(), "other/notes.md".to_string())]);
    }

    #[test]
    fn skips_external_urls_in_markdown() {
        let links = extract_markdown_links("Go [home](https://example.com) externally.");
        assert!(links.is_empty(), "external URLs must be skipped");
    }

    #[test]
    fn skips_non_md_markdown_links() {
        let links = extract_markdown_links("Image [pic](img.png) is not a page.");
        assert!(links.is_empty(), "only .md targets qualify");
    }

    #[test]
    fn extracts_wikilink_bare() {
        let links = extract_markdown_links("See [[projects/alpha]] for details.");
        assert_eq!(
            links,
            vec![("projects/alpha".to_string(), "projects/alpha.md".to_string())]
        );
    }

    #[test]
    fn extracts_wikilink_with_display() {
        let links = extract_markdown_links("Ref [[projects/beta|The Beta Project]] now.");
        assert_eq!(
            links,
            vec![(
                "The Beta Project".to_string(),
                "projects/beta.md".to_string()
            )]
        );
    }

    #[test]
    fn strips_wikilink_section_anchor() {
        // Mirrors TS: the `#section` anchor is stripped from `rel_target`
        // but RETAINED in the display name (faithful port, not a fix).
        let links = extract_markdown_links("Link [[page#section]] anchors.");
        assert_eq!(
            links,
            vec![("page#section".to_string(), "page.md".to_string())]
        );
    }

    #[test]
    fn skips_external_wikilink() {
        let links = extract_markdown_links("Remote [[https://x.com/y]] skipped.");
        assert!(links.is_empty(), "external wikilinks skipped");
    }

    #[test]
    fn mixed_content_ordered_by_source() {
        // TS runs the markdown pattern over all content first, then wikilinks
        // (md-first ordering), so `c` (a .md link) precedes `Second` (a
        // wikilink) despite source order being the reverse.
        let content = "A [first](a.md) and [[b|Second]] then [c](d/c.md) and [[e]].";
        let links = extract_markdown_links(content);
        assert_eq!(
            links,
            vec![
                ("first".to_string(), "a.md".to_string()),
                ("c".to_string(), "d/c.md".to_string()),
                ("Second".to_string(), "b.md".to_string()),
                ("e".to_string(), "e.md".to_string()),
            ]
        );
    }

    #[test]
    fn empty_content_yields_nothing() {
        assert!(extract_markdown_links("").is_empty());
    }
}
