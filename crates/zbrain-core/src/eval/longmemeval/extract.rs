//! LongMemEval inline claim extractor. Port of TS
//! `src/eval/longmemeval/extract.ts` (v0.40.2.0).
//!
//! Populates the benchmark brain's `facts` table so trajectory routing has
//! data to retrieve. The benchmark contract change is disclosed in the JSON
//! envelope's `methodology_note` field: this is full-haystack preprocessing,
//! NOT a zbrain-retrieval-only result.
//!
//! Per-session flow (identical to TS):
//!   1. Hash the session body (sha256). Cache hit → reuse parsed claims.
//!   2. Cache miss → one extractor chat call. Output is a JSON array of
//!      claim/event records.
//!   3. [`parse_extracted_json_array`] repairs the output (fence strip →
//!      direct parse → first `[...]` substring). Never panics — a
//!      non-array response degrades to zero claims for that session.
//!   4. Canonicalize each `entity` via the per-question alias map +
//!      [`resolve_entity_slug_with_source`] (real-page-aware).
//!      First-mention-wins lowercase canonicalization keeps "Marco" /
//!      "Marco Smith" / "marco" collapsed to one slug.
//!   5. Insert via [`BrainEngine::insert_fact`] (no embedding — the
//!      benchmark doesn't need drift_score).
//!
//! ## Deliberate deviations from TS
//!
//! - **Cache ownership.** TS keeps the sha256 cache + hit counters in
//!   module scope and exposes `resetExtractorState()`. Rust runs its test
//!   suite multi-threaded inside one process, so a process-global cache
//!   would leak between concurrent tests. The cache is therefore an
//!   explicit [`ExtractorCache`] value the harness owns for the duration of
//!   one benchmark run — behaviourally identical to TS's
//!   "reset once per run" contract, with [`ExtractorCache::reset`] kept for
//!   parity. Interior mutability (`Mutex`) preserves TS's shared-across-
//!   concurrent-sessions semantics.
//! - **Bulk insert.** TS issues one `engine.insertFacts(rows)` batch. The
//!   Rust engine trait has no bulk variant, so rows go in via a
//!   `insert_fact` loop. Two consequences, both invisible to the benchmark:
//!   (a) `insert_fact` marks supersede on a prior active same-entity
//!   same-kind row — `find_trajectory` filters on `expired_at IS NULL` only
//!   (never `superseded_by`), so every point still charts; (b) an exact
//!   duplicate row (same entity+kind+text, still active) is reported
//!   `Duplicate` instead of inserted, so `inserted` can trail `parsed`.
//! - **`valid_from` shape.** TS builds a `Date` for the PG `timestamptz`
//!   column. The Rust facts layer stores ISO-ish strings everywhere
//!   (`think::trajectory`, `facts::decay`, …) and `find_trajectory` orders
//!   them lexically, which is equivalent for the `YYYY-MM-DD` prefix the
//!   validator enforces. The validated string is passed through verbatim.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::autopilot::phases::resolve::{
    resolve_entity_slug_with_source, ResolutionSource, ResolveResult,
};
use crate::engine::BrainEngine;
use crate::types::{FactInsertStatus, FactKind, FactVisibility, NewFact};

/// Wire shape for the extractor's per-session output. Mirrors TS
/// `ExtractedClaim`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedClaim {
    pub entity: String,
    pub metric: Option<String>,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub period: Option<String>,
    pub event_type: Option<String>,
    /// `YYYY-MM-DD` or a longer ISO string sharing that prefix.
    pub valid_from: String,
    pub text: String,
}

/// Per-question alias map. Persists across sessions within ONE question and
/// is dropped before the next question (aliases never leak across
/// questions). Mirrors TS `AliasMap` + `makeAliasMap`.
///
/// Semantics pinned by TS: "Marco" in session 1 and "Marco Smith" in
/// session 3 of the SAME question collapse to one slug.
#[derive(Debug, Default)]
pub struct AliasMap {
    inner: Mutex<HashMap<String, String>>,
}

