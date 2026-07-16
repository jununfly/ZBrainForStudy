//! Output page validators — ported from TS `src/core/output/validators/`.
//!
//! Validators run against a page's post-write, pre-commit in-memory state and
//! emit [`ValidationFinding`]s. They are the enforcement layer behind the
//! BrainWriter's strict/lint/off validation modes.
//!
//! Four validators are ported here:
//!   - `citation`   (pure string): every factual paragraph carries a citation.
//!   - `triple-hr`  (pure string): compiled_truth/timeline split hygiene.
//!   - `link`       (engine-read): internal wikilinks point to existing pages.
//!   - `back-link`  (engine-read): outbound links have a reverse back-link.
//!
//! The pure validators are free functions. The engine-read validators take a
//! `&dyn BrainEngine` and use only read-only trait methods (`get_page`,
//! `get_links`), mirroring the TS `PageValidationContext.engine` usage without
//! introducing any new trait surface.
//!
//! The BrainWriter orchestrator, scaffold, and slug-registry remain in TS for
//! now (tracked as later output-module slices).

use crate::engine::{BrainEngine, GetPageOpts};
use std::collections::{BTreeSet, HashMap};

/// A single validation problem found on a page. Mirrors TS `ValidationFinding`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationFinding {
    pub slug: String,
    pub validator: String,
    pub severity: Severity,
    /// 1-based line number, when the finding maps to a specific line.
    pub line: Option<usize>,
    pub message: String,
}

/// Finding severity. Mirrors TS `'error' | 'warning'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// The state a validator inspects. Mirrors TS `PageValidationContext` minus the
/// `engine` field — the engine is passed separately to the engine-read
/// validators so the pure validators stay engine-free.
#[derive(Debug, Clone, Default)]
pub struct PageValidationContext {
    pub slug: String,
    /// PageType (a `String` alias in the Rust type system).
    pub page_type: String,
    pub compiled_truth: String,
    pub timeline: String,
}

// ===========================================================================
// citation validator (pure string)
// ===========================================================================

/// A paragraph extracted from compiled_truth for citation validation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Paragraph {
    /// Text with code/comments/inline-code stripped out.
    stripped: String,
    /// 1-based line number where the paragraph starts.
    start_line: usize,
}

/// citation validator: every factual paragraph in compiled_truth carries at
/// least one citation marker. Paragraph-level (not sentence-level) so it is
/// deterministic. Mirrors TS `citationValidator`.
pub fn validate_citation(ctx: &PageValidationContext) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for p in split_paragraphs(&ctx.compiled_truth) {
        if !looks_factual(&p.stripped) {
            continue;
        }
        if citation_re_matches(&p.stripped) {
            continue;
        }
        findings.push(ValidationFinding {
            slug: ctx.slug.clone(),
            validator: "citation".to_string(),
            severity: Severity::Error,
            line: Some(p.start_line),
            message: format!(
                "Paragraph has no citation marker: \"{}\"",
                truncate(&p.stripped, 80)
            ),
        });
    }
    findings
}

/// A citation marker is `[Source: <non-ws content>]` OR an inline URL link
/// `](http(s)://...)`. Case-insensitive. Mirrors TS `CITATION_RE`:
/// `/\[Source:\s*\S[^\]]*\]|\]\(\s*https?:\/\/[^)]+\)/i`.
///
/// Implemented without a regex engine: scan for either alternative.
fn citation_re_matches(s: &str) -> bool {
    has_source_marker(s) || has_inline_url_link(s)
}

/// Matches `[Source:` (case-insensitive) followed by optional whitespace, then
/// at least one non-whitespace char before the closing `]`.
fn has_source_marker(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let needle = "[source:";
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(needle) {
        let idx = search_from + rel;
        let rest = &s[idx + needle.len()..];
        if source_marker_has_content(rest) {
            return true;
        }
        search_from = idx + 1;
    }
    false
}

