//! v0.31.2 — facts:absorb writer (Rust port of `src/core/facts/absorb-log.ts`).
//!
//! D5 contract: every absorbed failure in the facts extraction pipeline
//! writes one row to the existing `ingest_log` table, scoped per source and
//! grouped by stable reason codes so the doctor + admin dashboard can
//! categorize failures.
//!
//! Reasons:
//!   - `gateway_error`  — HTTP 429/5xx, timeout, network blip on chat()/embed().
//!   - `parse_failure`  — LLM returned malformed JSON, all parser fallbacks failed.
//!   - `queue_overflow` — FactsQueue cap hit; oldest entry dropped.
//!   - `queue_shutdown` — queue rejected the enqueue because shutdown is in progress.
//!   - `embed_failure`  — gateway down on embed; row inserts with NULL embedding.
//!   - `pipeline_error` — anything else absorbed inside the backstop catch.
//!
//! The writer is best-effort — a failure to log SHOULDN'T blow up the
//! caller's actual work. Errors are caught and stderr-warned; the caller
//! proceeds.

use crate::engine::BrainEngine;
use crate::types::IngestLogInput;
use std::sync::atomic::{AtomicBool, Ordering};

/// Stable reason codes for `facts:absorb` ingest-log rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactsAbsorbReason {
    GatewayError,
    ParseFailure,
    QueueOverflow,
    QueueShutdown,
    EmbedFailure,
    PipelineError,
}

impl FactsAbsorbReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FactsAbsorbReason::GatewayError => "gateway_error",
            FactsAbsorbReason::ParseFailure => "parse_failure",
            FactsAbsorbReason::QueueOverflow => "queue_overflow",
            FactsAbsorbReason::QueueShutdown => "queue_shutdown",
            FactsAbsorbReason::EmbedFailure => "embed_failure",
            FactsAbsorbReason::PipelineError => "pipeline_error",
        }
    }
}

/// All reason codes, for iteration/telemetry.
pub const FACTS_ABSORB_REASONS: &[FactsAbsorbReason] = &[
    FactsAbsorbReason::GatewayError,
    FactsAbsorbReason::ParseFailure,
    FactsAbsorbReason::QueueOverflow,
    FactsAbsorbReason::QueueShutdown,
    FactsAbsorbReason::EmbedFailure,
    FactsAbsorbReason::PipelineError,
];

// v0.39.3.0 WARN-4 + CV13 — module-scoped flag so the first-occurrence
// diagnostic log fires ONCE per process (same semantics as the TS test seam).
static HAS_LOGGED_DISCONNECTED: AtomicBool = AtomicBool::new(false);

/// Test seam: reset the first-occurrence flag between runs.
pub fn reset_disconnected_flag_for_tests() {
    HAS_LOGGED_DISCONNECTED.store(false, Ordering::SeqCst);
}

/// Write one row to `ingest_log` for a `facts:absorb` event. Best-effort.
pub async fn write_facts_absorb_log(
    engine: &dyn BrainEngine,
    reference: &str,
    reason: FactsAbsorbReason,
    detail: &str,
    source_id: &str,
) {
    let cleaned_detail: String = detail.chars().take(240).collect();
    let input = IngestLogInput {
        source_id: source_id.to_string(),
        source_type: "facts:absorb".to_string(),
        source_ref: reference.to_string(),
        pages_updated: vec![],
        summary: format!("{}: {}", reason.as_str(), cleaned_detail),
    };

    if let Err(e) = engine.log_ingest(&input).await {
        // Typed check: the 'No database connection' class fires after every
        // `zbrain capture` because the facts subsystem opens its own engine
        // handle that isn't connected in the CLI capture path. First
        // occurrence prints a trace; subsequent ones are silent.
        if e.to_string().contains("No database connection") {
            if !HAS_LOGGED_DISCONNECTED.swap(true, Ordering::SeqCst) {
                eprintln!(
                    "[facts:absorb] suppressed: 'No database connection' fires on a separate engine handle \
                     (known WARN-4 in v0.38; subsequent occurrences silent this process). First-occurrence trace: {e:?}"
                );
            }
            return;
        }
        // All other failures keep the loud warn — observability can't break
        // the runtime path, but real subsystem errors should be visible.
        eprintln!(
            "[facts:absorb] failed to log {} for {}: {}",
            reason.as_str(),
            reference,
            e
        );
    }
}

/// Classify an arbitrary error into one of the stable reason codes. Heuristic
/// substring match on the lowercased error message; falls back to
/// `pipeline_error` when nothing matches. (Plain `str::contains` keeps this
/// allocation-free on the hot path — no per-call regex compilation.)
#[must_use]
pub fn classify_facts_absorb_error(err: &(dyn std::error::Error + 'static)) -> FactsAbsorbReason {
    let m = err.to_string().to_lowercase();

    if contains_any(&m, &["timeout", "timed out", "etimedout"]) {
        return FactsAbsorbReason::GatewayError;
    }
    if contains_any(&m, &["429", "rate limit", "rate-limit", "too many requests"]) {
        return FactsAbsorbReason::GatewayError;
    }
    if contains_any(&m, &["500", "501", "502", "503", "504", "server error", "internal server", "bad gateway", "service unavail"])
        || m.contains("5xx")
    {
        return FactsAbsorbReason::GatewayError;
    }
    if contains_any(&m, &["econnreset", "econnrefused", "eai_again", "getaddrinfo"]) {
        return FactsAbsorbReason::GatewayError;
    }
    if contains_any(&m, &["json.parse", "unexpected token", "invalid json", "not valid json"]) {
        return FactsAbsorbReason::ParseFailure;
    }
    if contains_any(&m, &["queueoverflowerror", "queue overflow", "overflow", "cap hit"]) {
        return FactsAbsorbReason::QueueOverflow;
    }
    if contains_any(&m, &["queueshutdownerror", "queue shutdown", "shutting down", "shutting down"]) {
        return FactsAbsorbReason::QueueShutdown;
    }
    if m.contains("embed") && (m.contains("fail") || m.contains("error")) {
        return FactsAbsorbReason::EmbedFailure;
    }

    FactsAbsorbReason::PipelineError
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(msg: &str) -> FactsAbsorbReason {
        let err = std::io::Error::new(std::io::ErrorKind::Other, msg);
        classify_facts_absorb_error(&err)
    }

    #[test]
    fn timeout_is_gateway_error() {
        assert_eq!(
            classify("request timed out after 30s"),
            FactsAbsorbReason::GatewayError
        );
    }

    #[test]
    fn rate_limit_is_gateway_error() {
        assert_eq!(
            classify("429 Too Many Requests"),
            FactsAbsorbReason::GatewayError
        );
    }

    #[test]
    fn json_parse_is_parse_failure() {
        assert_eq!(
            classify("JSON.parse: unexpected token"),
            FactsAbsorbReason::ParseFailure
        );
    }

    #[test]
    fn queue_overflow_classified() {
        assert_eq!(
            classify("QueueOverflowError: cap hit"),
            FactsAbsorbReason::QueueOverflow
        );
    }

    #[test]
    fn queue_shutdown_classified() {
        assert_eq!(
            classify("queue shutdown in progress"),
            FactsAbsorbReason::QueueShutdown
        );
    }

    #[test]
    fn embed_failure_classified() {
        assert_eq!(
            classify("embed failed: gateway down"),
            FactsAbsorbReason::EmbedFailure
        );
    }

    #[test]
    fn unknown_is_pipeline_error() {
        assert_eq!(
            classify("something totally unknown"),
            FactsAbsorbReason::PipelineError
        );
    }
}
