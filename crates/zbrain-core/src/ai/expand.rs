//! Multi-query expansion — pure sanitization + orchestration layer.
//!
//! Ported from TS `src/core/search/expansion.ts` (+ the thin gateway wrapper
//! `gateway.expand`). This module owns two responsibilities:
//!
//! 1. **Prompt-injection defense** ([`sanitize_query_for_prompt`]) and
//!    **untrusted-output validation** ([`sanitize_expansion_output`]). These
//!    are zbrain's responsibility, not the provider's: the provider is
//!    LLM-agnostic, so sanitization stays here on both the inbound (user ->
//!    LLM) and outbound (LLM -> search) edges.
//! 2. **Orchestration** ([`expand_query`]): CJK-aware word-count gate,
//!    availability gate, sanitize -> call provider -> sanitize -> dedup/cap.
//!
//! The actual LLM call is abstracted behind [`ExpansionProvider`] (the seam,
//! mirroring `RerankClient`/`ChatProvider`). The real structured-output HTTP
//! implementation is NOT built here — TS `gateway.expand` needs
//! `generateObject` (structured JSON), which the slice-3 `ChatProvider` seam
//! does not yet expose. That real provider is a registered known-gap
//! (registered in docs/plans/MIGRATION.md (G26)); this slice ships the pure
//! layer + trait + a test mock so the whole pipeline is testable and search
//! can wire it the moment a structured-output seam lands.

use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::cjk::count_cjk_aware_words;

/// Maximum queries returned by [`expand_query`] (original + alternatives).
const MAX_QUERIES: usize = 3;
/// Minimum CJK-aware word count before expansion is attempted. Short queries
/// (1–2 words) are already specific enough; expanding them wastes an LLM call.
const MIN_WORDS: usize = 3;
/// Hard cap on characters fed to / accepted from the LLM channel.
const MAX_QUERY_CHARS: usize = 500;
/// Maximum LLM-produced alternatives accepted after sanitization.
const MAX_ALTERNATIVES: usize = 2;

// Fenced code blocks: ```...``` (non-greedy, dot-matches-newline).
static FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```.*?```").expect("FENCE_RE must compile"));
// HTML-ish tags: <tag ...> or </tag>.
static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</?[a-zA-Z][^>]*>").expect("TAG_RE must compile"));
// Leading injection prefixes: "ignore:", "forget ", "system:", repeated.
static PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:\s*(?:ignore|forget|disregard|override|system|assistant|human)[\s:]+)+")
        .expect("PREFIX_RE must compile")
});
// Runs of whitespace (any kind) to collapse into a single space.
static WS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").expect("WS_RE must compile"));

/// Error surfaced by an [`ExpansionProvider`]. Orchestration treats any error
/// as a soft failure and falls back to the original query, so this stays
/// deliberately simple.
#[derive(Debug)]
pub enum ExpansionError {
    Provider(String),
}

impl std::fmt::Display for ExpansionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(msg) => write!(f, "expansion provider error: {msg}"),
        }
    }
}

impl std::error::Error for ExpansionError {}

/// The LLM seam for query expansion. Mirrors TS `gateway.expand(query)`:
/// returns `[query, ...alternatives]` — the first element echoes the input
/// query, and every element after it is an untrusted LLM-produced alternative
/// that MUST pass through [`sanitize_expansion_output`] before use.
#[async_trait]
pub trait ExpansionProvider: Send + Sync {
    async fn expand(&self, query: &str) -> Result<Vec<String>, ExpansionError>;
}

