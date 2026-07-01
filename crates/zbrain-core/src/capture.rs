//! Capture pipeline — binary protection, frontmatter merge, and content hash.
//!
//! Ported from `src/commands/capture.ts`. This module provides the core
//! capture logic as pure functions, decoupled from HTTP/CLI layers.
//!
//! Pipeline:
//!   1. `detect_binary` — scan first 8KB for NUL byte
//!   2. `parse_frontmatter_from_body` — extract YAML frontmatter from markdown body
//!   3. `merge_capture_frontmatter` — merge auto-stamped fields with user frontmatter
//!   4. `capture_content` — main entry: raw bytes → CaptureResult

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ingestion::compute_content_hash;

/// Options for the capture pipeline.
/// Mirrors TS `RunOpts` (capture-relevant subset).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureOpts {
    /// Page type override (CLI --type flag).
    /// Precedence: CLI flag > user frontmatter > 'note'.
    pub page_type: Option<String>,

    /// Source identifier (--source flag or resolved source).
    /// Becomes `captured_via` in frontmatter.
    pub source: Option<String>,

    /// ISO 8601 timestamp for `captured_at`. If None, caller should provide current time.
    /// User can pre-stamp for retroactive captures.
    pub captured_at: Option<String>,
}

/// Result of the capture pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResult {
    /// The body content after frontmatter extraction (if any).
    /// For content that had frontmatter, this is the body only.
    /// For content without frontmatter, this is the full body.
    pub body: String,

    /// Merged frontmatter as JSON. Ready to pass to `PageInput::frontmatter`.
    pub frontmatter: Value,

    /// Normalized content hash (SHA-256 hex).
    pub content_hash: String,

    /// Whether the input had pre-existing frontmatter.
    pub had_frontmatter: bool,
}

/// Error types for the capture pipeline.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CaptureError {
    #[error("binary content detected at byte offset {offset} (NUL byte in first 8KB)")]
    BinaryContent { offset: usize },

    #[error("content is empty")]
    EmptyContent,

    #[error("invalid UTF-8: {0}")]
    InvalidUtf8(String),

    #[error("malformed frontmatter: {0}")]
    MalformedFrontmatter(String),
}

/// Scan the first 8KB of data for a NUL byte (0x00).
///
/// Returns `Some(offset)` if binary content is detected, `None` if clean.
/// Ported from TS `detectBinaryNullByte`.
///
/// Real text files (including UTF-8 with multi-byte CJK, emoji, BOM) never
/// contain a NUL byte — text encoding uses non-zero continuation bytes.
/// NUL appears in binary formats: executables, archives, compressed images,
/// PDFs, most office documents.
///
/// Known limitation: PNG without NUL in first 8KB slips through.
/// Future: magic-byte allowlist (per 1-7-1-4 content extraction).
pub fn detect_binary(data: &[u8]) -> Option<usize> {
    let limit = data.len().min(8 * 1024);
    for (i, &byte) in data[..limit].iter().enumerate() {
        if byte == 0 {
            return Some(i);
        }
    }
    None
}

/// Derive a title from the first non-empty, non-frontmatter-delimiter line.
/// Strips leading markdown heading marks (`#`), capped at 80 chars.
/// Falls back to "Capture" when no usable line exists.
/// Ported from TS `deriveTitle`.
pub fn derive_title(body: &str) -> String {
    let first_line = body
        .lines()
        .find(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty() && trimmed != "---"
        })
        .unwrap_or("");

    let title = first_line
        .trim_start_matches('#')
        .trim();
    let title = if title.is_empty() { "Capture" } else { title };
    title.chars().take(80).collect()
}