impl AliasMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, key: &str) -> Option<String> {
        self.inner.lock().expect("poisoned").get(key).cloned()
    }

    fn set(&self, key: &str, slug: &str) {
        self.inner
            .lock()
            .expect("poisoned")
            .insert(key.to_string(), slug.to_string());
    }

    fn contains(&self, key: &str) -> bool {
        self.inner.lock().expect("poisoned").contains_key(key)
    }

    /// Number of alias entries. Test/telemetry helper.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Cache hit/miss telemetry. Mirrors TS `CacheStats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub size: usize,
}

#[derive(Debug, Default)]
struct CacheInner {
    /// sha256(body) → claims parsed for that body.
    entries: HashMap<String, Vec<ExtractedClaim>>,
    hits: u64,
    misses: u64,
}

/// sha256-keyed claim cache scoped to one benchmark run. Mirrors the TS
/// module-scope `cache` + `cacheHits` / `cacheMisses` counters.
#[derive(Debug, Default)]
pub struct ExtractorCache {
    inner: Mutex<CacheInner>,
}

impl ExtractorCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears entries + counters. Mirrors TS `resetExtractorState()`.
    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("poisoned");
        inner.entries.clear();
        inner.hits = 0;
        inner.misses = 0;
    }

    /// Mirrors TS `getCacheStats()`.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().expect("poisoned");
        CacheStats { hits: inner.hits, misses: inner.misses, size: inner.entries.len() }
    }

    /// Look up by body hash, bumping the hit counter on success and the miss
    /// counter on failure. The lock is released before the caller awaits the
    /// extractor call.
    fn take(&self, hash: &str) -> Option<Vec<ExtractedClaim>> {
        let mut inner = self.inner.lock().expect("poisoned");
        match inner.entries.get(hash).cloned() {
            Some(claims) => {
                inner.hits += 1;
                Some(claims)
            }
            None => {
                inner.misses += 1;
                None
            }
        }
    }

    fn store(&self, hash: &str, claims: &[ExtractedClaim]) {
        self.inner
            .lock()
            .expect("poisoned")
            .entries
            .insert(hash.to_string(), claims.to_vec());
    }
}

/// System prompt for the extractor call. Byte-identical to TS
/// `EXTRACTOR_SYSTEM_PROMPT`.
pub const EXTRACTOR_SYSTEM_PROMPT: &str = r#"You extract typed claims and events from a single chat-session transcript.

Output a JSON array of records. Each record has these fields:
  - entity:     The thing the claim is ABOUT (person name, company, place, object).
                Use the most specific name mentioned. Lowercase.
  - metric:     Canonical metric label (lowercase snake_case) like "mrr", "arr",
                "team_size", "role". Null when the row is an event rather than
                a typed numeric claim.
  - value:      The numeric value of the claim. Use a number, not a string.
                Null for non-numeric or event rows.
  - unit:       Currency or unit like "USD", "%", "count". Null when not present.
  - period:     Periodicity like "monthly", "annual", "once". Null when not present.
  - event_type: Event label like "meeting", "purchase", "trip", "job_change",
                "location_change". Null when the row is a numeric claim.
  - valid_from: The date the claim or event was true (YYYY-MM-DD). Use the
                session date if the transcript doesn't anchor a specific date.
  - text:       Short paraphrase of the underlying claim or event (one sentence,
                max 200 chars).

A row should have EITHER metric+value (numeric claim) OR event_type (event).
Not both. Skip filler conversation, opinions without dates, and questions —
extract only assertions of typed-claim or event shape.

If nothing in the transcript looks extractable, return [].

Output ONLY the JSON array. No prose, no markdown fences."#;

/// Max output tokens for one extractor call. Mirrors TS `max_tokens: 2000`.
const EXTRACTOR_MAX_TOKENS: u32 = 2000;

/// ```` ```json … ``` ```` fence. `(?is)` mirrors the TS `i` flag plus the
/// `[\s\S]` idiom JS needs for a dot-matches-newline class.
static JSON_FENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)```(?:json)?\s*\n?(.*?)```").expect("json-fence regex must compile")
});

/// Greedy first-`[` to last-`]` span. Mirrors TS `/\[[\s\S]*\]/`.
static JSON_ARRAY_SPAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\[.*\]").expect("json-array regex must compile"));

/// `YYYY-MM-DD` prefix guard on `valid_from`. Mirrors TS
/// `/^\d{4}-\d{2}-\d{2}/`.
static DATE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}").expect("date-prefix regex must compile"));

