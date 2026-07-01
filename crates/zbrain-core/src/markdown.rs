//! Markdown parsing and metadata inference.
//!
//! Ported from TS `src/core/markdown.ts` — `parseMarkdown`, `splitBody`,
//! `inferType`, `inferTitle`, `inferSlug`, `inferTags`.
//!
//! Part of roadmap node 1-7-1-4: Content extraction.

use serde_json::Value;
use std::collections::HashMap;

// ─── Path prefix → type mapping (ZBRAIN_BASE_PATH_PREFIXES) ───────────

/// Built-in path prefix → page type mapping.
/// Mirrors TS `ZBRAIN_BASE_PATH_PREFIXES`.
static BASE_PATH_PREFIXES: &[(&str, &str)] = &[
    ("people", "person"),
    ("person", "person"),
    ("companies", "company"),
    ("company", "company"),
    ("projects", "project"),
    ("project", "project"),
    ("products", "product"),
    ("product", "product"),
    ("tools", "tool"),
    ("tool", "tool"),
    ("events", "event"),
    ("event", "event"),
    ("meetings", "meeting"),
    ("meeting", "meeting"),
    ("decisions", "decision"),
    ("decision", "decision"),
    ("tasks", "task"),
    ("task", "task"),
    ("notes", "note"),
    ("note", "note"),
    ("docs", "doc"),
    ("doc", "doc"),
    ("articles", "article"),
    ("article", "article"),
    ("topics", "topic"),
    ("topic", "topic"),
    ("tags", "tag"),
    ("tag", "tag"),
    ("concepts", "concept"),
    ("concept", "concept"),
    ("sources", "source"),
    ("source", "source"),
];

// ─── ParsedMarkdown output ─────────────────────────────────────────────

/// Result of parsing a markdown file.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMarkdown {
    /// Merged frontmatter (from YAML block + inferred defaults).
    pub frontmatter: Value,
    /// Body content before the timeline split.
    pub compiled_truth: String,
    /// Timeline content after the split delimiter (empty if no timeline).
    pub timeline: String,
    /// Inferred page type (e.g. "person", "note").
    pub type_: String,
    /// Inferred title.
    pub title: String,
    /// Inferred slug.
    pub slug: String,
    /// Inferred tags.
    pub tags: Vec<String>,
}

// ─── splitBody ─────────────────────────────────────────────────────────

/// Timeline delimiter patterns (in priority order).
const TIMELINE_DELIMITERS: &[&str] = &["<!-- timeline -->", "--- timeline ---", "## Timeline"];

/// Split markdown body into `(compiled_truth, timeline)`.
///
/// Scans for timeline delimiters. Everything before the first delimiter is
/// `compiled_truth`; everything after is `timeline`. If no delimiter is
/// found, `timeline` is empty and the whole body is `compiled_truth`.
///
/// Mirrors TS `splitBody`.
pub fn split_body(body: &str) -> (String, String) {
    for delim in TIMELINE_DELIMITERS {
        if let Some(pos) = body.find(delim) {
            let compiled = body[..pos].trim().to_string();
            let timeline = body[pos + delim.len()..].trim().to_string();
            return (compiled, timeline);
        }
    }
    (body.trim().to_string(), String::new())
}

// ─── inferType ─────────────────────────────────────────────────────────

/// Infer page type from frontmatter, path, or pack prefix mapping.
///
/// Priority:
/// 1. `frontmatter.type` (string)
/// 2. First path segment matches built-in `BASE_PATH_PREFIXES`
/// 3. First path segment matches user-supplied `path_prefixes`
/// 4. Default: `"note"`
///
/// Mirrors TS `inferType` / `inferTypeFromPack`.
pub fn infer_type(
    body_path: &str,
    frontmatter: &Value,
    path_prefixes: Option<&HashMap<String, String>>,
) -> String {
    // 1. frontmatter.type
    if let Some(t) = frontmatter.get("type").and_then(|v| v.as_str()) {
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }

    // Extract first path segment
    let first_seg = first_path_segment(body_path);

    // 2. Built-in path prefixes
    if !first_seg.is_empty() {
        for &(prefix, typ) in BASE_PATH_PREFIXES {
            if first_seg == prefix {
                return typ.to_string();
            }
        }
    }

    // 3. User-supplied path prefixes (from schema pack)
    if let Some(map) = path_prefixes {
        if !first_seg.is_empty() {
            if let Some(typ) = map.get(first_seg) {
                return typ.clone();
            }
        }
    }

    // 4. Default
    "note".to_string()
}