/// Parse YAML frontmatter from a markdown body.
///
/// Returns `(frontmatter_json, body_without_frontmatter)`.
/// If no frontmatter block is found, returns `(None, original_body)`.
///
/// Frontmatter detection: body must start with `---\n` or `---\r\n`,
/// tolerating leading BOM. We do NOT use simple `starts_with("---")`
/// because a body opening with a horizontal rule like `--- separator ---`
/// would false-positive.
pub fn parse_frontmatter_from_body(body: &str) -> Result<(Option<Value>, String), CaptureError> {
    // Strip leading BOM
    let body = body.strip_prefix('\u{FEFF}').unwrap_or(body);

    // Check for frontmatter delimiter: must start with `---\n` or `---\r\n`
    let has_fm = body.starts_with("---\n") || body.starts_with("---\r\n");
    if !has_fm {
        return Ok((None, body.to_string()));
    }

    // Find the closing `---`. The TS `gray-matter` library handles this;
    // we replicate the basic behavior: find the second `---` on its own line.
    let lines: Vec<&str> = body.lines().collect();
    // Skip the opening `---`
    if lines.len() < 2 {
        return Ok((None, body.to_string()));
    }

    let mut fm_end: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            fm_end = Some(i);
            break;
        }
    }

    match fm_end {
        None => {
            // No closing delimiter found — treat as no frontmatter
            Ok((None, body.to_string()))
        }
        Some(end_idx) => {
            let fm_yaml = lines[1..end_idx].join("\n");
            let parsed: Value = serde_yaml::from_str(&fm_yaml).map_err(|e| {
                CaptureError::MalformedFrontmatter(e.to_string())
            })?;

            // Ensure parsed result is an object
            if !parsed.is_object() {
                return Err(CaptureError::MalformedFrontmatter(
                    "frontmatter must be a YAML mapping".into(),
                ));
            }

            // Reconstruct body without frontmatter
            let body_after = if end_idx + 1 < lines.len() {
                lines[end_idx + 1..].join("\n")
            } else {
                String::new()
            };

            Ok((Some(parsed), body_after))
        }
    }
}

/// Merge capture's auto-stamped fields with existing frontmatter.
///
/// Ported from TS `mergeCaptureFrontmatter`. Precedence rules (user-wins by default):
///   - `type`:         opts.page_type (CLI flag) > userFm.type > 'note'
///   - `title`:        userFm.title > derived-from-body
///   - `captured_via`: userFm.captured_via > opts.source > 'capture-cli'
///   - `captured_at`:  userFm.captured_at > opts.captured_at > now
///   - Other user-declared keys pass through verbatim.
pub fn merge_capture_frontmatter(
    existing_fm: Option<&Value>,
    body: &str,
    opts: &CaptureOpts,
    now_iso: &str,
) -> Value {
    match existing_fm {
        None => {
            // No existing frontmatter: stamp fresh block
            let title = derive_title(body);
            let mut fm = serde_json::Map::new();
            fm.insert("type".into(), Value::String(
                opts.page_type.clone().unwrap_or_else(|| "note".into()),
            ));
            fm.insert("title".into(), Value::String(title));
            fm.insert(
                "captured_via".into(),
                Value::String(opts.source.clone().unwrap_or_else(|| "capture-cli".into())),
            );
            fm.insert(
                "captured_at".into(),
                Value::String(opts.captured_at.clone().unwrap_or_else(|| now_iso.into())),
            );
            Value::Object(fm)
        }
        Some(user_fm) => {
            // Existing frontmatter: merge with user-wins precedence
            let mut merged = if let Value::Object(map) = user_fm {
                map.clone()
            } else {
                serde_json::Map::new()
            };

            // type: CLI flag > userFm.type > 'note'
            if !merged.contains_key("type") {
                merged.insert("type".into(), Value::String("note".into()));
            }
            if let Some(ref pt) = opts.page_type {
                merged.insert("type".into(), Value::String(pt.clone()));
            }

            // title: userFm.title > derived-from-body
            if !merged.contains_key("title") {
                let title = derive_title(body);
                merged.insert("title".into(), Value::String(title));
            }

            // captured_via: userFm.captured_via > opts.source > 'capture-cli'
            if !merged.contains_key("captured_via") {
                let via = opts.source.clone().unwrap_or_else(|| "capture-cli".into());
                merged.insert("captured_via".into(), Value::String(via));
            }

            // captured_at: userFm.captured_at > opts.captured_at > now
            if !merged.contains_key("captured_at") {
                let at = opts.captured_at.clone().unwrap_or_else(|| now_iso.into());
                merged.insert("captured_at".into(), Value::String(at));
            }

            Value::Object(merged)
        }
    }
}