/// Given the text after `[Source:`, return true if before the closing `]` there
/// is at least one non-whitespace char. `\s*\S[^\]]*` then `]`.
fn source_marker_has_content(rest: &str) -> bool {
    let mut seen_content = false;
    for ch in rest.chars() {
        if ch == ']' {
            return seen_content;
        }
        if !ch.is_whitespace() {
            seen_content = true;
        }
    }
    // No closing ']' found → does not match the anchored `...]` form.
    false
}

/// Matches `](  https?://...)` — an inline markdown link with an http(s) URL.
/// `\]\(\s*https?:\/\/[^)]+\)`.
fn has_inline_url_link(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let rest = &s[i + 2..];
            if inline_url_link_body(rest) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Given text after `](`, verify `\s*https?://[^)]+)`.
fn inline_url_link_body(rest: &str) -> bool {
    let trimmed = rest.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let scheme_len = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return false;
    };
    // After the scheme there must be at least one char before ')'.
    let after = &trimmed[scheme_len..];
    let mut seen = false;
    for ch in after.chars() {
        if ch == ')' {
            return seen;
        }
        seen = true;
    }
    false
}

/// Split compiled_truth into paragraphs, dropping fenced code blocks entirely
/// and stripping inline noise. Mirrors TS `splitParagraphs`.
fn split_paragraphs(md: &str) -> Vec<Paragraph> {
    let mut out = Vec::new();
    let lines: Vec<&str> = md.split('\n').collect();

    let mut current_lines: Vec<&str> = Vec::new();
    let mut current_start_line: usize = 1;
    let mut inside_fence = false;
    let mut fence_marker = "";

    fn flush(
        out: &mut Vec<Paragraph>,
        current_lines: &mut Vec<&str>,
        current_start_line: &mut usize,
        end_line: usize,
    ) {
        if current_lines.is_empty() {
            return;
        }
        let raw = current_lines.join("\n");
        let stripped = strip_inline_noise(&raw);
        let stripped = stripped.trim().to_string();
        if !stripped.is_empty() {
            out.push(Paragraph {
                stripped,
                start_line: *current_start_line,
            });
        }
        current_lines.clear();
        *current_start_line = end_line + 1;
    }

    for (i, line) in lines.iter().enumerate() {
        let line_num = i + 1;

        if inside_fence {
            if line.starts_with(fence_marker) {
                inside_fence = false;
            }
            continue; // drop fenced lines entirely
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            inside_fence = true;
            fence_marker = if line.starts_with("```") { "```" } else { "~~~" };
            flush(&mut out, &mut current_lines, &mut current_start_line, i);
            current_start_line = line_num + 1;
            continue;
        }

        // Blank line → paragraph boundary. TS uses /^\s*$/.
        if line.trim().is_empty() {
            flush(&mut out, &mut current_lines, &mut current_start_line, i);
            current_start_line = line_num + 1;
            continue;
        }

        if current_lines.is_empty() {
            current_start_line = line_num;
        }
        current_lines.push(line);
    }
    flush(
        &mut out,
        &mut current_lines,
        &mut current_start_line,
        lines.len(),
    );

    out
}

/// Strip HTML comments and inline code, collapse whitespace. Mirrors TS
/// `stripInlineNoise`.
fn strip_inline_noise(s: &str) -> String {
    let no_comments = strip_html_comments(s);
    let no_code = strip_inline_code(&no_comments);
    collapse_whitespace(&no_code)
}