/// Extract the first segment of a path (e.g. "people/alice.md" → "people").
fn first_path_segment(path: &str) -> &str {
    let path = path.trim_start_matches('/').trim_start_matches(".\\").trim_start_matches("./");
    path.split(['/', '\\']).next().unwrap_or("")
}

// ─── inferTitle ────────────────────────────────────────────────────────

/// Infer page title.
///
/// Priority:
/// 1. `frontmatter.title` (string)
/// 2. First `# Heading` (h1) in body
/// 3. File name without extension, humanized (e.g. "my-note" → "My note")
///
/// Mirrors TS `inferTitle`.
pub fn infer_title(frontmatter: &Value, body: &str, body_path: &str) -> String {
    // 1. frontmatter.title
    if let Some(t) = frontmatter.get("title").and_then(|v| v.as_str()) {
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }

    // 2. First h1 heading
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            let title = trimmed[2..].trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }

    // 3. File name
    file_stem_to_title(body_path)
}

/// Convert a file stem to a human-readable title.
/// "my-project-notes" → "My project notes"
/// Only the first word is capitalized; rest remain lowercase.
fn file_stem_to_title(path: &str) -> String {
    let stem = extract_file_stem(path);
    if stem.is_empty() {
        return "Untitled".to_string();
    }
    // Split on - and _, capitalize only the first word
    let words: Vec<String> = stem
        .split(['-', '_'])
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, w)| {
            if i == 0 {
                // Capitalize first word
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        let mut result = first.to_uppercase().collect::<String>();
                        result.push_str(&chars.as_str().to_lowercase());
                        result
                    }
                }
            } else {
                w.to_lowercase()
            }
        })
        .collect();
    if words.is_empty() {
        "Untitled".to_string()
    } else {
        words.join(" ")
    }
}

/// Extract file stem (name without extension) from a path.
/// Returns empty string for dotfiles (e.g. ".md", ".gitignore").
fn extract_file_stem(path: &str) -> &str {
    // Get the file name component
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path);
    // Remove extension
    match name.rfind('.') {
        Some(pos) if pos > 0 => &name[..pos],
        Some(0) => "", // dotfile: ".md" → stem is empty
        _ => name,
    }
}

/// Check if a path has directory components (contains / or \).
fn has_directory(path: &str) -> bool {
    path.contains('/') || path.contains('\\')
}

// ─── inferSlug ─────────────────────────────────────────────────────────

/// Infer page slug.
///
/// Priority:
/// 1. `frontmatter.slug`
/// 2. File name without extension, slugified
/// 3. Full path (without extension), slugified
///
/// Mirrors TS `inferSlug`.
pub fn infer_slug(frontmatter: &Value, body_path: &str) -> String {
    // 1. frontmatter.slug
    if let Some(s) = frontmatter.get("slug").and_then(|v| v.as_str()) {
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }

    // 2. File name stem, slugified (when stem is a meaningful extract)
    let stem = extract_file_stem(body_path);
    if !stem.is_empty() && stem != body_path {
        return slugify(stem);
    }

    // 3. Full path (without extension), slugified
    let path_no_ext = if let Some(pos) = body_path.rfind('.') {
        &body_path[..pos]
    } else {
        body_path
    };
    slugify(path_no_ext)
}

/// Simple slugify: lowercase, replace non-alphanumeric with hyphens,
/// trim leading/trailing hyphens. Does NOT collapse consecutive hyphens
/// (matches TS behavior: path segments are separated by single hyphens,
/// but special characters in filenames produce consecutive hyphens).
fn slugify(s: &str) -> String {
    let s = s.to_lowercase();
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            result.push(ch);
        } else {
            result.push('-');
        }
    }
    result.trim_matches('-').to_string()
}

// ─── inferTags ─────────────────────────────────────────────────────────

/// Infer tags from frontmatter or path.
///
/// Priority:
/// 1. `frontmatter.tags` (JSON array of strings)
/// 2. Directory path segments (excluding file name)
/// 3. Empty
///
/// Mirrors TS `inferTags`.
pub fn infer_tags(frontmatter: &Value, body_path: &str) -> Vec<String> {
    // 1. frontmatter.tags
    if let Some(tags) = frontmatter.get("tags") {
        if let Some(arr) = tags.as_array() {
            let result: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !result.is_empty() {
                return result;
            }
        }
        // Also handle comma-separated string
        if let Some(s) = tags.as_str() {
            let result: Vec<String> = s
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if !result.is_empty() {
                return result;
            }
        }
    }

    // 2. Directory path segments
    let dir_segments = path_directory_segments(body_path);
    if !dir_segments.is_empty() {
        return dir_segments;
    }

    vec![]
}