/// Main capture pipeline entry point.
///
/// Takes raw bytes and capture options, returns a `CaptureResult` ready
/// to be assembled into a `PageInput` for `put_page`.
///
/// Pipeline:
///   1. Binary detection (NUL byte scan)
///   2. UTF-8 decode
///   3. Empty content check
///   4. Parse frontmatter from body
///   5. Merge capture frontmatter
///   6. Compute content hash (on the normalized body + frontmatter)
pub fn capture_content(raw: &[u8], opts: &CaptureOpts) -> Result<CaptureResult, CaptureError> {
    // 1. Binary detection
    if let Some(offset) = detect_binary(raw) {
        return Err(CaptureError::BinaryContent { offset });
    }

    // 2. UTF-8 decode
    let body = String::from_utf8(raw.to_vec())
        .map_err(|e| CaptureError::InvalidUtf8(e.to_string()))?;

    // 3. Empty content check
    if body.trim().is_empty() {
        return Err(CaptureError::EmptyContent);
    }

    // 4. Parse frontmatter
    let (existing_fm, body_without_fm) = parse_frontmatter_from_body(&body)?;
    let had_frontmatter = existing_fm.is_some();

    // 5. Merge frontmatter
    let now_iso = chrono_now_iso();
    let frontmatter = merge_capture_frontmatter(
        existing_fm.as_ref(),
        &body_without_fm,
        opts,
        &now_iso,
    );

    // 6. Compute content hash
    // Hash the body (without frontmatter) for dedup — the frontmatter
    // contains ephemeral fields (captured_at) that should not affect hash.
    let content_hash = compute_content_hash(&body_without_fm);

    Ok(CaptureResult {
        body: body_without_fm,
        frontmatter,
        content_hash,
        had_frontmatter,
    })
}