/// Remove `<!-- ... -->` spans (multiline). Each span replaced by a space.
fn strip_html_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        out.push(' ');
        if let Some(end_rel) = rest[start + 4..].find("-->") {
            rest = &rest[start + 4 + end_rel + 3..];
        } else {
            // Unterminated comment: drop the remainder.
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// Remove inline code spans ``` `...` ``` that do not cross a newline. Each
/// replaced by a space. Mirrors TS ``/`[^`\n]*`/g``.
fn strip_inline_code(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            // Find a closing backtick on the same line (no newline between).
            let mut j = i + 1;
            let mut closed = false;
            while j < chars.len() {
                if chars[j] == '\n' {
                    break;
                }
                if chars[j] == '`' {
                    closed = true;
                    break;
                }
                j += 1;
            }
            if closed {
                out.push(' ');
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Collapse any run of whitespace to a single space. Mirrors TS `/\s+/g → ' '`.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    out
}

/// Heuristic: does this paragraph make a factual claim that should carry a
/// citation? Mirrors TS `looksFactual`.
fn looks_factual(stripped: &str) -> bool {
    if stripped.is_empty() {
        return false;
    }
    // Heading: `#{1,6}\s`
    if is_heading(stripped) {
        return false;
    }
    // Blockquote: `^>`
    if stripped.starts_with('>') {
        return false;
    }
    // Pure key-value line: `^[-*]?\s*\*\*[^*]+:\*\*\s*\S[^.]*$` AND no '.'
    if is_key_value_line(stripped) && !stripped.contains('.') {
        return false;
    }
    // Table row: `^\s*\|.+\|\s*$`
    if is_table_row(stripped) {
        return false;
    }
    // Bullet of only a wikilink/url: `^[-*]\s*\[[^\]]+\]\([^)]+\)\s*$`
    if is_bullet_only_link(stripped) {
        return false;
    }
    // Short labels without a verb-ish word.
    if stripped.chars().count() < 40 && !has_factual_verb(stripped) {
        return false;
    }
    true
}

/// `^#{1,6}\s`
fn is_heading(s: &str) -> bool {
    let hashes = s.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return false;
    }
    s[hashes..].starts_with(|c: char| c.is_whitespace())
}

/// `^[-*]?\s*\*\*[^*]+:\*\*\s*\S[^.]*$` — a bold "**Key:**" label with a value.
fn is_key_value_line(s: &str) -> bool {
    let mut rest = s;
    // Optional leading bullet marker.
    if rest.starts_with('-') || rest.starts_with('*') {
        // Only strip a single `-`/`*` if it's a bullet (`* ` / `- `), not the
        // `**` bold opener. TS `[-*]?` is a single optional char.
        if !rest.starts_with("**") {
            rest = &rest[1..];
        }
    }
    rest = rest.trim_start();
    if !rest.starts_with("**") {
        return false;
    }
    let after_open = &rest[2..];
    // `[^*]+:` then `**`
    let close_rel = match after_open.find("**") {
        Some(v) => v,
        None => return false,
    };
    let label = &after_open[..close_rel];
    if label.is_empty() || label.contains('*') || !label.ends_with(':') {
        return false;
    }
    // Label without the trailing ':' must be non-empty (`[^*]+` before `:`).
    if label.len() == 1 {
        return false;
    }
    let after_close = &after_open[close_rel + 2..];
    // `\s*\S[^.]*$`
    let value = after_close.trim_start();
    !value.is_empty()
}

/// `^\s*\|.+\|\s*$`
fn is_table_row(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('|') && t.ends_with('|') && t.len() > 2
}

/// `^[-*]\s*\[[^\]]+\]\([^)]+\)\s*$`
fn is_bullet_only_link(s: &str) -> bool {
    let mut rest = s;
    if !(rest.starts_with('-') || rest.starts_with('*')) {
        return false;
    }
    rest = rest[1..].trim_start();
    // `\[[^\]]+\]`
    if !rest.starts_with('[') {
        return false;
    }
    let close_br = match rest.find(']') {
        Some(v) => v,
        None => return false,
    };
    if close_br <= 1 {
        return false; // needs at least one char inside [...]
    }
    let after_br = &rest[close_br + 1..];
    if !after_br.starts_with('(') {
        return false;
    }
    let close_paren = match after_br.find(')') {
        Some(v) => v,
        None => return false,
    };
    if close_paren <= 1 {
        return false; // needs at least one char inside (...)
    }
    // Nothing but whitespace after the closing paren.
    after_br[close_paren + 1..].trim().is_empty()
}

/// Case-insensitive whole-word match for the TS verb list.
fn has_factual_verb(s: &str) -> bool {
    const VERBS: &[&str] = &[
        "is", "was", "were", "has", "have", "had", "will", "would", "built",
        "raised", "founded", "said", "wrote", "attended", "works", "joined",
        "left", "shipped",
    ];
    let lower = s.to_ascii_lowercase();
    for word in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if VERBS.contains(&word) {
            return true;
        }
    }
    false
}