/// Upper bound on the stored `fact` text. Mirrors TS `r.text.slice(0, 500)`
/// (counted in `char`s so a multi-byte paraphrase can't split a code point).
const MAX_CLAIM_TEXT_CHARS: usize = 500;

/// SHA-256 of the raw session body — the cache-hit decision depends ONLY on
/// what would actually be sent to the extractor. Mirrors TS
/// `hashSessionBody`.
fn hash_session_body(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Parse a JSON array out of model output. Mirrors TS
/// `parseExtractedJsonArray`:
///   1. Strip ```` ```json … ``` ```` fences if present, then parse.
///   2. Otherwise parse the first `[…]` substring.
/// Anything else yields an empty vector — the caller treats that as
/// "fail open, 0 facts for this session".
#[must_use]
pub fn parse_extracted_json_array(raw: &str) -> Vec<serde_json::Value> {
    if raw.trim().is_empty() {
        return Vec::new();
    }

    let cleaned = match JSON_FENCE.captures(raw) {
        Some(caps) => caps.get(1).map_or("", |m| m.as_str()).trim(),
        None => raw.trim(),
    };

    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(cleaned)
    {
        return items;
    }

    if let Some(m) = JSON_ARRAY_SPAN.find(cleaned) {
        if let Ok(serde_json::Value::Array(items)) =
            serde_json::from_str::<serde_json::Value>(m.as_str())
        {
            return items;
        }
    }

    Vec::new()
}

/// Validate one record from the extractor output. Malformed records are
/// dropped (`None`) so a bad row can't poison the batch. Mirrors TS
/// `validateClaim`.
#[must_use]
pub fn validate_claim(raw: &serde_json::Value) -> Option<ExtractedClaim> {
    let obj = raw.as_object()?;

    let entity = obj.get("entity")?.as_str()?;
    if entity.trim().is_empty() {
        return None;
    }
    let text = obj.get("text")?.as_str()?;
    let valid_from = obj.get("valid_from")?.as_str()?;

    if !DATE_PREFIX.is_match(valid_from) {
        return None;
    }

    // Exactly one of metric / event_type is expected (xor). Defensive: both
    // null is accepted — a no-op row rather than a crash. Matches TS.
    let str_field = |key: &str| obj.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let value = obj
        .get("value")
        .and_then(serde_json::Value::as_f64)
        .filter(|v| v.is_finite());

    Some(ExtractedClaim {
        entity: entity.to_string(),
        metric: str_field("metric"),
        value,
        unit: str_field("unit"),
        period: str_field("period"),
        event_type: str_field("event_type"),
        valid_from: valid_from.to_string(),
        text: text.chars().take(MAX_CLAIM_TEXT_CHARS).collect(),
    })
}

/// Canonicalize an entity string via the per-question alias map +
/// [`resolve_entity_slug_with_source`]. First-mention-wins: the first
/// canonical slug resolved for a normalized key sticks. Mirrors TS
/// `canonicalizeEntity`.
///
/// Normalization: lowercase + trim. Multi-token names also register under
/// their first token, so a later bare "Marco" hits the same slug as an
/// earlier "Marco Smith" (and vice versa).
async fn canonicalize_entity(
    engine: &dyn BrainEngine,
    source_id: &str,
    raw_entity: &str,
    alias_map: &AliasMap,
) -> Option<ResolveResult> {
    let normalized = raw_entity.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }

    // Direct alias hit (full normalized form).
    if let Some(slug) = alias_map.get(&normalized) {
        return Some(ResolveResult { slug, source: ResolutionSource::FuzzyMatch });
    }

    // Multi-word: check the first-token alias too.
    let first_token = normalized.split_whitespace().next().unwrap_or("").to_string();
    if first_token != normalized {
        if let Some(slug) = alias_map.get(&first_token) {
            alias_map.set(&normalized, &slug);
            return Some(ResolveResult { slug, source: ResolutionSource::FuzzyMatch });
        }
    }

    // No alias hit — resolve via engine. Real-page hits beat slugify.
    let resolved = resolve_entity_slug_with_source(engine, source_id, raw_entity).await?;

    // Cache the canonical slug under BOTH the full normalized form and the
    // first token so future short-form mentions hit.
    alias_map.set(&normalized, &resolved.slug);
    if first_token != normalized && !alias_map.contains(&first_token) {
        alias_map.set(&first_token, &resolved.slug);
    }
    Some(resolved)
}

