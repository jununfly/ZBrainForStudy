//! Auto-link entity *mentions* to known entity pages (G76 / 1-3).
//!
//! Ports TS `by-mention.ts` (`src/core/by-mention.ts`): a gazetteer-based
//! mention linker. Two exported building blocks plus an orchestration pass:
//!
//! * [`build_gazetteer`] — query every entity-typed page in the brain and
//!   produce a token-Map lookup (first token → entries). Pure, no IO beyond
//!   the single engine read.
//! * [`find_mentioned_entities`] — pure, IO-free maximal-munch scanner over
//!   body text. Applies the self-link guard, cross-source guard, and the
//!   per-page first-mention-only cap (one link per `(from_slug, target)`).
//! * [`run_by_mention`] — the orchestration: build a *global* gazetteer
//!   (all sources), then walk the scoped source's markdown pages, scan
//!   `compiled_truth` + `timeline`, and write bidirectional
//!   `mentions` / `mentioned_by` edges via `add_links_batch`.
//!
//! ## Design decisions (locked in the v0.42.0.0 #1409 review)
//!
//! * D2  Hardcoded entity-type filter (not pack-aware; pack v2 is TODO-1).
//! * D6  Token-Map + multi-word phrase pass — no regex alternation, no
//!        Aho-Corasick, no new deps.
//! * D7  DB-source only — the page walk is a DB iteration; no FS access.
//! * D12 `link_source = "mentions"` (excluded from backlink ranking).
//! * D13 Self-link guard.
//! * CK12 Ignore-list applied at *gazetteer-build* time, not match time.
//!        Built-in ambiguous tokens (Apple, Amazon, …) are dropped only when
//!        no corresponding entity page exists. If the user explicitly created
//!        the page, the gazetteer presence wins.
//!
//! ## Engine-agnostic reads
//!
//! Like the reconcile-links module, this reads pages through the public
//! `BrainEngine` API (`list_pages`) instead of `execute_raw`, so it is
//! testable on `InMemoryEngine` (where `execute_raw` is unsupported) and
//! avoids the Windows libsql FFI flakiness.

use crate::engine::{BrainEngine, PageFilters};
use crate::types::{LinkBatchInput, PageKind};
use crate::Result;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

// ── constants ───────────────────────────────────────────────────────────

/// D2: hardcoded entity types for v1. Pack-aware extension is TODO-1.
pub const LINKABLE_ENTITY_TYPES: &[&str] = &["person", "company", "organization", "entity"];

/// Minimum title length for gazetteer inclusion. Filters 2–3 char names
/// (AI, YC, X, IBM) that produce dense false-positive auto-links in body
/// text. Matches TS `MIN_NAME_LENGTH`.
const MIN_NAME_LENGTH: usize = 4;

/// Built-in ignore list — common ambiguous tokens whose body mentions are
/// usually NOT references to the named brand/entity. Suppressed at
/// gazetteer-build time when no corresponding entity page exists (CK12).
const DEFAULT_IGNORE_LIST: &[&str] =
    &["Apple", "Amazon", "Square", "Stripe", "Box", "Meta", "Target", "Oracle"];

/// Chunk size for `add_links_batch` flushes (mirrors reconcile-links).
pub const MENTION_BATCH_SIZE: usize = 200;

// ── tokenizer ─────────────────────────────────────────────────────────────

/// Token-only regex. ASCII `[a-zA-Z0-9]+` runs, lowercased. Non-ASCII (CJK,
/// accented) is deliberately not tokenized in v1 — the entity gazetteer is
/// English-dominant in production today (widen to `\p{L}+` is a future
/// option). Mirrors TS `TOKEN_RE`.
static TOKEN_RE: OnceLock<Regex> = OnceLock::new();

fn token_re() -> &'static Regex {
    TOKEN_RE.get_or_init(|| Regex::new(r"[a-zA-Z0-9]+").expect("valid token regex"))
}

/// A token found during body scan, with its offset in the (length-preserving)
/// stripped text. Offset is also valid into the *original* text because
/// [`strip_code_blocks`] preserves byte length.
#[derive(Debug, Clone)]
struct ScannedToken {
    text: String,    // lowercase
    offset: usize,   // byte index in stripped (== original) text
    length: usize,   // original length (for span tracking)
}