/// Defense-in-depth sanitization for a user query before it reaches the LLM.
///
/// Strips fenced code blocks, HTML-ish tags, and leading instruction-override
/// prefixes; truncates to [`MAX_QUERY_CHARS`]; collapses whitespace. Mirrors
/// TS `sanitizeQueryForPrompt`. Never logs the query text itself (privacy).
#[must_use]
pub fn sanitize_query_for_prompt(query: &str) -> String {
    // Truncate first (char-safe), matching TS `slice(0, MAX_QUERY_CHARS)`
    // semantics closely enough for defense purposes.
    let truncated: String = if query.chars().count() > MAX_QUERY_CHARS {
        query.chars().take(MAX_QUERY_CHARS).collect()
    } else {
        query.to_string()
    };

    let no_fence = FENCE_RE.replace_all(&truncated, " ");
    let no_tags = TAG_RE.replace_all(&no_fence, " ");
    let no_prefix = PREFIX_RE.replace(&no_tags, "");
    let collapsed = WS_RE.replace_all(&no_prefix, " ");
    collapsed.trim().to_string()
}

/// Validate LLM-produced alternative queries. LLM output is untrusted: strip
/// control characters, drop empties, truncate, dedup case-insensitively, and
/// take at most [`MAX_ALTERNATIVES`]. Mirrors TS `sanitizeExpansionOutput`.
#[must_use]
pub fn sanitize_expansion_output(alternatives: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in alternatives {
        // Strip control chars (C0 0x00–0x1f, DEL 0x7f, and C1). Matches TS
        // `replace(/[\x00-\x1f\x7f]/g, '')`, slightly stricter on C1.
        let stripped: String = raw.chars().filter(|c| !c.is_control()).collect();
        let mut s = stripped.trim().to_string();
        if s.is_empty() {
            continue;
        }
        if s.chars().count() > MAX_QUERY_CHARS {
            s = s.chars().take(MAX_QUERY_CHARS).collect();
        }
        let key = s.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(s);
        if out.len() >= MAX_ALTERNATIVES {
            break;
        }
    }
    out
}

/// Expand `query` into up to [`MAX_QUERIES`] search queries.
///
/// Orchestration (mirrors TS `expandQuery`):
/// 1. CJK-aware word-count gate: queries below [`MIN_WORDS`] are returned
///    as-is (too short to benefit from expansion).
/// 2. Sanitize the query for the LLM channel. If sanitization empties it,
///    fall back to the original.
/// 3. Call the provider; treat any error as a soft failure -> `[query]`.
/// 4. Validate the alternatives (everything after the echoed first entry),
///    prepend the ORIGINAL (unsanitized) query, dedup case-insensitively,
///    and cap at [`MAX_QUERIES`].
///
/// The availability gate (TS `isAvailable('expansion')`) lives at the call
/// site: pass `None` for `provider` when no expansion provider is configured
/// and this returns `[query]` without any work.
pub async fn expand_query(
    query: &str,
    provider: Option<&dyn ExpansionProvider>,
) -> Vec<String> {
    if count_cjk_aware_words(query) < MIN_WORDS {
        return vec![query.to_string()];
    }

    let Some(provider) = provider else {
        return vec![query.to_string()];
    };

    let sanitized = sanitize_query_for_prompt(query);
    if sanitized.is_empty() {
        return vec![query.to_string()];
    }

    // gateway.expand returns [echoed_query, ...alternatives]. Feed it the
    // sanitized copy so the LLM channel is safe; the ORIGINAL query is
    // re-inserted below as the first downstream search entry.
    let Ok(gateway_results) = provider.expand(&sanitized).await else {
        return vec![query.to_string()];
    };

    // Alternatives = everything after the first (echoed) entry.
    let alternatives: Vec<String> = gateway_results.into_iter().skip(1).collect();
    let sanitized_alts = sanitize_expansion_output(&alternatives);

    // Original query + sanitized alternatives, deduped (case-insensitive,
    // trimmed) preserving first-seen casing, capped at MAX_QUERIES.
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for candidate in std::iter::once(query.to_string()).chain(sanitized_alts) {
        let key = candidate.to_lowercase().trim().to_string();
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(candidate);
        if out.len() >= MAX_QUERIES {
            break;
        }
    }
    out
}