/// Run the extractor on one session body. Returns parsed claims, or `None`
/// on a provider error — the caller treats `None` as "extract nothing for
/// this session" so a blip can't stall benchmark progress. Mirrors TS
/// `callExtractor`.
async fn call_extractor(
    chat: &dyn ChatProvider,
    body: &str,
    model: &str,
) -> Option<Vec<ExtractedClaim>> {
    let opts = ChatOpts {
        model: Some(model.to_string()),
        system: Some(EXTRACTOR_SYSTEM_PROMPT.to_string()),
        messages: vec![ChatMessage::text(ChatRole::User, body)],
        tools: Vec::new(),
        max_tokens: Some(EXTRACTOR_MAX_TOKENS),
        cache_system: false,
    };

    // `ChatResult::text` is already the concatenation of every text block,
    // matching TS's `response.content.find(b => b.type === 'text')`.
    let text = chat.chat(opts).await.ok()?.text;
    if text.is_empty() {
        return Some(Vec::new());
    }

    Some(
        parse_extracted_json_array(&text)
            .iter()
            .filter_map(validate_claim)
            .collect(),
    )
}

/// Per-session extraction telemetry. Mirrors TS `ExtractResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExtractResult {
    /// Number of facts inserted into the benchmark brain.
    pub inserted: usize,
    /// Claims parsed from the extractor response (pre-canonicalization).
    pub parsed: usize,
    /// Whether this session's claims came from cache (hit) or the model.
    pub cache_hit: bool,
}

/// Inputs for [`extract_and_insert_claims`]. Mirrors the TS options object.
pub struct ExtractOpts<'a> {
    pub engine: &'a dyn BrainEngine,
    pub chat: &'a dyn ChatProvider,
    pub model: &'a str,
    pub session_slug: &'a str,
    pub session_id: &'a str,
    pub session_body: &'a str,
    pub source_id: &'a str,
    pub alias_map: &'a AliasMap,
    pub cache: &'a ExtractorCache,
}