/// Get current time as ISO 8601 string.
/// Uses chrono if available, otherwise falls back to a simple format.
fn chrono_now_iso() -> String {
    // We avoid a chrono dependency by formatting manually.
    // The TS code uses `new Date().toISOString()`.
    // For now, return a UTC ISO string using std::time.
    use std::time::SystemTime;

    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let nanos = dur.subsec_nanos();

    // Convert Unix timestamp to date-time components
    let days_since_epoch = secs / 86400;
    let secs_of_day = secs % 86400;

    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    let seconds = secs_of_day % 60;
    let millis = nanos / 1_000_000;

    // Compute year/month/day from days since Unix epoch
    // Algorithm: civil_from_days from Howard Hinnant
    let z = days_since_epoch as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, hours, minutes, seconds, millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── detect_binary ──────────────────────────────────────────────────

    #[test]
    fn clean_utf8_text_passes() {
        assert_eq!(detect_binary(b"hello world"), None);
    }

    #[test]
    fn nul_byte_detected() {
        let mut data = vec![b'a'; 100];
        data[50] = 0;
        assert_eq!(detect_binary(&data), Some(50));
    }

    #[test]
    fn nul_at_start() {
        let data = [0u8, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(detect_binary(&data), Some(0));
    }

    #[test]
    fn nul_at_end() {
        let mut data = vec![b'a'; 100];
        data[99] = 0;
        assert_eq!(detect_binary(&data), Some(99));
    }

    #[test]
    fn empty_input_passes() {
        assert_eq!(detect_binary(b""), None);
    }

    #[test]
    fn only_scans_first_8kb() {
        // Create 9KB of clean data, put NUL at byte 9000
        let mut data = vec![b'a'; 9 * 1024];
        data[9000] = 0;
        // NUL is beyond 8KB limit, should NOT be detected
        assert_eq!(detect_binary(&data), None);
    }

    #[test]
    fn utf8_cjk_passes() {
        // Chinese text with multi-byte UTF-8
        let data = "你好世界！这是一段中文文本。".as_bytes();
        assert_eq!(detect_binary(data), None);
    }

    #[test]
    fn utf8_emoji_passes() {
        let data = "Hello 🎉🌍🔥!".as_bytes();
        assert_eq!(detect_binary(data), None);
    }

    // ─── derive_title ───────────────────────────────────────────────────

    #[test]
    fn derive_from_heading() {
        assert_eq!(derive_title("# My Title"), "My Title");
    }

    #[test]
    fn derive_from_plain_text() {
        assert_eq!(derive_title("just some text"), "just some text");
    }

    #[test]
    fn skips_frontmatter_delimiter() {
        assert_eq!(derive_title("---\n# Real Title"), "Real Title");
    }

    #[test]
    fn caps_at_80_chars() {
        let long = "a".repeat(100);
        let title = derive_title(&long);
        assert_eq!(title.len(), 80);
    }

    #[test]
    fn empty_body_defaults_to_capture() {
        assert_eq!(derive_title(""), "Capture");
    }

    #[test]
    fn whitespace_only_defaults_to_capture() {
        assert_eq!(derive_title("   \n  \n"), "Capture");
    }

    // ─── parse_frontmatter_from_body ────────────────────────────────────

    #[test]
    fn no_frontmatter_returns_none() {
        let body = "# Just a heading\n\nSome content.";
        let (fm, remaining) = parse_frontmatter_from_body(body).unwrap();
        assert!(fm.is_none());
        assert_eq!(remaining, body);
    }

    #[test]
    fn parses_simple_frontmatter() {
        let body = "---\ntitle: Hello\ntype: note\n---\n\nBody content.";
        let (fm, remaining) = parse_frontmatter_from_body(body).unwrap();
        let fm = fm.unwrap();
        assert_eq!(fm["title"], "Hello");
        assert_eq!(fm["type"], "note");
        assert_eq!(remaining, "\nBody content.");
    }

    #[test]
    fn frontmatter_with_crlf() {
        let body = "---\r\ntitle: Test\r\n---\r\n\r\nBody.";
        let (fm, remaining) = parse_frontmatter_from_body(body).unwrap();
        let fm = fm.unwrap();
        assert_eq!(fm["title"], "Test");
        // lines() normalizes CRLF to LF when reconstructing
        assert_eq!(remaining, "\nBody.");
    }

    #[test]
    fn frontmatter_with_bom() {
        let body = "\u{FEFF}---\ntitle: BOM Test\n---\n\nContent.";
        let (fm, remaining) = parse_frontmatter_from_body(body).unwrap();
        let fm = fm.unwrap();
        assert_eq!(fm["title"], "BOM Test");
        assert_eq!(remaining, "\nContent.");
    }

    #[test]
    fn horizontal_rule_not_treated_as_frontmatter() {
        // `--- separator ---` should NOT be treated as frontmatter
        let body = "--- separator ---\n\nContent.";
        let (fm, _remaining) = parse_frontmatter_from_body(body).unwrap();
        assert!(fm.is_none());
    }

    #[test]
    fn unclosed_frontmatter_treated_as_no_frontmatter() {
        let body = "---\ntitle: Unclosed\n\nBody content.";
        let (fm, _remaining) = parse_frontmatter_from_body(body).unwrap();
        assert!(fm.is_none());
    }

    #[test]
    fn frontmatter_with_tags_array() {
        let body = "---\ntitle: Tagged\ntags:\n  - rust\n  - capture\n---\n\nBody.";
        let (fm, remaining) = parse_frontmatter_from_body(body).unwrap();
        let fm = fm.unwrap();
        assert_eq!(fm["title"], "Tagged");
        assert_eq!(fm["tags"][0], "rust");
        assert_eq!(fm["tags"][1], "capture");
        assert_eq!(remaining, "\nBody.");
    }

    #[test]
    fn malformed_yaml_returns_error() {
        let body = "---\ninvalid: yaml: : here\n---\n\nBody.";
        let result = parse_frontmatter_from_body(body);
        assert!(result.is_err());
        match result.unwrap_err() {
            CaptureError::MalformedFrontmatter(_) => {}
            e => panic!("expected MalformedFrontmatter, got {:?}", e),
        }
    }

    #[test]
    fn non_object_frontmatter_errors() {
        // A YAML list is not valid frontmatter
        let body = "---\n- item1\n- item2\n---\n\nBody.";
        let result = parse_frontmatter_from_body(body);
        assert!(result.is_err());
    }

    // ─── merge_capture_frontmatter ──────────────────────────────────────

    #[test]
    fn merge_no_existing_frontmatter() {
        let opts = CaptureOpts {
            page_type: Some("article".into()),
            source: Some("cli".into()),
            captured_at: None,
        };
        let fm = merge_capture_frontmatter(None, "# My Title\n\nContent", &opts, "2026-01-01T00:00:00Z");
        assert_eq!(fm["type"], "article");
        assert_eq!(fm["title"], "My Title");
        assert_eq!(fm["captured_via"], "cli");
        assert_eq!(fm["captured_at"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn merge_no_existing_defaults() {
        let opts = CaptureOpts::default();
        let fm = merge_capture_frontmatter(None, "plain text", &opts, "2026-01-01T00:00:00Z");
        assert_eq!(fm["type"], "note");
        assert_eq!(fm["title"], "plain text");
        assert_eq!(fm["captured_via"], "capture-cli");
    }

    #[test]
    fn merge_with_existing_user_wins() {
        let user_fm = serde_json::json!({
            "title": "User Title",
            "type": "journal",
            "tags": ["personal"],
            "description": "my note"
        });
        let opts = CaptureOpts::default();
        let fm = merge_capture_frontmatter(
            Some(&user_fm),
            "body content",
            &opts,
            "2026-01-01T00:00:00Z",
        );
        // User's title wins
        assert_eq!(fm["title"], "User Title");
        // User's type wins (CLI flag not set)
        assert_eq!(fm["type"], "journal");
        // User's custom keys preserved
        assert_eq!(fm["tags"][0], "personal");
        assert_eq!(fm["description"], "my note");
        // Auto-stamped fields added
        assert_eq!(fm["captured_via"], "capture-cli");
    }

    #[test]
    fn merge_cli_type_overrides_user() {
        let user_fm = serde_json::json!({"type": "journal"});
        let opts = CaptureOpts {
            page_type: Some("article".into()),
            ..Default::default()
        };
        let fm = merge_capture_frontmatter(
            Some(&user_fm),
            "body",
            &opts,
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(fm["type"], "article");
    }

    #[test]
    fn merge_user_captured_at_preserved() {
        let user_fm = serde_json::json!({"captured_at": "2025-06-01T12:00:00Z"});
        let opts = CaptureOpts::default();
        let fm = merge_capture_frontmatter(
            Some(&user_fm),
            "body",
            &opts,
            "2026-01-01T00:00:00Z",
        );
        // User's pre-stamped captured_at preserved (retroactive capture)
        assert_eq!(fm["captured_at"], "2025-06-01T12:00:00Z");
    }

    #[test]
    fn merge_user_captured_via_preserved() {
        let user_fm = serde_json::json!({"captured_via": "apple-notes"});
        let opts = CaptureOpts::default();
        let fm = merge_capture_frontmatter(
            Some(&user_fm),
            "body",
            &opts,
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(fm["captured_via"], "apple-notes");
    }

    // ─── capture_content ────────────────────────────────────────────────

    #[test]
    fn capture_simple_text() {
        let opts = CaptureOpts::default();
        let result = capture_content(b"Hello, world!", &opts).unwrap();
        assert!(!result.had_frontmatter);
        assert_eq!(result.body, "Hello, world!");
        assert_eq!(result.frontmatter["type"], "note");
        assert_eq!(result.frontmatter["captured_via"], "capture-cli");
        assert_eq!(result.content_hash.len(), 64);
    }

    #[test]
    fn capture_with_frontmatter() {
        let input = "---\ntitle: Existing\ntype: journal\n---\n\nBody text.";
        let opts = CaptureOpts::default();
        let result = capture_content(input.as_bytes(), &opts).unwrap();
        assert!(result.had_frontmatter);
        assert_eq!(result.body, "\nBody text.");
        assert_eq!(result.frontmatter["title"], "Existing");
        assert_eq!(result.frontmatter["type"], "journal");
    }

    #[test]
    fn capture_rejects_binary() {
        let mut data = vec![b'a'; 100];
        data[50] = 0;
        let opts = CaptureOpts::default();
        let result = capture_content(&data, &opts);
        assert!(matches!(result.unwrap_err(), CaptureError::BinaryContent { .. }));
    }

    #[test]
    fn capture_rejects_empty() {
        let opts = CaptureOpts::default();
        let result = capture_content(b"   \n  ", &opts);
        assert!(matches!(result.unwrap_err(), CaptureError::EmptyContent));
    }

    #[test]
    fn capture_with_bom() {
        let input = "\u{FEFF}# Title\n\nContent.";
        let opts = CaptureOpts::default();
        let result = capture_content(input.as_bytes(), &opts).unwrap();
        assert!(!result.had_frontmatter);
        assert_eq!(result.frontmatter["title"], "Title");
    }

    #[test]
    fn capture_with_cli_type() {
        let opts = CaptureOpts {
            page_type: Some("article".into()),
            ..Default::default()
        };
        let result = capture_content(b"Some content", &opts).unwrap();
        assert_eq!(result.frontmatter["type"], "article");
    }

    #[test]
    fn capture_content_hash_deterministic() {
        let opts = CaptureOpts::default();
        let r1 = capture_content(b"same content", &opts).unwrap();
        let r2 = capture_content(b"same content", &opts).unwrap();
        assert_eq!(r1.content_hash, r2.content_hash);
    }

    #[test]
    fn capture_content_hash_differs() {
        let opts = CaptureOpts::default();
        let r1 = capture_content(b"content A", &opts).unwrap();
        let r2 = capture_content(b"content B", &opts).unwrap();
        assert_ne!(r1.content_hash, r2.content_hash);
    }

    #[test]
    fn capture_with_source() {
        let opts = CaptureOpts {
            source: Some("my-source".into()),
            ..Default::default()
        };
        let result = capture_content(b"Content", &opts).unwrap();
        assert_eq!(result.frontmatter["captured_via"], "my-source");
    }
}