/// Extract directory segments from a path (excluding the file name).
/// "people/engineering/alice.md" → ["people", "engineering"]
fn path_directory_segments(path: &str) -> Vec<String> {
    let path = path.trim_start_matches('/').trim_start_matches(".\\").trim_start_matches("./");
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts.len() <= 1 {
        return vec![];
    }
    // All parts except the last (file name)
    parts[..parts.len() - 1]
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ─── parse_markdown (main entry) ───────────────────────────────────────

/// Parse a markdown body into structured `ParsedMarkdown`.
///
/// This is the main entry point for the extraction layer. It:
/// 1. Parses YAML frontmatter from the body (via `parse_frontmatter_from_body`
///    from the `capture` module)
/// 2. Splits body into compiled_truth + timeline
/// 3. Infers type, title, slug, tags
///
/// `body_path` is the file path (used for type/title/slug/tag inference).
/// `path_prefixes` is an optional schema pack path→type mapping.
pub fn parse_markdown(
    body: &str,
    body_path: &str,
    path_prefixes: Option<&HashMap<String, String>>,
) -> ParsedMarkdown {
    // 1. Parse frontmatter from body
    let (frontmatter, body_without_fm) =
        crate::capture::parse_frontmatter_from_body(body).unwrap_or_else(|_| {
            // If YAML parsing fails, treat whole body as content with empty frontmatter
            (Some(serde_json::Value::Object(Default::default())), body.to_string())
        });
    let fm = frontmatter.unwrap_or_else(|| serde_json::Value::Object(Default::default()));

    // 2. Split body into compiled_truth + timeline
    let (compiled_truth, timeline) = split_body(&body_without_fm);

    // 3. Infer metadata
    let type_ = infer_type(body_path, &fm, path_prefixes);
    let title = infer_title(&fm, &compiled_truth, body_path);
    let slug = infer_slug(&fm, body_path);
    let tags = infer_tags(&fm, body_path);

    ParsedMarkdown {
        frontmatter: fm,
        compiled_truth,
        timeline,
        type_,
        title,
        slug,
        tags,
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── split_body ─────────────────────────────────────────────────────

    #[test]
    fn split_body_no_timeline() {
        let (compiled, timeline) = split_body("Hello world\n\nMore text.");
        assert_eq!(compiled, "Hello world\n\nMore text.");
        assert!(timeline.is_empty());
    }

    #[test]
    fn split_body_html_comment_delimiter() {
        let body = "Main content.\n\n<!-- timeline -->\n\nTimeline stuff.";
        let (compiled, timeline) = split_body(body);
        assert_eq!(compiled, "Main content.");
        assert_eq!(timeline, "Timeline stuff.");
    }

    #[test]
    fn split_body_dash_delimiter() {
        let body = "Main.\n\n--- timeline ---\n\nHistory.";
        let (compiled, timeline) = split_body(body);
        assert_eq!(compiled, "Main.");
        assert_eq!(timeline, "History.");
    }

    #[test]
    fn split_body_h2_delimiter() {
        let body = "Main.\n\n## Timeline\n\nHistory.";
        let (compiled, timeline) = split_body(body);
        assert_eq!(compiled, "Main.");
        assert_eq!(timeline, "History.");
    }

    #[test]
    fn split_body_first_delimiter_wins() {
        let body = "A.\n\n<!-- timeline -->\n\nB.\n\n--- timeline ---\n\nC.";
        let (compiled, timeline) = split_body(body);
        assert_eq!(compiled, "A.");
        assert_eq!(timeline, "B.\n\n--- timeline ---\n\nC.");
    }

    #[test]
    fn split_body_empty_compiled_truth() {
        let body = "<!-- timeline -->\n\nOnly timeline.";
        let (compiled, timeline) = split_body(body);
        assert_eq!(compiled, "");
        assert_eq!(timeline, "Only timeline.");
    }

    #[test]
    fn split_body_empty_timeline() {
        let body = "Only main.\n\n<!-- timeline -->";
        let (compiled, timeline) = split_body(body);
        assert_eq!(compiled, "Only main.");
        assert_eq!(timeline, "");
    }

    // ── infer_type ─────────────────────────────────────────────────────

    #[test]
    fn infer_type_from_frontmatter() {
        let fm = json!({"type": "project"});
        assert_eq!(infer_type("notes/readme.md", &fm, None), "project");
    }

    #[test]
    fn infer_type_from_frontmatter_trims() {
        let fm = json!({"type": "  person  "});
        assert_eq!(infer_type("x.md", &fm, None), "person");
    }

    #[test]
    fn infer_type_empty_frontmatter_type_falls_through() {
        let fm = json!({"type": ""});
        assert_eq!(infer_type("notes/readme.md", &fm, None), "note");
    }

    #[test]
    fn infer_type_from_builtin_prefix() {
        let fm = json!({});
        assert_eq!(infer_type("people/alice.md", &fm, None), "person");
        assert_eq!(infer_type("companies/acme.md", &fm, None), "company");
        assert_eq!(infer_type("projects/foo.md", &fm, None), "project");
        assert_eq!(infer_type("meetings/standup.md", &fm, None), "meeting");
        assert_eq!(infer_type("decisions/42.md", &fm, None), "decision");
        assert_eq!(infer_type("tasks/todo.md", &fm, None), "task");
        assert_eq!(infer_type("docs/api.md", &fm, None), "doc");
        assert_eq!(infer_type("topics/rust.md", &fm, None), "topic");
    }

    #[test]
    fn infer_type_singular_prefix() {
        let fm = json!({});
        assert_eq!(infer_type("person/bob.md", &fm, None), "person");
        assert_eq!(infer_type("company/acme.md", &fm, None), "company");
        assert_eq!(infer_type("event/conf.md", &fm, None), "event");
    }

    #[test]
    fn infer_type_from_pack_prefixes() {
        let fm = json!({});
        let mut map = HashMap::new();
        map.insert("widgets".to_string(), "widget".to_string());
        assert_eq!(infer_type("widgets/foo.md", &fm, Some(&map)), "widget");
    }

    #[test]
    fn infer_type_builtin_wins_over_pack() {
        let fm = json!({});
        let mut map = HashMap::new();
        map.insert("people".to_string(), "custom_person".to_string());
        // Built-in prefix "people" → "person" takes priority
        assert_eq!(infer_type("people/bob.md", &fm, Some(&map)), "person");
    }

    #[test]
    fn infer_type_default_note() {
        let fm = json!({});
        assert_eq!(infer_type("random/unknown.md", &fm, None), "note");
    }

    #[test]
    fn infer_type_leading_slash() {
        let fm = json!({});
        assert_eq!(infer_type("/people/alice.md", &fm, None), "person");
    }

    #[test]
    fn infer_type_windows_path() {
        let fm = json!({});
        assert_eq!(infer_type("people\\alice.md", &fm, None), "person");
    }

    // ── infer_title ────────────────────────────────────────────────────

    #[test]
    fn infer_title_from_frontmatter() {
        let fm = json!({"title": "My Project"});
        assert_eq!(infer_title(&fm, "", "x.md"), "My Project");
    }

    #[test]
    fn infer_title_from_h1() {
        let fm = json!({});
        let body = "Some intro.\n\n# Actual Title\n\nMore text.";
        assert_eq!(infer_title(&fm, body, "x.md"), "Actual Title");
    }

    #[test]
    fn infer_title_h1_not_h2() {
        let fm = json!({});
        let body = "## Not a title\n\n# Real Title";
        assert_eq!(infer_title(&fm, body, "x.md"), "Real Title");
    }

    #[test]
    fn infer_title_from_filename() {
        let fm = json!({});
        assert_eq!(infer_title(&fm, "", "my-project-notes.md"), "My project notes");
        assert_eq!(infer_title(&fm, "", "hello_world.md"), "Hello world");
    }

    #[test]
    fn infer_title_untitled_fallback() {
        let fm = json!({});
        assert_eq!(infer_title(&fm, "", ""), "Untitled");
        assert_eq!(infer_title(&fm, "", ".md"), "Untitled");
    }

    #[test]
    fn infer_title_empty_frontmatter_falls_through() {
        let fm = json!({"title": ""});
        let body = "# Real Title";
        assert_eq!(infer_title(&fm, body, "x.md"), "Real Title");
    }

    // ── infer_slug ─────────────────────────────────────────────────────

    #[test]
    fn infer_slug_from_frontmatter() {
        let fm = json!({"slug": "my-custom-slug"});
        assert_eq!(infer_slug(&fm, "something/else.md"), "my-custom-slug");
    }

    #[test]
    fn infer_slug_from_filename() {
        let fm = json!({});
        assert_eq!(infer_slug(&fm, "My Cool Note.md"), "my-cool-note");
    }

    #[test]
    fn infer_slug_from_full_path() {
        // When stem is extractable, it's used (not the full path)
        let fm = json!({});
        assert_eq!(infer_slug(&fm, "people/Alice Wang.md"), "alice-wang");
    }

    #[test]
    fn infer_slug_no_extension() {
        let fm = json!({});
        assert_eq!(infer_slug(&fm, "simple-name"), "simple-name");
    }

    #[test]
    fn infer_slug_special_chars() {
        let fm = json!({});
        assert_eq!(infer_slug(&fm, "Hello! World@2024.md"), "hello--world-2024");
    }

    #[test]
    fn infer_slug_no_collapse_hyphens() {
        // TS does NOT collapse consecutive hyphens in slugs
        let fm = json!({});
        assert_eq!(infer_slug(&fm, "a---b___c.md"), "a---b---c");
    }

    // ── infer_tags ─────────────────────────────────────────────────────

    #[test]
    fn infer_tags_from_frontmatter_array() {
        let fm = json!({"tags": ["rust", "systems"]});
        assert_eq!(infer_tags(&fm, "x.md"), vec!["rust", "systems"]);
    }

    #[test]
    fn infer_tags_from_frontmatter_string() {
        let fm = json!({"tags": "rust, systems, async"});
        assert_eq!(infer_tags(&fm, "x.md"), vec!["rust", "systems", "async"]);
    }

    #[test]
    fn infer_tags_from_directory() {
        let fm = json!({});
        assert_eq!(infer_tags(&fm, "people/engineering/alice.md"), vec!["people", "engineering"]);
    }

    #[test]
    fn infer_tags_single_segment_no_tags() {
        let fm = json!({});
        let tags = infer_tags(&fm, "readme.md");
        assert!(tags.is_empty());
    }

    #[test]
    fn infer_tags_empty_frontmatter_tags_falls_through() {
        let fm = json!({"tags": []});
        assert_eq!(infer_tags(&fm, "people/bob.md"), vec!["people"]);
    }

    #[test]
    fn infer_tags_leading_slash() {
        let fm = json!({});
        assert_eq!(infer_tags(&fm, "/people/engineering/alice.md"), vec!["people", "engineering"]);
    }

    // ── parse_markdown integration ─────────────────────────────────────

    #[test]
    fn parse_markdown_full() {
        let body = "---\ntitle: Hello\ntype: project\ntags:\n  - rust\n  - cli\n---\n\n# Hello World\n\nThis is the body.\n\n<!-- timeline -->\n\n2024: Started.\n";
        let result = parse_markdown(body, "projects/my-app.md", None);
        assert_eq!(result.frontmatter["title"], "Hello");
        assert_eq!(result.type_, "project");
        assert_eq!(result.title, "Hello");
        assert_eq!(result.slug, "my-app");
        assert_eq!(result.tags, vec!["rust", "cli"]);
        assert_eq!(result.compiled_truth, "# Hello World\n\nThis is the body.");
        assert_eq!(result.timeline, "2024: Started.");
    }

    #[test]
    fn parse_markdown_no_frontmatter() {
        let body = "# Just a title\n\nContent.";
        let result = parse_markdown(body, "notes/scratch.md", None);
        assert_eq!(result.type_, "note");
        assert_eq!(result.title, "Just a title");
        assert_eq!(result.slug, "scratch");
        assert_eq!(result.compiled_truth, "# Just a title\n\nContent.");
        assert!(result.timeline.is_empty());
        // Directory "notes" becomes a tag
        assert_eq!(result.tags, vec!["notes"]);
    }

    #[test]
    fn parse_markdown_minimal() {
        let body = "Just raw text, no frontmatter.";
        let result = parse_markdown(body, "x.md", None);
        assert_eq!(result.type_, "note");
        assert_eq!(result.title, "X");
        assert_eq!(result.slug, "x");
        assert_eq!(result.compiled_truth, "Just raw text, no frontmatter.");
    }

    #[test]
    fn parse_markdown_with_pack_prefixes() {
        let body = "# Widget Foo";
        let mut map = HashMap::new();
        map.insert("widgets".to_string(), "widget".to_string());
        let result = parse_markdown(body, "widgets/foo.md", Some(&map));
        // Built-in doesn't have "widgets", so pack mapping is used
        assert_eq!(result.type_, "widget");
    }
}
