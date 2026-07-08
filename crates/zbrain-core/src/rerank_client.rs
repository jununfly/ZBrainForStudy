//! Cross-encoder rerank HTTP client + query-pipeline post-processing.
//!
//! Mirrors the TypeScript runtime split across `src/core/ai/gateway.ts`
//! (`rerank()` — the raw HTTP call + error classification) and
//! `src/core/search/rerank.ts` (`applyReranker()` — the head/tail reorder +
//! fail-open + audit). Both collapse into this one module on the Rust side:
//! [`RerankClient`] is the transport seam, [`apply_reranker`] is the
//! call-site logic that reorders results and fails open to RRF order.
//!
//! Fail-open contract: every error class (auth, rate-limit, network, timeout,
//! payload-too-large, unknown) logs one row to the rerank-audit JSONL
//! ([`crate::rerank_audit`]) and returns the input results in their original
//! RRF order. Search reliability beats reranker quality — a flaky upstream
//! must never break or stall search. This module is the SOLE production
//! producer of rerank-audit failure rows (see `rerank_audit.rs` header).
//!
//! Only the top [`DEFAULT_RERANK_TOP_N`] rows are sent to the cross-encoder;
//! the un-reranked tail keeps its RRF order and is appended unchanged. This
//! mirrors the TS `topNIn` default and bounds both latency and the 5MB ZE
//! payload cap.

use crate::rerank_audit::RerankFailureReason;

/// How many of the top RRF results are sent to the cross-encoder. Mirrors the
/// TS `applyReranker` `topNIn` default of 30 (`src/core/search/rerank.ts:27`):
/// only the head is reranked; the tail keeps its RRF order and is appended.
/// Hardcoded (not config-exposed) to mirror the TS current state — no caller
/// has asked to tune it, and full-corpus rerank would both diverge from TS and
/// risk hitting [`MAX_RERANK_PAYLOAD_BYTES`] on large result sets.
pub const DEFAULT_RERANK_TOP_N: usize = 30;

/// Per-request rerank timeout. Mirrors the TS `DEFAULT_RERANK_TIMEOUT_MS`
/// (`src/core/ai/gateway.ts:2813`). Search is a hot path; a long upstream
/// stall degrades UX, so an exceeded deadline classifies as
/// [`RerankFailureReason::Timeout`] and fails open to RRF order.
pub const DEFAULT_RERANK_TIMEOUT_MS: u64 = 5000;

/// Upstream request-body cap enforced BEFORE the HTTP call (ZeroEntropy limits
/// `/v1/models/rerank` to 5MB). Mirrors the TS `max_payload_bytes` on the ZE
/// reranker touchpoint (`src/core/ai/recipes/zeroentropyai.ts:62`). A serialized
/// body over this cap short-circuits to [`RerankFailureReason::PayloadTooLarge`]
/// without ever issuing the request.
pub const MAX_RERANK_PAYLOAD_BYTES: usize = 5_000_000;

/// Default cross-encoder model (`provider:model`). Mirrors the TS
/// `DEFAULT_RERANKER_MODEL` (`src/core/ai/gateway.ts:75`) and the ZE reranker
/// touchpoint default (`src/core/ai/recipes/zeroentropyai.ts:57`).
pub const DEFAULT_RERANK_MODEL: &str = "zeroentropyai:zerank-2";

/// Default ZeroEntropy rerank endpoint. Mirrors the TS base-URL default
/// (`src/core/ai/recipes/zeroentropyai.ts:36`) joined with the rerank path
/// (`src/core/ai/gateway.ts:2883` — `/models/rerank`).
pub const DEFAULT_RERANK_ENDPOINT: &str = "https://api.zeroentropy.dev/v1/models/rerank";

/// Classified rerank failure. `reason` drives both the fail-open audit row
/// ([`RerankFailureReason`]) and — for callers who care — the distinction
/// between transient (network/timeout/rate_limit) and terminal (auth) modes.
/// Mirrors the TS `RerankError` (`src/core/ai/gateway.ts:2772`); the reason
/// set is identical so audit rows round-trip byte-for-byte with the TS writer.
#[derive(Debug, Clone)]
pub struct RerankError {
    /// Human-readable summary (truncated to 200 chars by the audit writer).
    pub message: String,
    /// Stable classification; identical variants to the TS `reason` union.
    pub reason: RerankFailureReason,
    /// HTTP status when the failure came from a response (None for
    /// timeout / transport / pre-flight errors).
    pub status: Option<u16>,
}