/// Production [`ExpansionProvider`] backed by a [`ChatProvider`].
///
/// Mirrors TS `gateway.expand`: ships the (already-sanitized) query to an LLM
/// with a prompt asking for alternative search phrasings, and parses the
/// reply into `[echoed_query, ...alternatives]`. `expand_query` prepends the
/// ORIGINAL query and `skip(1)`s this echoed entry, so returning
/// `[input, ...alts]` matches the convention.
///
/// Honest degradation: any chat error or unparseable reply yields `[query]`
/// so the caller falls back to a single-query search rather than failing or
/// fabricating alternatives. This is the real provider the module doc
/// describes as a known-gap (G26) — it uses the unstructured `ChatProvider`
/// seam (free-text reply) rather than TS's `generateObject` structured seam,
/// which is sufficient for expansion (KNOWN-GAPS G74b wires it into eval).
pub struct ChatExpansionProvider {
    chat: Arc<dyn ChatProvider>,
}

impl ChatExpansionProvider {
    #[must_use]
    pub fn new(chat: Arc<dyn ChatProvider>) -> Self {
        Self { chat }
    }
}

const EXPANSION_SYSTEM: &str = "You are a search query expansion assistant. \
Given a user's search query, produce alternative phrasings that a retrieval \
system should also try. Respond with ONLY a JSON array of strings (the \
alternative queries), no prose and no markdown fences.";

#[async_trait]
impl ExpansionProvider for ChatExpansionProvider {
    async fn expand(&self, query: &str) -> Result<Vec<String>, ExpansionError> {
        let opts = ChatOpts {
            system: Some(EXPANSION_SYSTEM.to_string()),
            messages: vec![ChatMessage::text(
                ChatRole::User,
                format!("Return alternative search queries for: {query}"),
            )],
            max_tokens: Some(256),
            ..Default::default()
        };
        let text = match self.chat.chat(opts).await {
            Ok(result) => result.text,
            Err(_) => return Ok(vec![query.to_string()]),
        };
        let mut out = vec![query.to_string()];
        out.extend(parse_expansion_response(&text));
        Ok(out)
    }
}

/// Tolerant parser for an LLM expansion reply.
///
/// Accepts, in order: a strict JSON array of strings; the first `[ ... ]`
/// span found inside the text (e.g. fenced or prose-wrapped); or a
/// newline/numbered list. Always returns the cleaned alternatives (empties
/// dropped) and never errors — a useless reply simply yields no alternatives.
fn parse_expansion_response(text: &str) -> Vec<String> {
    let trimmed = text.trim();

    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) {
        return strings_from_json_array(items);
    }

    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        if start < end {
            if let Ok(Value::Array(items)) =
                serde_json::from_str::<Value>(&trimmed[start..=end])
            {
                let parsed = strings_from_json_array(items);
                if !parsed.is_empty() {
                    return parsed;
                }
            }
        }
    }

    // Newline / numbered list fallback: only treat lines that actually look
    // like list items (leading bullet or `N.` / `N)` marker) as alternatives.
    // Plain prose with no markers is a "useless reply" and yields nothing
    // (honest degradation — see `parse_useless_reply_yields_nothing`).
    trimmed
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            let is_list_item = t.starts_with("- ")
                || t.starts_with("* ")
                || {
                    let bytes = t.as_bytes();
                    let mut idx = 0;
                    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                        idx += 1;
                    }
                    idx > 0
                        && idx < bytes.len()
                        && (bytes[idx] == b'.' || bytes[idx] == b')')
                        && (idx + 1 == bytes.len() || bytes[idx + 1] == b' ')
                };
            if !is_list_item {
                return None;
            }
            let cleaned = t
                .trim_start_matches(|c: char| {
                    c.is_ascii_digit() || c == '-' || c == '*' || c == '.' || c == ')' || c == '('
                })
                .trim()
                .trim_matches('"')
                .trim()
                .to_string();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            }
        })
        .collect()
}