/// Truncate to `n` chars, appending `...` when over. Mirrors TS `truncate`:
/// `s.length <= n ? s : s.slice(0, n - 3) + '...'`. Operates on chars.
fn truncate(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n.saturating_sub(3)).collect();
    format!("{head}...")
}

// ===========================================================================
// triple-hr validator (pure string)
// ===========================================================================

/// triple-hr validator: compiled_truth/timeline split hygiene. Mirrors TS
/// `tripleHrValidator`. Both cases are warning-severity, one finding max each.
pub fn validate_triple_hr(ctx: &PageValidationContext) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    // Case 1: standalone `---` inside compiled_truth (outside code fences).
    let mut inside_fence = false;
    let mut fence_marker = "";
    for (i, line) in ctx.compiled_truth.split('\n').enumerate() {
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
        // `^-{3,}\s*$`
        if is_bare_hr_line(line) {
            findings.push(ValidationFinding {
                slug: ctx.slug.clone(),
                validator: "triple-hr".to_string(),
                severity: Severity::Warning,
                line: Some(i + 1),
                message: "Bare \"---\" line in compiled_truth would re-split on round-trip. Use spaced em-dash or thematic-break inside a list context.".to_string(),
            });
            break; // one finding per page is enough
        }
    }

    // Case 2: timeline has a heading that looks like spilled compiled-truth.
    for (i, raw_line) in ctx.timeline.split('\n').enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip the top-level "## Timeline" header: `^##\s+Timeline\s*$` (i).
        if is_timeline_header(line) {
            continue;
        }
        if is_heading(line) {
            findings.push(ValidationFinding {
                slug: ctx.slug.clone(),
                validator: "triple-hr".to_string(),
                severity: Severity::Warning,
                line: Some(i + 1),
                message: format!(
                    "Heading in timeline section: \"{}\". Timeline entries should be append-only bullet lines.",
                    truncate(line, 60)
                ),
            });
            break;
        }
    }

    findings
}

/// `^-{3,}\s*$` — three or more dashes then only whitespace.
fn is_bare_hr_line(line: &str) -> bool {
    let dashes = line.chars().take_while(|&c| c == '-').count();
    if dashes < 3 {
        return false;
    }
    line[dashes..].trim().is_empty()
}

/// `^##\s+Timeline\s*$` case-insensitive.
fn is_timeline_header(line: &str) -> bool {
    if !line.starts_with("##") {
        return false;
    }
    let after = &line[2..];
    // Require at least one whitespace then "Timeline" then only whitespace.
    if !after.starts_with(|c: char| c.is_whitespace()) {
        return false;
    }
    after.trim().eq_ignore_ascii_case("Timeline")
}

// ===========================================================================
// link validator (engine-read)
// ===========================================================================

/// link validator: brain-internal wikilinks point to pages that exist. Scans
/// compiled_truth + timeline for markdown links, classifies them, and checks
/// internal targets against the engine. Mirrors TS `linkValidator`.
pub async fn validate_link(
    engine: &dyn BrainEngine,
    ctx: &PageValidationContext,
) -> crate::Result<Vec<ValidationFinding>> {
    let mut findings = Vec::new();
    let body = format!("{}\n{}", ctx.compiled_truth, ctx.timeline);

    // Collect unique internal targets first to batch engine lookups. Preserve
    // per-target link positions (order-preserving) for dangling diagnostics.
    let mut internal_targets: BTreeSet<String> = BTreeSet::new();
    let mut link_positions: Vec<(String, usize)> = Vec::new();

    for (href, line) in iterate_links(&body) {
        if is_external_url(&href) {
            continue;
        }
        if is_non_brain_ref(&href) {
            findings.push(ValidationFinding {
                slug: ctx.slug.clone(),
                validator: "link".to_string(),
                severity: Severity::Warning,
                line: Some(line),
                message: format!(
                    "Non-brain link (mailto/anchor/scheme): {}",
                    truncate(&href, 80)
                ),
            });
            continue;
        }
        match normalize_to_slug(&href) {
            None => {
                findings.push(ValidationFinding {
                    slug: ctx.slug.clone(),
                    validator: "link".to_string(),
                    severity: Severity::Warning,
                    line: Some(line),
                    message: format!("Unresolvable link path: {}", truncate(&href, 80)),
                });
            }
            Some(slug) => {
                internal_targets.insert(slug.clone());
                link_positions.push((slug, line));
            }
        }
    }

    // Batch-check which targets exist. BTreeSet gives deterministic ordering.
    for slug in &internal_targets {
        let page = engine.get_page(slug, &GetPageOpts::default()).await?;
        if page.is_some() {
            continue;
        }
        for (target, line) in &link_positions {
            if target == slug {
                findings.push(ValidationFinding {
                    slug: ctx.slug.clone(),
                    validator: "link".to_string(),
                    severity: Severity::Error,
                    line: Some(*line),
                    message: format!("Dangling wikilink to {slug} (no such page)"),
                });
            }
        }
    }

    Ok(findings)
}