impl std::fmt::Display for RerankError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RerankError {}

impl RerankError {
    fn new(message: impl Into<String>, reason: RerankFailureReason) -> Self {
        Self { message: message.into(), reason, status: None }
    }

    fn with_status(message: impl Into<String>, reason: RerankFailureReason, status: u16) -> Self {
        Self { message: message.into(), reason, status: Some(status) }
    }
}

/// A single reranked result: which input document (by its position in the
/// `documents` slice sent to [`RerankClient::rerank`]) and its cross-encoder
/// relevance score. Mirrors the TS `RerankResult` (`{index, relevanceScore}`,
/// `src/core/ai/gateway.ts:2794`). Upstream returns these sorted by score
/// descending, but [`apply_reranker`] does not rely on that ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankOutcome {
    /// Zero-based index into the documents slice that was sent upstream.
    pub index: usize,
    /// Cross-encoder relevance score (higher = more relevant).
    pub relevance_score: f64,
}

/// Input to a single rerank call. Mirrors the TS `RerankInput`
/// (`src/core/ai/gateway.ts:2783`), minus the budget/tracker plumbing which
/// is not part of the Rust search path yet.
#[derive(Debug, Clone)]
pub struct RerankRequest {
    /// Query text. Empty query is rejected as `unknown` (matches TS).
    pub query: String,
    /// Candidate document texts, positionally aligned with the returned
    /// [`RerankOutcome::index`]. Empty slice short-circuits to `[]`.
    pub documents: Vec<String>,
    /// `provider:model` override. `None` uses [`DEFAULT_RERANK_MODEL`].
    pub model: Option<String>,
    /// Per-call timeout override in ms. `None` uses
    /// [`DEFAULT_RERANK_TIMEOUT_MS`].
    pub timeout_ms: Option<u64>,
}

/// Transport seam for the cross-encoder rerank call. Production wires the
/// `reqwest`-backed [`ZeroEntropyRerankClient`]; tests install a mock to
/// exercise the [`apply_reranker`] reorder + fail-open + audit paths without
/// touching the network (mirrors the TS `__setRerankTransportForTests` seam
/// and the `MockEmbeddingProvider` precedent in `embedding.rs`).
#[async_trait::async_trait]
pub trait RerankClient: Send + Sync {
    /// Submit a query + documents to the reranker. Returns per-document
    /// scores on success, or a classified [`RerankError`] the caller maps to
    /// a fail-open audit row.
    async fn rerank(&self, req: &RerankRequest) -> Result<Vec<RerankOutcome>, RerankError>;
}

/// Everything the query pipeline needs to run the rerank post-processing
/// stage. Carried on `OperationContext` (as `Option`, `None` = reranker off)
/// so the operation layer can gate + fail-open without the engine trait or
/// the CLI layer needing to know about reranking. Bundled (rather than four
/// loose context fields) so an off configuration is a single `None`.
#[derive(Clone)]
pub struct RerankSettings {
    /// The transport (real ZE client in production, mock in tests).
    pub client: std::sync::Arc<dyn RerankClient>,
    /// Directory the fail-open audit JSONL is written under
    /// (`~/.zbrain/audit` in production; overridable via `ZBRAIN_AUDIT_DIR`).
    pub audit_dir: std::path::PathBuf,
    /// `provider:model` override recorded in audit rows / sent upstream.
    /// `None` uses [`DEFAULT_RERANK_MODEL`].
    pub model: Option<String>,
}

impl std::fmt::Debug for RerankSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RerankSettings")
            .field("audit_dir", &self.audit_dir)
            .field("model", &self.model)
            .field("client", &"Arc<dyn RerankClient>")
            .finish()
    }
}

/// SHA-256 prefix (8 hex chars) of the query, for privacy-preserving audit
/// dedupe. Mirrors the TS `hashQuery` (`src/core/search/rerank.ts:43`): never
/// log query text, but let the doctor collapse repeat failures on one query.
fn hash_query(query: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(query.as_bytes());
    hex::encode(digest)[..8].to_string()
}