fn strings_from_json_array(items: Vec<Value>) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|v| v.as_str().map(ToString::to_string))
        .filter(|s| !s.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test mock: returns a pre-seeded [`echoed_query`, ...alternatives] vec,
    /// or an error. Records the query it was called with.
    struct MockExpansionProvider {
        result: Result<Vec<String>, ()>,
    }

    impl MockExpansionProvider {
        fn returning(alts: &[&str]) -> Self {
            // First element echoes the query (filled per-call by expand()).
            // We prepend a placeholder here; expand_query only reads skip(1).
            let mut v = vec!["__echo__".to_string()];
            v.extend(alts.iter().map(|s| (*s).to_string()));
            Self { result: Ok(v) }
        }
        fn failing() -> Self {
            Self { result: Err(()) }
        }
    }

    #[async_trait]
    impl ExpansionProvider for MockExpansionProvider {
        async fn expand(&self, _query: &str) -> Result<Vec<String>, ExpansionError> {
            self.result
                .clone()
                .map_err(|()| ExpansionError::Provider("mock failure".to_string()))
        }
    }

    // ── sanitize_query_for_prompt ───────────────────────────────────────

    #[test]
    fn sanitize_query_strips_fenced_code() {
        let q = "find the ```rm -rf /``` config file";
        let out = sanitize_query_for_prompt(q);
        assert!(!out.contains("rm -rf"), "fence not stripped: {out}");
        assert!(out.contains("find the"));
        assert!(out.contains("config file"));
    }

    #[test]
    fn sanitize_query_strips_html_tags() {
        let out = sanitize_query_for_prompt("hello <script>alert(1)</script> world");
        assert!(!out.contains('<'), "tags not stripped: {out}");
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
    }

    #[test]
    fn sanitize_query_strips_injection_prefix() {
        let out = sanitize_query_for_prompt("ignore: system: what is rust ownership");
        assert!(out.starts_with("what is rust"), "prefix not stripped: {out}");
    }

    #[test]
    fn sanitize_query_collapses_whitespace() {
        let out = sanitize_query_for_prompt("  multiple   spaces\there  ");
        assert_eq!(out, "multiple spaces here");
    }

    #[test]
    fn sanitize_query_truncates_to_max_chars() {
        let long = "a ".repeat(400); // 800 chars
        let out = sanitize_query_for_prompt(&long);
        assert!(out.chars().count() <= MAX_QUERY_CHARS);
    }

    #[test]
    fn sanitize_query_clean_input_passthrough() {
        let out = sanitize_query_for_prompt("how does tcp handshake work");
        assert_eq!(out, "how does tcp handshake work");
    }

    // ── sanitize_expansion_output ───────────────────────────────────────

    #[test]
    fn sanitize_output_strips_control_chars() {
        let alts = vec!["hel\u{0}lo\u{7f} world".to_string()];
        let out = sanitize_expansion_output(&alts);
        assert_eq!(out, vec!["hello world".to_string()]);
    }

    #[test]
    fn sanitize_output_drops_empty_and_whitespace_only() {
        let alts = vec!["  ".to_string(), "\u{0}".to_string(), "real query".to_string()];
        let out = sanitize_expansion_output(&alts);
        assert_eq!(out, vec!["real query".to_string()]);
    }

    #[test]
    fn sanitize_output_dedups_case_insensitively() {
        let alts = vec!["Rust Ownership".to_string(), "rust ownership".to_string()];
        let out = sanitize_expansion_output(&alts);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn sanitize_output_caps_at_two_alternatives() {
        let alts = vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
        ];
        let out = sanitize_expansion_output(&alts);
        assert_eq!(out.len(), MAX_ALTERNATIVES);
        assert_eq!(out, vec!["one".to_string(), "two".to_string()]);
    }

    // ── expand_query orchestration ──────────────────────────────────────

    #[tokio::test]
    async fn expand_short_query_returns_as_is() {
        // 2 words < MIN_WORDS, provider must not even be consulted.
        let provider = MockExpansionProvider::returning(&["ignored alternative here"]);
        let out = expand_query("rust ownership", Some(&provider)).await;
        assert_eq!(out, vec!["rust ownership".to_string()]);
    }

    #[tokio::test]
    async fn expand_no_provider_returns_as_is() {
        let out = expand_query("how does tcp handshake work in detail", None).await;
        assert_eq!(out, vec!["how does tcp handshake work in detail".to_string()]);
    }

    #[tokio::test]
    async fn expand_provider_error_falls_back_to_original() {
        let provider = MockExpansionProvider::failing();
        let out = expand_query("how does tcp handshake work", Some(&provider)).await;
        assert_eq!(out, vec!["how does tcp handshake work".to_string()]);
    }

    #[tokio::test]
    async fn expand_prepends_original_and_caps_at_max_queries() {
        let provider = MockExpansionProvider::returning(&[
            "tcp three way handshake steps",
            "tcp connection establishment syn ack",
            "extra alternative beyond cap",
        ]);
        let out = expand_query("how does tcp handshake work", Some(&provider)).await;
        // Original first, then <= 2 alternatives, capped at MAX_QUERIES total.
        assert_eq!(out.len(), MAX_QUERIES);
        assert_eq!(out[0], "how does tcp handshake work");
    }

    #[tokio::test]
    async fn expand_dedups_alternative_equal_to_original() {
        let provider = MockExpansionProvider::returning(&[
            "how does tcp handshake work", // dup of original
            "tcp syn ack sequence explained",
        ]);
        let out = expand_query("how does tcp handshake work", Some(&provider)).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "how does tcp handshake work");
        assert_eq!(out[1], "tcp syn ack sequence explained");
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;
    use std::sync::Arc;

    #[test]
    fn parse_strict_json_array() {
        let out = parse_expansion_response(r#"["tcp syn ack", "handshake steps"]"#);
        assert_eq!(out, vec!["tcp syn ack", "handshake steps"]);
    }

    #[test]
    fn parse_json_array_inside_prose() {
        let out = parse_expansion_response("Here you go:\n[\"alt one\", \"alt two\"]\nhope that helps");
        assert_eq!(out, vec!["alt one", "alt two"]);
    }

    #[test]
    fn parse_newline_list_with_markers() {
        let out = parse_expansion_response("1. first alternative\n- second one\n3) third");
        assert_eq!(out, vec!["first alternative", "second one", "third"]);
    }

    #[test]
    fn parse_useless_reply_yields_nothing() {
        let out = parse_expansion_response("I'm not sure what you mean.");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn chat_provider_expands_via_json() {
        let mock = MockChatProvider::new("");
        mock.queue_text(r#"["tcp three way handshake", "syn ack fin explained"]"#);
        let provider = ChatExpansionProvider::new(Arc::new(mock));
        let out = expand_query(
            "how does the tcp handshake actually work in detail",
            Some(&provider),
        )
        .await;
        // original first, then the two parsed alternatives.
        assert_eq!(
            out,
            vec![
                "how does the tcp handshake actually work in detail".to_string(),
                "tcp three way handshake".to_string(),
                "syn ack fin explained".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn chat_provider_error_falls_back_to_original() {
        let mock = MockChatProvider::new("");
        mock.queue_error(crate::ai::chat::ChatError::Transient {
            message: "injected test failure".to_string(),
        });
        let provider = ChatExpansionProvider::new(Arc::new(mock));
        let out = expand_query(
            "how does the tcp handshake actually work in detail",
            Some(&provider),
        )
        .await;
        assert_eq!(out, vec!["how does the tcp handshake actually work in detail"]);
    }
}