/// Extract claims from one session body, canonicalize entities via the
/// per-question alias map + engine resolver, and insert them into the facts
/// table. Mirrors TS `extractAndInsertClaims`.
///
/// Never returns `Err`: internal failures degrade to "0 facts inserted" and
/// the caller moves on to the next session.
pub async fn extract_and_insert_claims(opts: ExtractOpts<'_>) -> ExtractResult {
    let hash = hash_session_body(opts.session_body);

    let (claims, cache_hit) = match opts.cache.take(&hash) {
        Some(cached) => (Some(cached), true),
        None => {
            let fresh = call_extractor(opts.chat, opts.session_body, opts.model).await;
            if let Some(ref claims) = fresh {
                opts.cache.store(&hash, claims);
            }
            (fresh, false)
        }
    };

    let claims = match claims {
        Some(c) if !c.is_empty() => c,
        _ => return ExtractResult { inserted: 0, parsed: 0, cache_hit },
    };
    let parsed = claims.len();

    // Canonicalize entities + insert. Rows whose entity resolves to `None`
    // (empty after trim) are dropped, matching TS.
    let mut inserted = 0usize;
    let mut row_num: i32 = 1;
    for claim in &claims {
        let Some(canonical) =
            canonicalize_entity(opts.engine, opts.source_id, &claim.entity, opts.alias_map).await
        else {
            continue;
        };

        let new_fact = NewFact {
            fact: claim.text.clone(),
            kind: Some(if claim.event_type.is_some() { FactKind::Event } else { FactKind::Fact }),
            entity_slug: Some(canonical.slug.clone()),
            visibility: Some(FactVisibility::Private),
            context: None,
            valid_from: Some(claim.valid_from.clone()),
            valid_until: None,
            source: "longmemeval:extractor".to_string(),
            source_session: Some(opts.session_id.to_string()),
            confidence: None,
            notability: Some("medium".to_string()),
            claim_metric: claim.metric.clone(),
            claim_value: claim.value,
            claim_unit: claim.unit.clone(),
            claim_period: claim.period.clone(),
            event_type: claim.event_type.clone(),
            row_num: Some(row_num),
            source_markdown_slug: Some(opts.session_slug.to_string()),
        };
        row_num += 1;

        // Fail-open per row: a rejected insert (duplicate text, backend
        // hiccup) costs that one claim, not the whole session. TS's single
        // batch statement would zero the count instead; the difference is
        // telemetry-only.
        match opts.engine.insert_fact(opts.source_id, &canonical.slug, &new_fact).await {
            Ok(FactInsertStatus::Inserted | FactInsertStatus::Superseded) => inserted += 1,
            Ok(FactInsertStatus::Duplicate) | Err(_) => {}
        }
    }

    ExtractResult { inserted, parsed, cache_hit }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;
    use crate::engine::InMemoryEngine;

    fn claim_json(entity: &str, date: &str) -> String {
        format!(
            r#"[{{"entity":"{entity}","metric":"mrr","value":1200,"unit":"USD","period":"monthly","event_type":null,"valid_from":"{date}","text":"{entity} MRR hit 1200 USD"}}]"#
        )
    }

    // ---- parse_extracted_json_array ----

    #[test]
    fn parses_a_bare_json_array() {
        let out = parse_extracted_json_array(r#"[{"entity":"marco"}]"#);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["entity"], "marco");
    }

    #[test]
    fn strips_markdown_fences_before_parsing() {
        let raw = "```json\n[{\"entity\":\"marco\"}]\n```";
        let out = parse_extracted_json_array(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["entity"], "marco");
    }

    #[test]
    fn strips_unlabeled_fences() {
        let out = parse_extracted_json_array("```\n[1, 2, 3]\n```");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn falls_back_to_the_first_bracket_substring() {
        let raw = "Here are the claims:\n[{\"entity\":\"marco\"}]\nHope that helps!";
        let out = parse_extracted_json_array(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["entity"], "marco");
    }

    #[test]
    fn returns_empty_for_blank_or_unparseable_output() {
        assert!(parse_extracted_json_array("").is_empty());
        assert!(parse_extracted_json_array("   \n  ").is_empty());
        assert!(parse_extracted_json_array("I could not find anything.").is_empty());
        // A JSON object (not an array) is not accepted.
        assert!(parse_extracted_json_array(r#"{"entity":"marco"}"#).is_empty());
    }

    // ---- validate_claim ----

    #[test]
    fn validates_a_well_formed_claim() {
        let raw: serde_json::Value = serde_json::from_str(
            r#"{"entity":"marco","metric":"mrr","value":1200.5,"unit":"USD",
                "period":"monthly","event_type":null,"valid_from":"2024-05-01",
                "text":"MRR hit 1200.5"}"#,
        )
        .unwrap();
        let claim = validate_claim(&raw).expect("valid");
        assert_eq!(claim.entity, "marco");
        assert_eq!(claim.metric.as_deref(), Some("mrr"));
        assert_eq!(claim.value, Some(1200.5));
        assert_eq!(claim.event_type, None);
        assert_eq!(claim.valid_from, "2024-05-01");
    }

    #[test]
    fn rejects_rows_missing_the_required_fields() {
        let cases = [
            r#"{"text":"x","valid_from":"2024-05-01"}"#,          // no entity
            r#"{"entity":"  ","text":"x","valid_from":"2024-05-01"}"#, // blank entity
            r#"{"entity":"marco","valid_from":"2024-05-01"}"#,    // no text
            r#"{"entity":"marco","text":"x"}"#,                   // no valid_from
            r#"{"entity":"marco","text":"x","valid_from":"May 2024"}"#, // bad date
            r#"{"entity":"marco","text":"x","valid_from":"2024-5-1"}"#, // unpadded date
        ];
        for case in cases {
            let raw: serde_json::Value = serde_json::from_str(case).unwrap();
            assert!(validate_claim(&raw).is_none(), "should reject: {case}");
        }
        assert!(validate_claim(&serde_json::json!("a string")).is_none());
        assert!(validate_claim(&serde_json::json!(null)).is_none());
    }

    #[test]
    fn accepts_a_longer_iso_valid_from_prefix() {
        let raw = serde_json::json!({
            "entity": "marco", "text": "x", "valid_from": "2024-05-01T10:30:00Z",
        });
        let claim = validate_claim(&raw).expect("valid");
        assert_eq!(claim.valid_from, "2024-05-01T10:30:00Z");
    }

    #[test]
    fn coerces_non_conforming_optional_fields_to_none() {
        let raw = serde_json::json!({
            "entity": "marco", "text": "x", "valid_from": "2024-05-01",
            "metric": 42, "value": "1200", "unit": null, "period": false,
        });
        let claim = validate_claim(&raw).expect("valid");
        assert_eq!(claim.metric, None, "numeric metric is not a string");
        assert_eq!(claim.value, None, "stringy value is not a number");
        assert_eq!(claim.unit, None);
        assert_eq!(claim.period, None);
    }

    #[test]
    fn truncates_claim_text_at_the_char_cap() {
        let raw = serde_json::json!({
            "entity": "marco", "text": "é".repeat(800), "valid_from": "2024-05-01",
        });
        let claim = validate_claim(&raw).expect("valid");
        assert_eq!(claim.text.chars().count(), MAX_CLAIM_TEXT_CHARS);
    }

    // ---- cache ----

    #[test]
    fn cache_counts_hits_misses_and_size() {
        let cache = ExtractorCache::new();
        assert_eq!(cache.stats(), CacheStats { hits: 0, misses: 0, size: 0 });

        assert!(cache.take("h1").is_none());
        assert_eq!(cache.stats().misses, 1);

        cache.store("h1", &[]);
        assert!(cache.take("h1").is_some());
        assert_eq!(cache.stats(), CacheStats { hits: 1, misses: 1, size: 1 });

        cache.reset();
        assert_eq!(cache.stats(), CacheStats { hits: 0, misses: 0, size: 0 });
    }

    #[test]
    fn hashing_is_stable_and_body_sensitive() {
        assert_eq!(hash_session_body("abc"), hash_session_body("abc"));
        assert_ne!(hash_session_body("abc"), hash_session_body("abd"));
        assert_eq!(hash_session_body("abc").len(), 64);
    }

    // ---- canonicalize_entity ----

    #[tokio::test]
    async fn alias_map_collapses_short_and_long_forms() {
        let engine = InMemoryEngine::new();
        let aliases = AliasMap::new();

        let first = canonicalize_entity(&engine, "default", "Marco Smith", &aliases)
            .await
            .expect("resolves");
        let second = canonicalize_entity(&engine, "default", "marco", &aliases)
            .await
            .expect("resolves");
        let third = canonicalize_entity(&engine, "default", "MARCO SMITH", &aliases)
            .await
            .expect("resolves");

        assert_eq!(first.slug, second.slug, "bare first name reuses the full-name slug");
        assert_eq!(first.slug, third.slug, "case-insensitive on the full form");
    }

    #[tokio::test]
    async fn alias_map_first_mention_wins() {
        let engine = InMemoryEngine::new();
        let aliases = AliasMap::new();

        let bare = canonicalize_entity(&engine, "default", "Marco", &aliases).await.unwrap();
        let full = canonicalize_entity(&engine, "default", "Marco Smith", &aliases).await.unwrap();
        assert_eq!(bare.slug, full.slug, "later long form inherits the earlier short-form slug");
    }

    #[tokio::test]
    async fn canonicalize_rejects_blank_entities() {
        let engine = InMemoryEngine::new();
        let aliases = AliasMap::new();
        assert!(canonicalize_entity(&engine, "default", "   ", &aliases).await.is_none());
        assert!(aliases.is_empty());
    }

    // ---- extract_and_insert_claims ----

    #[tokio::test]
    async fn extracts_parses_and_inserts_one_session() {
        let engine = InMemoryEngine::new();
        let chat = MockChatProvider::new("[]");
        chat.queue_text(claim_json("marco", "2024-05-01"));
        let aliases = AliasMap::new();
        let cache = ExtractorCache::new();

        let out = extract_and_insert_claims(ExtractOpts {
            engine: &engine,
            chat: &chat,
            model: "mock:mock-model",
            session_slug: "chat/s1",
            session_id: "s1",
            session_body: "body one",
            source_id: "default",
            alias_map: &aliases,
            cache: &cache,
        })
        .await;

        assert_eq!(out, ExtractResult { inserted: 1, parsed: 1, cache_hit: false });
        assert_eq!(cache.stats().misses, 1);
    }

    #[tokio::test]
    async fn identical_bodies_hit_the_cache_without_a_second_call() {
        let engine = InMemoryEngine::new();
        // Only ONE queued response: a second model call would fall through to
        // the default text `[]` and parse to zero claims, so a nonzero
        // `parsed` on the replay proves the cache served it.
        let chat = MockChatProvider::new("[]");
        chat.queue_text(claim_json("marco", "2024-05-01"));
        let aliases = AliasMap::new();
        let cache = ExtractorCache::new();

        let mk = |slug: &'static str, id: &'static str| ExtractOpts {
            engine: &engine,
            chat: &chat,
            model: "mock:mock-model",
            session_slug: slug,
            session_id: id,
            session_body: "same body",
            source_id: "default",
            alias_map: &aliases,
            cache: &cache,
        };

        let first = extract_and_insert_claims(mk("chat/s1", "s1")).await;
        let second = extract_and_insert_claims(mk("chat/s2", "s2")).await;

        assert!(!first.cache_hit);
        assert!(second.cache_hit, "identical body must reuse the cached claims");
        assert_eq!(second.parsed, 1);
        // Same entity + kind + text as the first insert → `Duplicate`, so the
        // replay reports 0 inserted while still counting `parsed`.
        assert_eq!(second.inserted, 0);
        assert_eq!(cache.stats(), CacheStats { hits: 1, misses: 1, size: 1 });
    }

    #[tokio::test]
    async fn provider_error_fails_open_with_zero_facts() {
        let engine = InMemoryEngine::new();
        let chat = MockChatProvider::new("[]");
        chat.queue_error(crate::ai::chat::ChatError::Transient {
            message: "429 slow down".to_string(),
        });
        let aliases = AliasMap::new();
        let cache = ExtractorCache::new();

        let out = extract_and_insert_claims(ExtractOpts {
            engine: &engine,
            chat: &chat,
            model: "mock:mock-model",
            session_slug: "chat/s1",
            session_id: "s1",
            session_body: "body",
            source_id: "default",
            alias_map: &aliases,
            cache: &cache,
        })
        .await;

        assert_eq!(out, ExtractResult { inserted: 0, parsed: 0, cache_hit: false });
        // A failed call is NOT cached — the next session retries.
        assert_eq!(cache.stats().size, 0);
    }

    #[tokio::test]
    async fn malformed_rows_are_dropped_but_good_rows_survive() {
        let engine = InMemoryEngine::new();
        let chat = MockChatProvider::new("[]");
        chat.queue_text(
            r#"[
              {"entity":"marco","metric":"mrr","value":1200,"unit":"USD","period":"monthly",
               "event_type":null,"valid_from":"2024-05-01","text":"MRR 1200"},
              {"entity":"","text":"blank entity","valid_from":"2024-05-02"},
              {"entity":"acme","text":"no date"},
              {"entity":"acme","metric":null,"value":null,"unit":null,"period":null,
               "event_type":"meeting","valid_from":"2024-06-01","text":"Met the acme team"}
            ]"#,
        );
        let aliases = AliasMap::new();
        let cache = ExtractorCache::new();

        let out = extract_and_insert_claims(ExtractOpts {
            engine: &engine,
            chat: &chat,
            model: "mock:mock-model",
            session_slug: "chat/s1",
            session_id: "s1",
            session_body: "body",
            source_id: "default",
            alias_map: &aliases,
            cache: &cache,
        })
        .await;

        assert_eq!(out.parsed, 2, "two of the four rows validate");
        assert_eq!(out.inserted, 2);
    }

    #[tokio::test]
    async fn non_array_model_output_yields_zero_claims() {
        let engine = InMemoryEngine::new();
        let chat = MockChatProvider::new("Nothing extractable here.");
        let aliases = AliasMap::new();
        let cache = ExtractorCache::new();

        let out = extract_and_insert_claims(ExtractOpts {
            engine: &engine,
            chat: &chat,
            model: "mock:mock-model",
            session_slug: "chat/s1",
            session_id: "s1",
            session_body: "body",
            source_id: "default",
            alias_map: &aliases,
            cache: &cache,
        })
        .await;

        assert_eq!(out, ExtractResult { inserted: 0, parsed: 0, cache_hit: false });
        // An empty (but successful) extraction IS cached — a replay must not
        // pay for a second call.
        assert_eq!(cache.stats().size, 1);
    }
}