/// Reorder the top [`DEFAULT_RERANK_TOP_N`] results by cross-encoder score,
/// stamping `rerank_score` + `reranker_delta` on each reordered head row, and
/// return the head followed by the un-reranked tail in its original RRF order.
///
/// Fail-open: `enabled = false`, an empty `results`, or any [`RerankError`]
/// returns the input unchanged. On error, one audit row is logged to
/// `audit_dir` first. Never panics, never propagates the error — search must
/// survive a flaky reranker. Mirrors the TS `applyReranker`
/// (`src/core/search/rerank.ts:58`).
///
/// `document_of` extracts the text sent to the cross-encoder for each result
/// (the matched span; the caller decides the exact field). The head is the
/// first `DEFAULT_RERANK_TOP_N` rows; the tail is everything after and is
/// appended verbatim so recall is preserved rather than truncated.
pub async fn apply_reranker<T>(
    client: &dyn RerankClient,
    enabled: bool,
    query: &str,
    mut results: Vec<T>,
    audit_dir: &std::path::Path,
    model: Option<&str>,
    document_of: impl Fn(&T) -> String,
    stamp: impl Fn(&mut T, f64, i64),
) -> Vec<T> {
    if !enabled || results.is_empty() {
        return results;
    }

    let top_n = DEFAULT_RERANK_TOP_N.min(results.len());
    // Split off the tail first so the head Vec owns exactly the rows we rerank;
    // the tail is re-appended unchanged after the reorder.
    let tail: Vec<T> = results.split_off(top_n);
    let mut head = results;

    let documents: Vec<String> = head.iter().map(&document_of).collect();
    let req = RerankRequest {
        query: query.to_string(),
        documents,
        model: model.map(str::to_string),
        timeout_ms: None,
    };

    let outcomes = match client.rerank(&req).await {
        Ok(o) => o,
        Err(e) => {
            // Fail-open: log one audit row, then return the untouched RRF
            // order (head + tail rejoined). Audit write is itself best-effort.
            crate::rerank_audit::log_rerank_failure(
                audit_dir,
                crate::rerank_audit::RerankFailureInput {
                    model: model.unwrap_or(DEFAULT_RERANK_MODEL).to_string(),
                    reason: e.reason,
                    query_hash: hash_query(query),
                    doc_count: head.len() as u32,
                    error_summary: e.message,
                },
            );
            head.extend(tail);
            return head;
        }
    };

    // Defensive: a malformed/empty upstream response passes through as the
    // original order (matches TS `reranked.length === 0` guard).
    if outcomes.is_empty() {
        head.extend(tail);
        return head;
    }

    // Rebuild the head in reranker order. We move each referenced row out of
    // `head` into `reordered`, leaving `None` placeholders so any row the
    // reranker omitted (rare; only with explicit top_n) can be re-appended in
    // its original position afterward — never silently dropped.
    let mut slots: Vec<Option<T>> = head.into_iter().map(Some).collect();
    let mut reordered: Vec<T> = Vec::with_capacity(slots.len());
    let mut seen = vec![false; slots.len()];
    for o in &outcomes {
        if o.index < slots.len() && !seen[o.index] {
            if let Some(mut item) = slots[o.index].take() {
                seen[o.index] = true;
                // reranker_delta = original RRF index - new head position
                // (positive = moved up). Computed here as a free by-product so
                // `--explain` need not re-derive it. Mirrors TS
                // `src/core/search/rerank.ts:123`.
                let delta = o.index as i64 - reordered.len() as i64;
                stamp(&mut item, o.relevance_score, delta);
                reordered.push(item);
            }
        }
    }
    // Re-append any head rows the reranker omitted, in original order, so
    // recall is preserved (matches TS `!seen.has(i)` loop).
    for slot in &mut slots {
        if let Some(item) = slot.take() {
            reordered.push(item);
        }
    }

    reordered.extend(tail);
    reordered
}

// --- Real ZeroEntropy HTTP client (requires the `rerank` feature) ---