/// Tokenize `text` into lowercased `[a-zA-Z0-9]+` runs, recording offsets.
fn tokenize_for_scan(text: &str) -> Vec<ScannedToken> {
    let re = token_re();
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        out.push(ScannedToken {
            text: m.as_str().to_ascii_lowercase(),
            offset: m.start(),
            length: m.as_str().len(),
        });
    }
    out
}

/// Tokenize a title into lowercased `[a-zA-Z0-9]+` runs (no offsets needed).
fn tokenize_title(title: &str) -> Vec<String> {
    token_re()
        .find_iter(title)
        .map(|m| m.as_str().to_ascii_lowercase())
        .collect()
}

/// Replace fenced (```) and inline (`) code spans with whitespace of
/// *equivalent length*, preserving byte offsets for any caller that cares
/// about positions. Slugs inside code are not real entity references, so
/// this is defense-in-depth. Length-preserving — the output has the same
/// byte length as the input. Faithful port of TS `stripCodeBlocks`.
fn strip_code_blocks(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < chars.len() {
        // Fenced block: ``` (optional language) ... ```
        if i + 3 <= chars.len() && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            let mut end: Option<usize> = None;
            let mut j = i + 3;
            while j + 3 <= chars.len() {
                if chars[j] == '`' && chars[j + 1] == '`' && chars[j + 2] == '`' {
                    end = Some(j);
                    break;
                }
                j += 1;
            }
            match end {
                Some(e) => {
                    for _ in i..e + 3 {
                        out.push(' ');
                    }
                    i = e + 3;
                }
                None => {
                    for _ in i..chars.len() {
                        out.push(' ');
                    }
                    break;
                }
            }
            continue;
        }
        // Inline code: `...` (single backtick, no newline inside)
        if chars[i] == '`' {
            let mut end: Option<usize> = None;
            let mut j = i + 1;
            while j < chars.len() {
                if chars[j] == '`' {
                    end = Some(j);
                    break;
                }
                if chars[j] == '\n' {
                    break;
                }
                j += 1;
            }
            match end {
                Some(e) => {
                    for _ in i..e + 1 {
                        out.push(' ');
                    }
                    i = e + 1;
                }
                None => {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ── gazetteer ─────────────────────────────────────────────────────────────

/// A single gazetteer entry (one canonical entity page).
#[derive(Debug, Clone)]
pub struct GazetteerEntry {
    /// Canonical page slug (e.g. `companies/acme-corp`).
    pub slug: String,
    /// Source id (multi-source brains). Defaults to `default`.
    pub source_id: String,
    /// Original title (preserved for the mention payload).
    pub title: String,
    /// Lowercase title tokens in order. Length 1 = single-word entity.
    pub tokens: Vec<String>,
}

/// Gazetteer keyed by lowercase FIRST token. Multiple entries can share a
/// first token (e.g. "Acme" + "Acme Corp"); at match time the scanner picks
/// the entry with the most tokens that matches the body sequence.
pub type Gazetteer = HashMap<String, Vec<GazetteerEntry>>;

/// Options for [`build_gazetteer`].
#[derive(Debug, Clone, Default)]
pub struct BuildGazetteerOpts {
    /// Optional user-supplied additional ignore-list entries (case-sensitive
    /// raw title match). Merged with [`DEFAULT_IGNORE_LIST`].
    pub extra_ignore: Option<Vec<String>>,
}

/// Build a token-Map gazetteer from all entity-typed pages in the brain.
///
/// Hardcoded type filter per D2. Soft-deleted pages excluded. Titles shorter
/// than [`MIN_NAME_LENGTH`] excluded. Ignore-list applied per CK12: built-in
/// ambiguous tokens dropped unless the user explicitly created the
/// corresponding page.
///
/// Returned gazetteer is keyed by lowercase first token; entries with the
/// same first token co-exist in the same bucket, sorted by token-count DESC
/// so maximal-munch walks longest-first.
pub async fn build_gazetteer(
    engine: &dyn BrainEngine,
    opts: BuildGazetteerOpts,
) -> Result<Gazetteer> {
    let mut entity_pages = Vec::new();
    for t in LINKABLE_ENTITY_TYPES {
        let pages = engine
            .list_pages(&PageFilters {
                page_type: Some((*t).to_string()),
                ..Default::default()
            })
            .await?;
        for p in pages {
            // Mirror TS `deleted_at IS NULL`.
            if p.deleted_at.is_none() {
                entity_pages.push(p);
            }
        }
    }

    // Pre-build the existing-title Set so the ignore-list rule can check
    // "does this name already correspond to a real page?" in O(1) (CK12).
    let existing_titles: HashSet<String> = entity_pages
        .iter()
        .filter(|p| !p.title.is_empty())
        .map(|p| p.title.clone())
        .collect();
    let mut ignore_set: HashSet<String> =
        DEFAULT_IGNORE_LIST.iter().map(|s| s.to_string()).collect();
    if let Some(extra) = &opts.extra_ignore {
        for e in extra {
            ignore_set.insert(e.clone());
        }
    }

    let mut gazetteer: Gazetteer = HashMap::new();
    for p in &entity_pages {
        // TS uses string `.length`; for ASCII titles byte == char length.
        // `.chars().count()` is the faithful UTF-16-ish length for BMP.
        if p.title.chars().count() < MIN_NAME_LENGTH {
            continue;
        }
        if ignore_set.contains(&p.title) && !existing_titles.contains(&p.title) {
            continue;
        }

        let tokens = tokenize_title(&p.title);
        if tokens.is_empty() {
            continue;
        }
        if tokens[0].chars().count() < MIN_NAME_LENGTH && tokens.len() == 1 {
            continue;
        }

        let entry = GazetteerEntry {
            slug: p.slug.clone(),
            source_id: p.source_id.clone(),
            title: p.title.clone(),
            tokens,
        };
        gazetteer
            .entry(entry.tokens[0].clone())
            .or_default()
            .push(entry);
    }

    // Sort each bucket by token-count DESC so maximal-munch walks longest-first.
    for bucket in gazetteer.values_mut() {
        bucket.sort_by(|a, b| b.tokens.len().cmp(&a.tokens.len()));
    }
    Ok(gazetteer)
}

// ── scanner (pure) ────────────────────────────────────────────────────────

/// A mention of a gazetteer entity found in body text.
#[derive(Debug, Clone, PartialEq)]
pub struct Mention {
    /// Target page slug (the entity being mentioned).
    pub slug: String,
    /// Target source id (cross-source guard).
    pub source_id: String,
    /// Display name (original title).
    pub name: String,
    /// Character offset in the ORIGINAL (un-stripped) body where the mention
    /// starts (valid because [`strip_code_blocks`] preserves offsets).
    pub offset: usize,
}

/// Options for [`find_mentioned_entities`].
#[derive(Debug, Clone)]
pub struct FindMentionsOpts {
    /// Source slug of the page being scanned. Used for self-link guard.
    pub from_slug: String,
    /// Source id of the page being scanned. Used for cross-source guard.
    pub from_source_id: String,
}

/// Scan `text` for mentions of gazetteer entities. Pure function — no IO.
/// Returns [`Mention`]s ordered by offset, deduped per `(from_slug →
/// entry.slug)` pair (first-mention-only cap).
///
/// Matcher is maximal-munch: at each token offset, the longest gazetteer
/// entry that matches the body-token sequence wins. Single-word entries are
/// length-1 maximal matches.
///
/// Guards (deterministic):
/// * D13 self-link: skip when `from_slug == entry.slug`.
/// * Cross-source: skip when `from_source_id != entry.source_id`.
/// * First-mention-only cap: dedup by `entry.slug`.
pub fn find_mentioned_entities(
    text: &str,
    gazetteer: &Gazetteer,
    opts: &FindMentionsOpts,
) -> Vec<Mention> {
    if text.is_empty() || gazetteer.is_empty() {
        return Vec::new();
    }
    let stripped = strip_code_blocks(text);
    let tokens = tokenize_for_scan(&stripped);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<Mention> = Vec::new();
    let mut seen_slugs: HashSet<String> = HashSet::new();
    let mut i = 0;

    while i < tokens.len() {
        let head = &tokens[i];
        let bucket = match gazetteer.get(&head.text) {
            Some(b) => b,
            None => {
                i += 1;
                continue;
            }
        };

        // Maximal-munch: bucket is pre-sorted longest-first. Find the first
        // entry whose subsequent tokens all match the body sequence.
        let mut matched: Option<&GazetteerEntry> = None;
        let mut matched_tokens = 0;
        for entry in bucket {
            if entry.tokens.len() == 1 {
                matched = Some(entry);
                matched_tokens = 1;
                break;
            }
            // Multi-word: validate subsequent tokens.
            if i + entry.tokens.len() > tokens.len() {
                continue;
            }
            let mut all_match = true;
            for k in 1..entry.tokens.len() {
                if tokens[i + k].text != entry.tokens[k] {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                matched = Some(entry);
                matched_tokens = entry.tokens.len();
                break;
            }
        }

        let matched = match matched {
            Some(m) => m,
            None => {
                i += 1;
                continue;
            }
        };

        // Guards.
        if matched.slug == opts.from_slug {
            i += matched_tokens;
            continue;
        }
        if matched.source_id != opts.from_source_id {
            i += matched_tokens;
            continue;
        }
        if seen_slugs.contains(&matched.slug) {
            i += matched_tokens;
            continue;
        }

        out.push(Mention {
            slug: matched.slug.clone(),
            source_id: matched.source_id.clone(),
            name: matched.title.clone(),
            offset: head.offset,
        });
        seen_slugs.insert(matched.slug.clone());
        i += matched_tokens;
    }

    out
}

// ── orchestration ─────────────────────────────────────────────────────────

/// Options for [`run_by_mention`].
#[derive(Debug, Clone, Default)]
pub struct ByMentionOpts {
    /// Scope the markdown page walk to one source (None = all sources).
    pub source_id: Option<String>,
    /// Report counts without writing any edges.
    pub dry_run: bool,
    /// Optional user-supplied additional ignore-list entries.
    pub extra_ignore: Option<Vec<String>>,
}

/// Result of a [`run_by_mention`] pass.
#[derive(Debug, Clone)]
pub struct ByMentionResult {
    /// `"ok"` after a write, `"dry_run"` when `dry_run` was set.
    pub status: String,
    /// Markdown pages scanned in the scoped source.
    pub pages_scanned: usize,
    /// Mentions found across all scanned pages.
    pub mentions_found: usize,
    /// Link rows attempted (2 per mention: forward + reverse).
    pub edges_attempted: usize,
    /// Link rows actually written (idempotent `add_links_batch` count).
    pub edges_written: usize,
}

/// Orchestrate the by-mention pass.
///
/// 1. Build a *global* gazetteer (all entity pages, all sources).
/// 2. Walk the scoped source's markdown pages (DB iteration; D7).
/// 3. For each page, scan `compiled_truth` + `timeline`, find mentions, and
///    queue bidirectional `mentions` / `mentioned_by` edges (D12:
///    `link_source = "mentions"`).
/// 4. Flush via `add_links_batch` in chunks (idempotent).
pub async fn run_by_mention(
    engine: &dyn BrainEngine,
    opts: &ByMentionOpts,
) -> Result<ByMentionResult> {
    let gazetteer = build_gazetteer(
        engine,
        BuildGazetteerOpts {
            extra_ignore: opts.extra_ignore.clone(),
        },
    )
    .await?;

    // Markdown page walk scoped to the requested source (D7). `list_pages`
    // returns full `Page` objects; filter kind + soft-delete in Rust
    // (InMemoryEngine returns deleted pages too).
    let pages = engine
        .list_pages(&PageFilters {
            source_id: opts.source_id.clone(),
            ..Default::default()
        })
        .await?;

    let mut batch: Vec<LinkBatchInput> = Vec::new();
    let mut pages_scanned: usize = 0;
    let mut mentions_found: usize = 0;

    for p in &pages {
        if p.deleted_at.is_some() {
            continue;
        }
        if p.page_kind != PageKind::Markdown {
            continue;
        }
        pages_scanned += 1;

        let haystack = format!("{}\n{}", p.compiled_truth, p.timeline);
        let mentions = find_mentioned_entities(
            &haystack,
            &gazetteer,
            &FindMentionsOpts {
                from_slug: p.slug.clone(),
                from_source_id: p.source_id.clone(),
            },
        );
        mentions_found += mentions.len();

        for m in &mentions {
            // Forward: page mentions entity.
            batch.push(LinkBatchInput {
                from_slug: p.slug.clone(),
                to_slug: m.slug.clone(),
                link_type: Some("mentions".to_string()),
                context: Some(m.name.clone()),
                link_source: Some("mentions".to_string()),
                origin_slug: Some(p.slug.clone()),
                origin_field: Some("compiled_truth".to_string()),
                from_source_id: Some(p.source_id.clone()),
                to_source_id: Some(m.source_id.clone()),
                origin_source_id: Some(p.source_id.clone()),
            });
            // Reverse: entity mentioned_by page.
            batch.push(LinkBatchInput {
                from_slug: m.slug.clone(),
                to_slug: p.slug.clone(),
                link_type: Some("mentioned_by".to_string()),
                context: Some(p.title.clone()),
                link_source: Some("mentions".to_string()),
                origin_slug: Some(p.slug.clone()),
                origin_field: Some("compiled_truth".to_string()),
                from_source_id: Some(m.source_id.clone()),
                to_source_id: Some(p.source_id.clone()),
                origin_source_id: Some(p.source_id.clone()),
            });
        }
    }

    let edges_attempted = batch.len();
    let mut edges_written: usize = 0;
    if !opts.dry_run {
        for chunk in batch.chunks(MENTION_BATCH_SIZE) {
            edges_written += engine.add_links_batch(chunk).await?;
        }
    }

    let status = if opts.dry_run {
        "dry_run".to_string()
    } else {
        "ok".to_string()
    };

    Ok(ByMentionResult {
        status,
        pages_scanned,
        mentions_found,
        edges_attempted,
        edges_written,
    })
}

// ── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BrainEngine, InMemoryEngine, PageInput};

    async fn put_entity(
        engine: &InMemoryEngine,
        slug: &str,
        page_type: &str,
        title: &str,
        source_id: &str,
    ) {
        let input = PageInput {
            page_type: page_type.to_string(),
            title: title.to_string(),
            compiled_truth: String::new(),
            timeline: None,
            page_kind: Some(PageKind::Markdown),
            ..Default::default()
        };
        engine
            .put_page(slug, Some(source_id), &input)
            .await
            .expect("put_entity");
    }

    async fn put_md(engine: &InMemoryEngine, slug: &str, body: &str, source_id: &str) {
        let input = PageInput {
            page_type: "guide".to_string(),
            title: slug.to_string(),
            compiled_truth: body.to_string(),
            timeline: None,
            page_kind: Some(PageKind::Markdown),
            ..Default::default()
        };
        engine
            .put_page(slug, Some(source_id), &input)
            .await
            .expect("put_md");
    }

    async fn put_code(engine: &InMemoryEngine, slug: &str, source_id: &str) {
        let input = PageInput {
            page_type: "guide".to_string(),
            title: slug.to_string(),
            compiled_truth: String::new(),
            timeline: None,
            page_kind: Some(PageKind::Code),
            ..Default::default()
        };
        engine
            .put_page(slug, Some(source_id), &input)
            .await
            .expect("put_code");
    }

    // ── pure: tokenizer / strip ──────────────────────────────────────────

    #[test]
    fn tokenize_for_scan_basic() {
        let toks = tokenize_for_scan("Hello World ACME-CORP");
        let texts: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["hello", "world", "acme", "corp"]);
        // Offsets are byte offsets into the original.
        assert_eq!(toks[0].offset, 0);
        assert_eq!(toks[1].offset, 6);
    }

    #[test]
    fn strip_code_blocks_replaces_with_whitespace() {
        let in_ = "use `Acme Corp` here but Acme Corp outside";
        let out = strip_code_blocks(in_);
        assert_eq!(out.len(), in_.len());
        // The inline-code span is now whitespace; the outside mention remains.
        assert!(out.contains("Acme Corp outside"));
        assert!(!out.contains("Acme Corp`"));
    }

    #[test]
    fn strip_code_blocks_handles_fenced() {
        let in_ = "mention Acme Corp\n```\nAcme Corp inside\n```\nend";
        let out = strip_code_blocks(in_);
        assert_eq!(out.len(), in_.len());
        // Fenced block replaced by spaces; only the leading mention survives.
        assert!(out.starts_with("mention Acme Corp"));
    }

    // ── pure: gazetteer build ────────────────────────────────────────────

    #[tokio::test]
    async fn build_gazetteer_excludes_short_titles() {
        let engine = InMemoryEngine::new();
        put_entity(&engine, "ai", "company", "AI", "default").await;
        let g = build_gazetteer(&engine, BuildGazetteerOpts::default())
            .await
            .unwrap();
        // "ai" has char length 2 < MIN_NAME_LENGTH → no entry.
        assert!(g.get("ai").is_none());
    }

    #[tokio::test]
    async fn build_gazetteer_keeps_ignored_when_page_exists() {
        // "Apple" is in DEFAULT_IGNORE_LIST, but a real page exists → kept (CK12).
        let engine = InMemoryEngine::new();
        put_entity(&engine, "companies/apple", "company", "Apple", "default").await;
        let g = build_gazetteer(&engine, BuildGazetteerOpts::default())
            .await
            .unwrap();
        assert!(g.get("apple").is_some());
    }

    #[tokio::test]
    async fn build_gazetteer_orders_bucket_longest_first() {
        let engine = InMemoryEngine::new();
        put_entity(&engine, "acme", "company", "Acme", "default").await;
        put_entity(&engine, "acme-corp", "company", "Acme Corp", "default").await;
        let g = build_gazetteer(&engine, BuildGazetteerOpts::default())
            .await
            .unwrap();
        let bucket = g.get("acme").expect("bucket");
        assert_eq!(bucket.len(), 2);
        // Longest first: "Acme Corp" (2 tokens) precedes "Acme" (1 token).
        assert_eq!(bucket[0].tokens.len(), 2);
        assert_eq!(bucket[0].slug, "acme-corp");
        assert_eq!(bucket[1].tokens.len(), 1);
    }

    // ── pure: scanner ────────────────────────────────────────────────────

    fn sample_gazetteer() -> Gazetteer {
        let mut g: Gazetteer = HashMap::new();
        let mut bucket = vec![
            GazetteerEntry {
                slug: "acme".to_string(),
                source_id: "default".to_string(),
                title: "Acme".to_string(),
                tokens: vec!["acme".to_string()],
            },
            GazetteerEntry {
                slug: "acme-corp".to_string(),
                source_id: "default".to_string(),
                title: "Acme Corp".to_string(),
                tokens: vec!["acme".to_string(), "corp".to_string()],
            },
        ];
        // Mirror build_gazetteer: longest-first so maximal-munch walks
        // multi-word entries before single-word ones.
        bucket.sort_by(|a, b| b.tokens.len().cmp(&a.tokens.len()));
        g.insert("acme".to_string(), bucket);
        g
    }

    #[test]
    fn find_mentions_maximal_munch() {
        let g = sample_gazetteer();
        let mentions = find_mentioned_entities(
            "I work at Acme Corp.",
            &g,
            &FindMentionsOpts {
                from_slug: "guide".to_string(),
                from_source_id: "default".to_string(),
            },
        );
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].slug, "acme-corp");
    }

    #[test]
    fn find_mentions_self_link_guard() {
        let g = sample_gazetteer();
        let mentions = find_mentioned_entities(
            "Acme Corp is mentioned here",
            &g,
            &FindMentionsOpts {
                from_slug: "acme-corp".to_string(),
                from_source_id: "default".to_string(),
            },
        );
        assert!(mentions.is_empty());
    }

    #[test]
    fn find_mentions_cross_source_guard() {
        let mut g = sample_gazetteer();
        // Entity lives in a different source than the scanning page.
        g.get_mut("acme").unwrap()[1].source_id = "srcB".to_string();
        let mentions = find_mentioned_entities(
            "I work at Acme Corp.",
            &g,
            &FindMentionsOpts {
                from_slug: "guide".to_string(),
                from_source_id: "srcA".to_string(),
            },
        );
        assert!(mentions.is_empty());
    }

    #[test]
    fn find_mentions_first_mention_only() {
        let g = sample_gazetteer();
        let mentions = find_mentioned_entities(
            "Acme Corp and Acme Corp again",
            &g,
            &FindMentionsOpts {
                from_slug: "guide".to_string(),
                from_source_id: "default".to_string(),
            },
        );
        assert_eq!(mentions.len(), 1);
    }

    #[test]
    fn find_mentions_strips_code_blocks() {
        let g = sample_gazetteer();
        // One real mention outside code, one inside an inline code span.
        let mentions = find_mentioned_entities(
            "Acme Corp outside `Acme Corp inside`",
            &g,
            &FindMentionsOpts {
                from_slug: "guide".to_string(),
                from_source_id: "default".to_string(),
            },
        );
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].slug, "acme-corp");
    }

    #[test]
    fn find_mentions_empty_inputs() {
        let g = sample_gazetteer();
        assert!(find_mentioned_entities("", &g, &FindMentionsOpts {
            from_slug: "x".to_string(),
            from_source_id: "default".to_string(),
        })
        .is_empty());
        assert!(find_mentioned_entities("plain text no entity", &HashMap::new(), &FindMentionsOpts {
            from_slug: "x".to_string(),
            from_source_id: "default".to_string(),
        })
        .is_empty());
    }

    // ── integration: run_by_mention (InMemoryEngine) ─────────────────────

    #[tokio::test]
    async fn by_mention_creates_mentions_edges() {
        let engine = InMemoryEngine::new();
        put_entity(&engine, "acme-corp", "company", "Acme Corp", "default").await;
        put_md(&engine, "guide", "We use Acme Corp for everything.", "default").await;

        let result = run_by_mention(
            &engine,
            &ByMentionOpts {
                source_id: Some("default".to_string()),
                dry_run: false,
                extra_ignore: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.pages_scanned, 2);
        assert_eq!(result.mentions_found, 1);
        assert_eq!(result.edges_attempted, 2);
        assert_eq!(result.edges_written, 2);

        // Forward edge: guide → acme-corp (mentions).
        let fwd = engine.get_links("guide", Some("default")).await.unwrap();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0].link_type, "mentions");
        assert_eq!(fwd[0].to_slug, "acme-corp");

        // Reverse edge: acme-corp → guide (mentioned_by).
        let back = engine.get_links("acme-corp", Some("default")).await.unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].link_type, "mentioned_by");
        assert_eq!(back[0].to_slug, "guide");
    }

    #[tokio::test]
    async fn by_mention_respects_dry_run() {
        let engine = InMemoryEngine::new();
        put_entity(&engine, "acme-corp", "company", "Acme Corp", "default").await;
        put_md(&engine, "guide", "We use Acme Corp for everything.", "default").await;

        let result = run_by_mention(
            &engine,
            &ByMentionOpts {
                source_id: Some("default".to_string()),
                dry_run: true,
                extra_ignore: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.status, "dry_run");
        assert_eq!(result.mentions_found, 1);
        assert_eq!(result.edges_written, 0);
        // Nothing written.
        let fwd = engine.get_links("guide", Some("default")).await.unwrap();
        assert!(fwd.is_empty());
    }

    #[tokio::test]
    async fn by_mention_only_scans_markdown_in_source() {
        let engine = InMemoryEngine::new();
        put_entity(&engine, "acme-corp", "company", "Acme Corp", "default").await;
        // Code page mentioning the entity must NOT be linked.
        put_code(&engine, "src-foo-ts", "default").await;
        // Markdown page in a *different* source must NOT be scanned.
        put_md(&engine, "other", "We use Acme Corp.", "srcB").await;

        let result = run_by_mention(
            &engine,
            &ByMentionOpts {
                source_id: Some("default".to_string()),
                dry_run: false,
                extra_ignore: None,
            },
        )
        .await
        .unwrap();

        // No *content* markdown page in `default` mentions Acme Corp
        // (only a code page + a page in srcB). The entity page itself is a
        // markdown page and is scanned, but its empty body yields no mentions.
        assert_eq!(result.pages_scanned, 1);
        assert_eq!(result.mentions_found, 0);
        let code_links = engine.get_links("src-foo-ts", Some("default")).await.unwrap();
        assert!(code_links.is_empty());
        let other_links = engine.get_links("other", Some("srcB")).await.unwrap();
        assert!(other_links.is_empty());
    }

    #[tokio::test]
    async fn by_mention_skips_short_title_entity() {
        let engine = InMemoryEngine::new();
        // "AI" (len 2) is excluded from the gazetteer.
        put_entity(&engine, "ai", "company", "AI", "default").await;
        put_md(&engine, "guide", "We use AI for everything.", "default").await;

        let result = run_by_mention(
            &engine,
            &ByMentionOpts {
                source_id: Some("default".to_string()),
                dry_run: false,
                extra_ignore: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.mentions_found, 0);
        let fwd = engine.get_links("guide", Some("default")).await.unwrap();
        assert!(fwd.is_empty());
    }
}