/// `^https?://` case-insensitive. Mirrors TS `isExternalUrl`.
pub fn is_external_url(href: &str) -> bool {
    let lower = href.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// `^(mailto:|tel:|javascript:|data:|#)` case-insensitive. Mirrors TS
/// `isNonBrainRef`.
pub fn is_non_brain_ref(href: &str) -> bool {
    let lower = href.to_ascii_lowercase();
    lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with('#')
}

/// Normalize a link href to a brain slug, or `None` if not slug-shaped.
/// Mirrors TS `normalizeToSlug`.
pub fn normalize_to_slug(href: &str) -> Option<String> {
    let mut s = href.trim().to_string();
    // Strip repeated leading relative-path components (./, ../, multi-level).
    // TS loops `/^\.\.?\/+/` until no match.
    loop {
        let stripped = strip_leading_rel(&s);
        if stripped.len() == s.len() {
            break;
        }
        s = stripped;
    }
    // Strip leading slashes.
    while s.starts_with('/') {
        s = s[1..].to_string();
    }
    // Strip trailing `.md` (case-insensitive).
    if s.len() >= 3 && s[s.len() - 3..].eq_ignore_ascii_case(".md") {
        s = s[..s.len() - 3].to_string();
    }
    // Must look like dir/name (or dir/name/subname):
    // `^[a-z0-9][a-z0-9\-]*(\/[a-z0-9][a-z0-9\-]*)+$` (i).
    if !is_slug_shaped(&s) {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

/// Strip a single leading `./` or `../` (with one-or-more trailing slashes),
/// matching `^\.\.?\/+`.
fn strip_leading_rel(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes[0] != b'.' {
        return s.to_string();
    }
    // One or two dots.
    let mut idx = 1;
    if bytes.len() > 1 && bytes[1] == b'.' {
        idx = 2;
    }
    // Require at least one slash after the dot(s).
    if idx >= bytes.len() || bytes[idx] != b'/' {
        return s.to_string();
    }
    // Consume all consecutive slashes.
    while idx < bytes.len() && bytes[idx] == b'/' {
        idx += 1;
    }
    s[idx..].to_string()
}

/// `^[a-z0-9][a-z0-9\-]*(\/[a-z0-9][a-z0-9\-]*)+$` case-insensitive.
fn is_slug_shaped(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let segments: Vec<&str> = s.split('/').collect();
    if segments.len() < 2 {
        return false; // needs at least one '/'.
    }
    for seg in segments {
        if !is_slug_segment(seg) {
            return false;
        }
    }
    true
}

/// `[a-z0-9][a-z0-9\-]*` case-insensitive: alnum start, then alnum-or-dash.
fn is_slug_segment(seg: &str) -> bool {
    let mut chars = seg.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Iterate markdown links as `(href, 1-based-line)`, skipping fenced code
/// blocks and inline code. Mirrors TS `iterateLinks` + `MD_LINK_RE`
/// `/\[([^\]]+)\]\(([^)]+)\)/g`.
fn iterate_links(body: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut inside_fence = false;
    let mut fence_marker = "";
    for (i, line) in body.split('\n').enumerate() {
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
        let cleaned = strip_inline_code(line);
        for href in extract_md_link_hrefs(&cleaned) {
            out.push((href, i + 1));
        }
    }
    out
}

/// Extract href parts of `[text](href)` links on a single line. `text` is
/// `[^\]]+` (non-empty, no `]`), `href` is `[^)]+` (non-empty, no `)`).
fn extract_md_link_hrefs(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            // Find closing ']' with no ']' inside (i.e. first ']') and at least
            // one char of text.
            let text_start = i + 1;
            let mut j = text_start;
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            if j < chars.len() && j > text_start && j + 1 < chars.len() && chars[j + 1] == '(' {
                // Parse href up to first ')'.
                let href_start = j + 2;
                let mut k = href_start;
                while k < chars.len() && chars[k] != ')' {
                    k += 1;
                }
                if k < chars.len() && k > href_start {
                    let href: String = chars[href_start..k].iter().collect();
                    out.push(href);
                    i = k + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

// ===========================================================================
// back-link validator (engine-read)
// ===========================================================================

/// back-link validator: every outbound link has a reverse back-link (the Iron
/// Law). Missing reverses are warnings. Mirrors TS `backLinkValidator`.
pub async fn validate_back_link(
    engine: &dyn BrainEngine,
    ctx: &PageValidationContext,
) -> crate::Result<Vec<ValidationFinding>> {
    let mut findings = Vec::new();

    let outbound = engine.get_links(&ctx.slug, None).await?;
    if outbound.is_empty() {
        return Ok(findings);
    }

    // Unique targets, deterministic order.
    let mut unique_targets: BTreeSet<String> = BTreeSet::new();
    for link in &outbound {
        unique_targets.insert(link.to_slug.clone());
    }

    for target in &unique_targets {
        let target_outbound = engine.get_links(target, None).await?;
        let has_reverse = target_outbound.iter().any(|l| l.to_slug == ctx.slug);
        if !has_reverse {
            findings.push(ValidationFinding {
                slug: ctx.slug.clone(),
                validator: "back-link".to_string(),
                severity: Severity::Warning,
                line: None,
                message: format!(
                    "Outbound link to {target} has no back-link ({target} does not reference {}). runAutoLink should reconcile this on next put_page; flag for inspection.",
                    ctx.slug
                ),
            });
        }
    }

    Ok(findings)
}

/// The ids of the built-in pure (engine-free) validators, in registration
/// order. The engine-read validators (`link`, `back-link`) are invoked
/// separately by callers that hold an engine handle. Mirrors the TS
/// `registerBuiltinValidators` id set.
pub const BUILTIN_VALIDATOR_IDS: &[&str] = &["citation", "link", "back-link", "triple-hr"];

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(compiled: &str, timeline: &str) -> PageValidationContext {
        PageValidationContext {
            slug: "people/alice".to_string(),
            page_type: "person".to_string(),
            compiled_truth: compiled.to_string(),
            timeline: timeline.to_string(),
        }
    }

    // ---- citation validator ------------------------------------------------

    #[test]
    fn citation_flags_factual_paragraph_missing_citation() {
        let c = ctx("Alice founded Acme in 2020 and raised a seed round.", "");
        let f = validate_citation(&c);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].validator, "citation");
        assert_eq!(f[0].severity, Severity::Error);
        assert_eq!(f[0].line, Some(1));
    }

    #[test]
    fn citation_passes_with_source_marker() {
        let c = ctx("Alice founded Acme in 2020. [Source: crunchbase]", "");
        assert!(validate_citation(&c).is_empty());
    }

    #[test]
    fn citation_passes_with_inline_url() {
        let c = ctx(
            "Alice founded Acme in 2020 per [the filing](https://sec.gov/x).",
            "",
        );
        assert!(validate_citation(&c).is_empty());
    }

    #[test]
    fn citation_empty_source_marker_does_not_satisfy() {
        let c = ctx("Alice founded Acme and raised money. [Source:]", "");
        assert_eq!(validate_citation(&c).len(), 1);
    }

    #[test]
    fn citation_whitespace_only_source_marker_does_not_satisfy() {
        let c = ctx("Alice founded Acme and raised money. [Source:   ]", "");
        assert_eq!(validate_citation(&c).len(), 1);
    }

    #[test]
    fn citation_ignores_headings_and_kv_and_bullet_links() {
        // Heading, key-value, wikilink bullet — none are factual paragraphs.
        let c = ctx(
            "## Overview\n\n**Status:** Active\n\n- [Acme](companies/acme.md)",
            "",
        );
        assert!(validate_citation(&c).is_empty());
    }

    #[test]
    fn citation_ignores_fenced_code_and_inline_code() {
        // Code fence content is dropped; the surrounding factual text needs a
        // citation though, so provide one.
        let c = ctx(
            "```\nAlice founded Acme with no citation here\n```\n\nUse `zbrain init` to start. [Source: docs]",
            "",
        );
        assert!(validate_citation(&c).is_empty());
    }

    #[test]
    fn citation_short_label_without_verb_is_not_factual() {
        let c = ctx("Acme Corporation", "");
        assert!(validate_citation(&c).is_empty());
    }

    #[test]
    fn citation_inline_code_stripped_but_paragraph_still_needs_citation() {
        // TS: 'Alice shipped `zbrain` last week.' → 1 finding (verb "shipped",
        // no citation). Inline code is removed but the prose is still factual.
        let c = ctx("Alice shipped `zbrain` last week.", "");
        assert_eq!(validate_citation(&c).len(), 1);
    }

    #[test]
    fn citation_ignores_blockquote() {
        let c = ctx("> quoted content without citation", "");
        assert!(validate_citation(&c).is_empty());
    }

    #[test]
    fn citation_ignores_html_comment_only() {
        let c = ctx("<!-- This is a note -->", "");
        assert!(validate_citation(&c).is_empty());
    }

    #[test]
    fn split_paragraphs_blank_line_separation_line_numbers() {
        let out = split_paragraphs("First para.\n\nSecond para.");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].start_line, 1);
        assert_eq!(out[1].start_line, 3);
    }

    // ---- triple-hr validator ----------------------------------------------

    #[test]
    fn triple_hr_flags_bare_hr_in_compiled_truth() {
        let c = ctx("Above the bar.\n\n---\n\nBelow.", "");
        let f = validate_triple_hr(&c);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].validator, "triple-hr");
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].line, Some(3));
    }

    #[test]
    fn triple_hr_ignores_hr_inside_code_fence() {
        let c = ctx("Text.\n\n```\n---\n```\n", "");
        assert!(validate_triple_hr(&c).is_empty());
    }

    #[test]
    fn triple_hr_flags_heading_in_timeline() {
        let c = ctx("Body.", "## Timeline\n### Spilled heading\n- **2020** | x");
        let f = validate_triple_hr(&c);
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("Heading in timeline section"));
        assert_eq!(f[0].line, Some(2));
    }

    #[test]
    fn triple_hr_timeline_header_itself_is_ok() {
        let c = ctx("Body.", "## Timeline\n- **2020** | founded");
        assert!(validate_triple_hr(&c).is_empty());
    }

    // ---- helper unit coverage ---------------------------------------------

    #[test]
    fn normalize_to_slug_strips_rel_and_ext() {
        assert_eq!(
            normalize_to_slug("../../people/alice-smith.md"),
            Some("people/alice-smith".to_string())
        );
        assert_eq!(
            normalize_to_slug("/companies/acme"),
            Some("companies/acme".to_string())
        );
        assert_eq!(normalize_to_slug("just-a-word"), None); // no '/'
        // A raw URL is not slug-shaped (the `https:` segment has a colon).
        assert_eq!(normalize_to_slug("https://x.com/y"), None);
    }

    #[test]
    fn external_and_non_brain_ref_classification() {
        assert!(is_external_url("https://a.com"));
        assert!(is_external_url("HTTP://a.com"));
        assert!(!is_external_url("people/alice"));
        assert!(is_non_brain_ref("mailto:a@b.com"));
        assert!(is_non_brain_ref("#anchor"));
        assert!(!is_non_brain_ref("people/alice"));
    }

    #[test]
    fn truncate_appends_ellipsis_over_limit() {
        assert_eq!(truncate("abc", 80), "abc");
        let long = "a".repeat(100);
        let t = truncate(&long, 80);
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with("..."));
    }

    // ---- link validator (engine-read) -------------------------------------

    use crate::engine::{InMemoryEngine, PageInput};
    use crate::types::LinkBatchInput;

    async fn put(engine: &InMemoryEngine, slug: &str) {
        engine
            .put_page(
                slug,
                None,
                &PageInput {
                    page_type: "person".to_string(),
                    title: slug.to_string(),
                    compiled_truth: "body".to_string(),
                    ..Default::default()
                },
            )
            .await
            .expect("put_page");
    }

    fn link(from: &str, to: &str) -> LinkBatchInput {
        LinkBatchInput {
            from_slug: from.to_string(),
            to_slug: to.to_string(),
            link_type: None,
            context: None,
            link_source: None,
            origin_slug: None,
            origin_field: None,
            from_source_id: None,
            to_source_id: None,
            origin_source_id: None,
        }
    }

    #[tokio::test]
    async fn link_flags_dangling_wikilink() {
        let engine = InMemoryEngine::new();
        // No target page created → dangling.
        let c = ctx("See [Bob](people/bob-jones.md) for details.", "");
        let f = validate_link(&engine, &c).await.expect("validate_link");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("Dangling wikilink to people/bob-jones"));
    }

    #[tokio::test]
    async fn link_passes_when_target_exists() {
        let engine = InMemoryEngine::new();
        put(&engine, "people/bob-jones").await;
        let c = ctx("See [Bob](people/bob-jones.md) for details.", "");
        let f = validate_link(&engine, &c).await.expect("validate_link");
        assert!(f.is_empty());
    }

    #[tokio::test]
    async fn link_warns_on_non_brain_ref() {
        let engine = InMemoryEngine::new();
        let c = ctx("Email [me](mailto:a@b.com).", "");
        let f = validate_link(&engine, &c).await.expect("validate_link");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("Non-brain link"));
    }

    #[tokio::test]
    async fn link_skips_external_urls() {
        let engine = InMemoryEngine::new();
        let c = ctx("See [docs](https://example.com/page).", "");
        let f = validate_link(&engine, &c).await.expect("validate_link");
        assert!(f.is_empty());
    }

    #[tokio::test]
    async fn link_ignores_links_in_code_fence() {
        let engine = InMemoryEngine::new();
        let c = ctx("```\n[Bob](people/bob.md)\n```\n", "");
        let f = validate_link(&engine, &c).await.expect("validate_link");
        assert!(f.is_empty());
    }

    // ---- back-link validator (engine-read) --------------------------------

    #[tokio::test]
    async fn back_link_no_outbound_no_findings() {
        let engine = InMemoryEngine::new();
        let c = ctx("body", "");
        let f = validate_back_link(&engine, &c).await.expect("validate_back_link");
        assert!(f.is_empty());
    }

    #[tokio::test]
    async fn back_link_missing_reverse_warns() {
        let engine = InMemoryEngine::new();
        put(&engine, "people/alice").await;
        put(&engine, "people/bob").await;
        // alice → bob, but bob has no reverse link.
        engine
            .add_links_batch(&[link("people/alice", "people/bob")])
            .await
            .expect("add_links_batch");
        let c = ctx("body", "");
        let f = validate_back_link(&engine, &c).await.expect("validate_back_link");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("has no back-link"));
    }

    #[tokio::test]
    async fn back_link_bidirectional_no_findings() {
        let engine = InMemoryEngine::new();
        put(&engine, "people/alice").await;
        put(&engine, "people/bob").await;
        engine
            .add_links_batch(&[
                link("people/alice", "people/bob"),
                link("people/bob", "people/alice"),
            ])
            .await
            .expect("add_links_batch");
        let c = ctx("body", "");
        let f = validate_back_link(&engine, &c).await.expect("validate_back_link");
        assert!(f.is_empty());
    }
}