/// `reqwest`-backed cross-encoder client hitting ZeroEntropy's
/// `/v1/models/rerank`. Mirrors the TS `rerank()` transport path
/// (`src/core/ai/gateway.ts:2832`): pre-flight payload guard, Bearer auth,
/// AbortController-style timeout, and the exact HTTP-status → reason
/// classification. The API key comes from the environment (secrets never land
/// in the config file — same posture as the embedding provider key).
#[cfg(feature = "rerank")]
pub struct ZeroEntropyRerankClient {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

#[cfg(feature = "rerank")]
impl ZeroEntropyRerankClient {
    /// Build a client from an explicit key + optional endpoint override.
    #[must_use]
    pub fn new(api_key: impl Into<String>, endpoint: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_RERANK_ENDPOINT.to_string()),
            api_key: api_key.into(),
        }
    }

    /// Build a client reading the key from `ZEROENTROPY_API_KEY`. Returns
    /// `None` when the var is unset/empty so the caller can leave the reranker
    /// disabled rather than fail search. Mirrors the TS env-driven auth
    /// resolution (`auth_env.required = ['ZEROENTROPY_API_KEY']`).
    #[must_use]
    pub fn from_env(endpoint: Option<String>) -> Option<Self> {
        match std::env::var("ZEROENTROPY_API_KEY") {
            Ok(k) if !k.is_empty() => Some(Self::new(k, endpoint)),
            _ => None,
        }
    }
}

/// Wire-format request body for `/v1/models/rerank`. Field names match the ZE
/// contract exactly (`{model, query, documents[], top_n?}`); `top_n` is
/// omitted so every input document is scored (matches TS default).
#[cfg(feature = "rerank")]
#[derive(serde::Serialize)]
struct ZeRerankBody<'a> {
    model: &'a str,
    query: &'a str,
    documents: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    top_n: Option<usize>,
}

/// Wire-format response: `{results: [{index, relevance_score}]}`. Mirrors the
/// shape the TS mapper reads (`src/core/ai/gateway.ts:2974`).
#[cfg(feature = "rerank")]
#[derive(serde::Deserialize)]
struct ZeRerankResponse {
    results: Vec<ZeRerankResultRow>,
}

#[cfg(feature = "rerank")]
#[derive(serde::Deserialize)]
struct ZeRerankResultRow {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    relevance_score: f64,
}

#[cfg(feature = "rerank")]
#[async_trait::async_trait]
impl RerankClient for ZeroEntropyRerankClient {
    async fn rerank(&self, req: &RerankRequest) -> Result<Vec<RerankOutcome>, RerankError> {
        if req.query.is_empty() {
            return Err(RerankError::new("rerank: query is required", RerankFailureReason::Unknown));
        }
        if req.documents.is_empty() {
            return Ok(Vec::new());
        }

        let model = req.model.as_deref().unwrap_or(DEFAULT_RERANK_MODEL);
        // ZE model id is the part after `provider:`; a bare id is used as-is.
        let model_id = model.rsplit(':').next().unwrap_or(model);

        let body = ZeRerankBody {
            model: model_id,
            query: &req.query,
            documents: &req.documents,
            top_n: None,
        };
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| RerankError::new(format!("rerank: serialize failed: {e}"), RerankFailureReason::Unknown))?;

        // Pre-flight payload guard — fail open BEFORE issuing the request when
        // the body exceeds the upstream 5MB cap. Mirrors TS gateway.ts:2905.
        if body_bytes.len() > MAX_RERANK_PAYLOAD_BYTES {
            return Err(RerankError::new(
                format!(
                    "rerank payload {} bytes exceeds {} byte cap",
                    body_bytes.len(),
                    MAX_RERANK_PAYLOAD_BYTES
                ),
                RerankFailureReason::PayloadTooLarge,
            ));
        }

        let timeout = std::time::Duration::from_millis(
            req.timeout_ms.unwrap_or(DEFAULT_RERANK_TIMEOUT_MS),
        );

        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body_bytes)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| classify_transport_error(&e))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            let mut msg = format!("rerank HTTP {status}");
            if !text.is_empty() {
                let snippet: String = text.chars().take(500).collect();
                msg = format!("{msg}: {snippet}");
            }
            // Status → reason, identical table to TS gateway.ts:2960.
            let reason = match status {
                401 | 403 => RerankFailureReason::Auth,
                429 => RerankFailureReason::RateLimit,
                s if s >= 500 => RerankFailureReason::Network,
                _ => RerankFailureReason::Unknown,
            };
            return Err(RerankError::with_status(msg, reason, status));
        }

        let parsed: ZeRerankResponse = resp
            .json()
            .await
            .map_err(|e| RerankError::new(format!("rerank: malformed response: {e}"), RerankFailureReason::Unknown))?;

        Ok(parsed
            .results
            .into_iter()
            .map(|r| RerankOutcome { index: r.index, relevance_score: r.relevance_score })
            .collect())
    }
}

/// Classify a `reqwest` transport error into a rerank reason. Timeout →
/// `Timeout`; everything else (DNS, connection refused, TLS) → `Network`.
/// Mirrors the TS catch block (`src/core/ai/gateway.ts:2980`) where an
/// AbortError-on-timeout becomes `timeout` and other transport errors become
/// `network`.
#[cfg(feature = "rerank")]
fn classify_transport_error(e: &reqwest::Error) -> RerankError {
    if e.is_timeout() {
        RerankError::new(format!("rerank timed out: {e}"), RerankFailureReason::Timeout)
    } else {
        RerankError::new(format!("rerank: {e}"), RerankFailureReason::Network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// A search-result stand-in for the reorder tests. Holds its original RRF
    /// rank plus the two rerank stamp slots (mirrors the two fields added to
    /// `engine::SearchResult`). `apply_reranker` is generic over the row type
    /// via the `document_of` / `stamp` closures, so this local type is enough.
    #[derive(Debug, Clone, PartialEq)]
    struct Row {
        text: String,
        rerank_score: Option<f64>,
        reranker_delta: Option<i64>,
    }

    impl Row {
        fn new(text: &str) -> Self {
            Self { text: text.to_string(), rerank_score: None, reranker_delta: None }
        }
    }

    fn document_of(r: &Row) -> String {
        r.text.clone()
    }

    fn stamp(r: &mut Row, score: f64, delta: i64) {
        r.rerank_score = Some(score);
        r.reranker_delta = Some(delta);
    }

    /// Mock rerank client. Returns a canned outcome list or a canned error,
    /// and records the requests it saw so tests can assert what was sent.
    struct MockRerank {
        result: Result<Vec<RerankOutcome>, RerankError>,
        seen: Mutex<Vec<RerankRequest>>,
    }

    impl MockRerank {
        fn ok(outcomes: Vec<RerankOutcome>) -> Self {
            Self { result: Ok(outcomes), seen: Mutex::new(Vec::new()) }
        }

        fn err(reason: RerankFailureReason, msg: &str) -> Self {
            Self { result: Err(RerankError::new(msg, reason)), seen: Mutex::new(Vec::new()) }
        }
    }

    #[async_trait::async_trait]
    impl RerankClient for MockRerank {
        async fn rerank(&self, req: &RerankRequest) -> Result<Vec<RerankOutcome>, RerankError> {
            self.seen.lock().unwrap().push(req.clone());
            self.result.clone()
        }
    }

    fn rows(texts: &[&str]) -> Vec<Row> {
        texts.iter().map(|t| Row::new(t)).collect()
    }

    fn audit_file(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().starts_with("rerank-failures-")))
    }

    // --- pass-through guards -------------------------------------------------

    #[tokio::test]
    async fn disabled_passes_through_untouched() {
        let dir = TempDir::new().unwrap();
        let client = MockRerank::ok(vec![]);
        let input = rows(&["a", "b", "c"]);
        let out = apply_reranker(
            &client, false, "q", input.clone(), dir.path(), None, document_of, stamp,
        )
        .await;
        assert_eq!(out, input, "disabled reranker must return input verbatim");
        assert!(client.seen.lock().unwrap().is_empty(), "must not call upstream when disabled");
        assert!(audit_file(dir.path()).is_none(), "no audit row when disabled");
    }

    #[tokio::test]
    async fn empty_results_pass_through() {
        let dir = TempDir::new().unwrap();
        let client = MockRerank::ok(vec![]);
        let out = apply_reranker(
            &client, true, "q", Vec::<Row>::new(), dir.path(), None, document_of, stamp,
        )
        .await;
        assert!(out.is_empty());
        assert!(client.seen.lock().unwrap().is_empty(), "no upstream call for empty input");
    }

    // --- happy-path reorder + stamps ----------------------------------------

    #[tokio::test]
    async fn reorders_head_and_stamps_score_and_delta() {
        let dir = TempDir::new().unwrap();
        // 3 rows; reranker flips them: doc2 best, then doc0, then doc1.
        let client = MockRerank::ok(vec![
            RerankOutcome { index: 2, relevance_score: 0.9 },
            RerankOutcome { index: 0, relevance_score: 0.5 },
            RerankOutcome { index: 1, relevance_score: 0.1 },
        ]);
        let input = rows(&["zero", "one", "two"]);
        let out = apply_reranker(
            &client, true, "q", input, dir.path(), None, document_of, stamp,
        )
        .await;

        assert_eq!(out.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), vec!["two", "zero", "one"]);
        // Scores stamped in reranker order.
        assert_eq!(out[0].rerank_score, Some(0.9));
        assert_eq!(out[1].rerank_score, Some(0.5));
        assert_eq!(out[2].rerank_score, Some(0.1));
        // delta = original index - new head position.
        assert_eq!(out[0].reranker_delta, Some(2 - 0)); // "two": 2 -> 0
        assert_eq!(out[1].reranker_delta, Some(0 - 1)); // "zero": 0 -> 1
        assert_eq!(out[2].reranker_delta, Some(1 - 2)); // "one": 1 -> 2
        assert!(audit_file(dir.path()).is_none(), "success writes no audit row");
    }

    #[tokio::test]
    async fn sends_query_and_documents_upstream() {
        let dir = TempDir::new().unwrap();
        let client = MockRerank::ok(vec![RerankOutcome { index: 0, relevance_score: 1.0 }]);
        let _ = apply_reranker(
            &client, true, "my query", rows(&["doc-a"]), dir.path(), Some("prov:model"), document_of, stamp,
        )
        .await;
        let seen = client.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].query, "my query");
        assert_eq!(seen[0].documents, vec!["doc-a".to_string()]);
        assert_eq!(seen[0].model.as_deref(), Some("prov:model"));
    }

    // --- fail-open on each error class + audit row --------------------------

    async fn assert_fail_open(reason: RerankFailureReason) {
        let dir = TempDir::new().unwrap();
        let client = MockRerank::err(reason, "boom");
        let input = rows(&["a", "b", "c"]);
        let out = apply_reranker(
            &client, true, "the query", input.clone(), dir.path(), Some("zeroentropyai:zerank-2"), document_of, stamp,
        )
        .await;
        // Fail-open: original RRF order, no stamps.
        assert_eq!(out, input, "fail-open must return input unchanged for {reason:?}");
        // Exactly one audit row was written and it round-trips with the reason.
        let path = audit_file(dir.path()).expect("audit file should be written on failure");
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "one audit row per failure");
        let event: crate::rerank_audit::RerankFailureEvent =
            serde_json::from_str(lines[0]).unwrap();
        assert_eq!(event.reason, reason);
        assert_eq!(event.model, "zeroentropyai:zerank-2");
        assert_eq!(event.doc_count, 3);
        assert_eq!(event.severity, "warn");
        // Query text is never logged — only its 8-char hash.
        assert_eq!(event.query_hash.len(), 8);
        assert!(!content.contains("the query"), "raw query text must never hit the audit");
    }

    #[tokio::test]
    async fn fail_open_auth() {
        assert_fail_open(RerankFailureReason::Auth).await;
    }

    #[tokio::test]
    async fn fail_open_rate_limit() {
        assert_fail_open(RerankFailureReason::RateLimit).await;
    }

    #[tokio::test]
    async fn fail_open_network() {
        assert_fail_open(RerankFailureReason::Network).await;
    }

    #[tokio::test]
    async fn fail_open_timeout() {
        assert_fail_open(RerankFailureReason::Timeout).await;
    }

    #[tokio::test]
    async fn fail_open_payload_too_large() {
        assert_fail_open(RerankFailureReason::PayloadTooLarge).await;
    }

    #[tokio::test]
    async fn fail_open_unknown() {
        assert_fail_open(RerankFailureReason::Unknown).await;
    }

    #[tokio::test]
    async fn fail_open_uses_default_model_in_audit_when_none() {
        let dir = TempDir::new().unwrap();
        let client = MockRerank::err(RerankFailureReason::Network, "boom");
        let _ = apply_reranker(
            &client, true, "q", rows(&["a"]), dir.path(), None, document_of, stamp,
        )
        .await;
        let path = audit_file(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let event: crate::rerank_audit::RerankFailureEvent =
            serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(event.model, DEFAULT_RERANK_MODEL, "audit falls back to default model label");
    }

    // --- head/tail split: only top_n reranked, tail keeps RRF order ---------

    #[tokio::test]
    async fn tail_beyond_top_n_keeps_rrf_order() {
        let dir = TempDir::new().unwrap();
        // Build DEFAULT_RERANK_TOP_N + 3 rows. Only the head is reranked; the
        // tail must stay in original order, appended after the reordered head.
        let n = DEFAULT_RERANK_TOP_N;
        let texts: Vec<String> = (0..n + 3).map(|i| format!("d{i}")).collect();
        let input: Vec<Row> = texts.iter().map(|t| Row::new(t)).collect();
        // Reranker reverses only the head it was given (indices 0..n).
        let outcomes: Vec<RerankOutcome> = (0..n)
            .rev()
            .map(|i| RerankOutcome { index: i, relevance_score: i as f64 })
            .collect();
        let client = MockRerank::ok(outcomes);
        let out = apply_reranker(
            &client, true, "q", input, dir.path(), None, document_of, stamp,
        )
        .await;

        // Only n documents were sent upstream (the head).
        assert_eq!(client.seen.lock().unwrap()[0].documents.len(), n, "only top_n sent upstream");
        // Head got reversed; tail (last 3) preserved in original order.
        assert_eq!(out[0].text, format!("d{}", n - 1), "head reordered by reranker");
        assert_eq!(out[n].text, format!("d{n}"), "tail row 1 keeps RRF position");
        assert_eq!(out[n + 1].text, format!("d{}", n + 1));
        assert_eq!(out[n + 2].text, format!("d{}", n + 2));
        // Tail rows carry no rerank stamps.
        assert_eq!(out[n].rerank_score, None);
        assert_eq!(out[n].reranker_delta, None);
    }

    #[tokio::test]
    async fn omitted_head_rows_reappended_not_dropped() {
        let dir = TempDir::new().unwrap();
        // Reranker returns only index 1 (drops 0 and 2). We must NOT lose
        // recall: the omitted rows are re-appended in original order.
        let client = MockRerank::ok(vec![RerankOutcome { index: 1, relevance_score: 0.9 }]);
        let out = apply_reranker(
            &client, true, "q", rows(&["a", "b", "c"]), dir.path(), None, document_of, stamp,
        )
        .await;
        assert_eq!(out.len(), 3, "no row dropped");
        assert_eq!(out[0].text, "b", "reranked row first");
        assert_eq!(out[0].rerank_score, Some(0.9));
        // omitted rows keep original order, no stamps.
        assert_eq!(out[1].text, "a");
        assert_eq!(out[1].rerank_score, None);
        assert_eq!(out[2].text, "c");
        assert_eq!(out[2].rerank_score, None);
    }

    #[tokio::test]
    async fn empty_upstream_response_passes_through() {
        let dir = TempDir::new().unwrap();
        let client = MockRerank::ok(vec![]);
        let input = rows(&["a", "b"]);
        let out = apply_reranker(
            &client, true, "q", input.clone(), dir.path(), None, document_of, stamp,
        )
        .await;
        assert_eq!(out, input, "malformed/empty upstream response = pass through, no stamps");
        assert!(audit_file(dir.path()).is_none(), "empty response is not a failure");
    }
}

/// Wire-contract tests for the real ZeroEntropy client (require the `rerank`
/// feature so `reqwest` and the wire structs are compiled).
#[cfg(all(test, feature = "rerank"))]
mod ze_wire_tests {
    use super::*;

    #[test]
    fn request_body_matches_ze_contract() {
        let docs = vec!["alpha".to_string(), "beta".to_string()];
        let body = ZeRerankBody {
            model: "zerank-2",
            query: "hello",
            documents: &docs,
            top_n: None,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["model"], "zerank-2");
        assert_eq!(json["query"], "hello");
        assert_eq!(json["documents"][0], "alpha");
        assert_eq!(json["documents"][1], "beta");
        // top_n omitted (every document scored) per TS default.
        assert!(json.get("top_n").is_none(), "top_n must be omitted when None");
    }

    #[test]
    fn response_parses_index_and_relevance_score() {
        let raw = r#"{"results":[{"index":2,"relevance_score":0.87},{"index":0,"relevance_score":0.12}]}"#;
        let parsed: ZeRerankResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].index, 2);
        assert_eq!(parsed.results[0].relevance_score, 0.87);
        assert_eq!(parsed.results[1].index, 0);
    }

    #[test]
    fn from_env_none_when_key_unset() {
        // Save/restore to avoid leaking state into other tests.
        let saved = std::env::var("ZEROENTROPY_API_KEY").ok();
        std::env::remove_var("ZEROENTROPY_API_KEY");
        assert!(ZeroEntropyRerankClient::from_env(None).is_none());
        if let Some(v) = saved {
            std::env::set_var("ZEROENTROPY_API_KEY", v);
        }
    }
}
