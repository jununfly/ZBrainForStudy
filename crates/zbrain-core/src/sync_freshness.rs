//! Sync-freshness doctor check — flags federated sources whose `local_path` is
//! set but whose `last_sync_at` has gone stale.
//!
//! Port of the TS `checkSyncFreshness` + `_resolveSyncFreshnessHours`
//! (`src/commands/doctor.ts`, v0.32.4). Split into two halves so the hot path
//! is pure and deterministic:
//!
//! * [`resolve_freshness_hours`] — reads a threshold (in hours) from an env
//!   var, applying the same undefined/empty/invalid → fallback rules as TS.
//!   The only impure part; the caller resolves the two thresholds once and
//!   hands the numbers to the classifier.
//! * [`classify_sync_freshness`] — a pure function over the source list, an
//!   injected `now_ms`, and the resolved warn/fail hour thresholds. No env,
//!   no wall clock ⇒ the boundary tests are stable (mirrors the TS `nowMs`
//!   injection seam that fixed the "exactly 72h ago" flake, PR #1138).
//!
//! Unlike the TS original — which reached for `engine.executeRaw` with a raw
//! `SELECT ... FROM sources WHERE local_path IS NOT NULL` — this reads the
//! already-typed `SourceRow` list the caller pulls from `list_sources`, so no
//! raw-SQL escape hatch is introduced.

use crate::engine::SourceRow;

/// Overall status of the sync-freshness check (worst-of across sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFreshnessStatus {
    Ok,
    Warn,
    Fail,
}

/// Milliseconds per hour.
const MS_PER_HOUR: i64 = 60 * 60 * 1000;

/// Default warn threshold (hours) when the env var is unset/invalid.
pub const DEFAULT_WARN_HOURS: f64 = 24.0;
/// Default fail threshold (hours) when the env var is unset/invalid.
pub const DEFAULT_FAIL_HOURS: f64 = 72.0;

/// Env var overriding the warn threshold.
pub const ENV_WARN_HOURS: &str = "ZBRAIN_SYNC_FRESHNESS_WARN_HOURS";
/// Env var overriding the fail threshold.
pub const ENV_FAIL_HOURS: &str = "ZBRAIN_SYNC_FRESHNESS_FAIL_HOURS";

/// Resolve a freshness threshold (in hours) from an env var, mirroring the TS
/// `_resolveSyncFreshnessHours`.
///
/// * undefined / empty            ⇒ `fallback`
/// * non-finite or `<= 0`         ⇒ `fallback`
/// * otherwise                    ⇒ the parsed number
///
/// TS logged a once-per-process warning on the invalid path; we drop that
/// stateful side effect — the fallback value is what callers depend on and
/// keeping the helper pure avoids a global flag.
#[must_use]
pub fn resolve_freshness_hours(var_name: &str, fallback: f64) -> f64 {
    match std::env::var(var_name) {
        // `raw === ''` in TS short-circuits before `Number(raw)`.
        Ok(raw) if !raw.is_empty() => match raw.trim().parse::<f64>() {
            Ok(n) if n.is_finite() && n > 0.0 => n,
            _ => fallback,
        },
        _ => fallback,
    }
}

/// Parse an ISO-8601 / RFC-3339 timestamp into Unix epoch **milliseconds**,
/// returning `None` on failure.
///
/// Mirrors the TS `new Date(x).getTime()` semantics: a corrupt timestamp
/// yields `NaN` there, and every subsequent numeric comparison against `NaN`
/// is false, so the source is effectively skipped (treated as fresh). Here a
/// parse failure returns `None` and the classifier `continue`s past it — same
/// observable behaviour without the `NaN` gymnastics.
#[must_use]
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Classify the freshness of the given sources against an injected `now_ms`.
///
/// Only federated sources (those with `local_path` set) participate, mirroring
/// the TS `WHERE local_path IS NOT NULL` filter. Returns the worst-of status
/// plus a user-facing message. `warn_hours` / `fail_hours` are the resolved
/// thresholds (see [`resolve_freshness_hours`]).
///
/// Per-source rules (in order):
/// * no `last_sync_at`            ⇒ fail ("never been synced")
/// * unparseable `last_sync_at`   ⇒ skipped (treated fresh; see [`parse_iso_ms`])
/// * `last_sync_at` in the future ⇒ warn (clock skew / corrupted timestamp)
/// * age `> fail_ms`              ⇒ fail (stale search)
/// * age `> warn_ms`              ⇒ warn
/// * otherwise                    ⇒ contributes nothing (fresh)
#[must_use]
pub fn classify_sync_freshness(
    sources: &[SourceRow],
    now_ms: i64,
    warn_hours: f64,
    fail_hours: f64,
) -> (SyncFreshnessStatus, String) {
    let federated: Vec<&SourceRow> = sources.iter().filter(|s| s.local_path.is_some()).collect();

    if federated.is_empty() {
        return (
            SyncFreshnessStatus::Ok,
            "No federated sources to sync".to_string(),
        );
    }

    let warn_ms = (warn_hours * MS_PER_HOUR as f64) as i64;
    let fail_ms = (fail_hours * MS_PER_HOUR as f64) as i64;

    let mut issues: Vec<String> = Vec::new();
    let mut has_warnings = false;
    let mut has_failures = false;

    for source in &federated {
        // Embed source.id in user-visible messages so `zbrain sync --source
        // <id>` matches what the user copy-pastes. Show display name in parens
        // when set and distinct from the id.
        let display = if !source.name.is_empty() && source.name != source.id {
            format!("'{}' ({})", source.id, source.name)
        } else {
            format!("'{}'", source.id)
        };

        let Some(last_sync_at) = source.last_sync_at.as_deref() else {
            issues.push(format!("Source {display} has never been synced"));
            has_failures = true;
            continue;
        };

        let Some(last_sync_ms) = parse_iso_ms(last_sync_at) else {
            // Corrupt timestamp: skip (matches TS NaN-comparison fall-through).
            continue;
        };

        let age_ms = now_ms - last_sync_ms;

        if age_ms < 0 {
            issues.push(format!(
                "Source {display} has future last_sync_at — clock skew or corrupted timestamp"
            ));
            has_warnings = true;
            continue;
        }

        let age_hours = age_ms / MS_PER_HOUR;
        let age_days = age_hours / 24;

        if age_ms > fail_ms {
            issues.push(format!(
                "Source {display} last synced {age_days}d ago — brain search is stale!"
            ));
            has_failures = true;
        } else if age_ms > warn_ms {
            issues.push(format!("Source {display} last synced {age_hours}h ago"));
            has_warnings = true;
        }
    }

    if has_failures {
        return (
            SyncFreshnessStatus::Fail,
            format!(
                "{}. Run `zbrain sync --source <id>` for each stale source",
                issues.join("; ")
            ),
        );
    }
    if has_warnings {
        return (
            SyncFreshnessStatus::Warn,
            format!(
                "{}. Run `zbrain sync --source <id>` to refresh",
                issues.join("; ")
            ),
        );
    }
    (
        SyncFreshnessStatus::Ok,
        format!(
            "All {} federated source(s) synced recently",
            federated.len()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    /// Fixed reference "now" for deterministic boundary tests.
    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 15, 0, 0, 0).unwrap()
    }

    fn now_ms() -> i64 {
        now().timestamp_millis()
    }

    /// RFC-3339 string for `hours` before the fixed `now()`.
    fn hours_ago(hours: i64) -> String {
        (now() - Duration::hours(hours)).to_rfc3339()
    }

    /// RFC-3339 string for `hours` after the fixed `now()`.
    fn hours_from_now(hours: i64) -> String {
        (now() + Duration::hours(hours)).to_rfc3339()
    }

    fn src(id: &str, name: &str, local_path: Option<&str>, last_sync_at: Option<&str>) -> SourceRow {
        SourceRow {
            id: id.to_string(),
            name: name.to_string(),
            local_path: local_path.map(String::from),
            last_commit: None,
            last_sync_at: last_sync_at.map(String::from),
            config: serde_json::Value::Null,
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        }
    }

    #[test]
    fn empty_source_list_is_ok() {
        let (status, msg) = classify_sync_freshness(&[], now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Ok);
        assert_eq!(msg, "No federated sources to sync");
    }

    #[test]
    fn source_without_local_path_is_ignored() {
        // A non-federated source (no local_path) is filtered out, leaving zero
        // federated sources ⇒ ok.
        let sources = vec![src("s1", "S1", None, None)];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Ok);
        assert_eq!(msg, "No federated sources to sync");
    }

    #[test]
    fn fresh_source_is_ok() {
        let sources = vec![src("s1", "s1", Some("/repo/s1"), Some(&hours_ago(1)))];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Ok);
        assert_eq!(msg, "All 1 federated source(s) synced recently");
    }

    #[test]
    fn never_synced_is_fail() {
        let sources = vec![src("s1", "S1", Some("/repo/s1"), None)];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Fail);
        assert!(msg.contains("has never been synced"), "msg = {msg}");
        assert!(msg.contains("'s1' (S1)"), "should show display name: {msg}");
    }

    #[test]
    fn future_timestamp_is_warn() {
        let sources = vec![src("s1", "s1", Some("/repo/s1"), Some(&hours_from_now(2)))];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Warn);
        assert!(msg.contains("future last_sync_at"), "msg = {msg}");
    }

    #[test]
    fn beyond_fail_threshold_is_fail() {
        // 100h ago > 72h fail threshold.
        let sources = vec![src("s1", "s1", Some("/repo/s1"), Some(&hours_ago(100)))];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Fail);
        assert!(msg.contains("brain search is stale"), "msg = {msg}");
        assert!(msg.contains("4d ago"), "100h floors to 4d: {msg}");
    }

    #[test]
    fn between_warn_and_fail_is_warn() {
        // 48h ago: > 24h warn, <= 72h fail.
        let sources = vec![src("s1", "s1", Some("/repo/s1"), Some(&hours_ago(48)))];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Warn);
        assert!(msg.contains("last synced 48h ago"), "msg = {msg}");
    }

    #[test]
    fn failure_dominates_warning() {
        // One stale (fail) + one moderately old (warn) ⇒ overall fail.
        let sources = vec![
            src("warned", "warned", Some("/repo/w"), Some(&hours_ago(48))),
            src("failed", "failed", Some("/repo/f"), Some(&hours_ago(200))),
        ];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Fail);
        assert!(msg.contains("for each stale source"), "msg = {msg}");
        // Both issues surface, joined by "; ".
        assert!(msg.contains("'warned'"), "msg = {msg}");
        assert!(msg.contains("'failed'"), "msg = {msg}");
    }

    #[test]
    fn unparseable_timestamp_is_skipped() {
        // Corrupt last_sync_at ⇒ skipped (treated fresh) ⇒ overall ok.
        let sources = vec![src("s1", "s1", Some("/repo/s1"), Some("not-a-date"))];
        let (status, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert_eq!(status, SyncFreshnessStatus::Ok);
        assert_eq!(msg, "All 1 federated source(s) synced recently");
    }

    #[test]
    fn display_uses_id_only_when_name_equals_id() {
        let sources = vec![src("s1", "s1", Some("/repo/s1"), None)];
        let (_, msg) = classify_sync_freshness(&sources, now_ms(), 24.0, 72.0);
        assert!(msg.contains("Source 's1' has never"), "no parens: {msg}");
        assert!(!msg.contains("('s1')"), "should not repeat id in parens: {msg}");
    }

    #[test]
    fn custom_thresholds_are_honoured() {
        // 10h ago with a tight 6h/8h grid ⇒ fail.
        let sources = vec![src("s1", "s1", Some("/repo/s1"), Some(&hours_ago(10)))];
        let (status, _) = classify_sync_freshness(&sources, now_ms(), 6.0, 8.0);
        assert_eq!(status, SyncFreshnessStatus::Fail);
    }

    #[test]
    fn parse_iso_ms_round_trips() {
        let ms = parse_iso_ms("2026-07-15T00:00:00Z").unwrap();
        assert_eq!(ms, now_ms());
        // The Rust wall-clock format (trailing `Z`, no offset) also parses.
        assert!(parse_iso_ms("2026-07-15T10:23:16Z").is_some());
        assert!(parse_iso_ms("garbage").is_none());
    }

    #[test]
    fn resolve_freshness_hours_fallback_when_unset() {
        // A var name guaranteed not to be set in the environment.
        let n = resolve_freshness_hours("ZBRAIN_SYNC_FRESHNESS_TEST_UNSET_XYZ", 24.0);
        assert_eq!(n, 24.0);
    }

    #[test]
    fn resolve_freshness_hours_valid_and_invalid() {
        let var = "ZBRAIN_SYNC_FRESHNESS_TEST_VALID_ABC";

        std::env::set_var(var, "12");
        assert_eq!(resolve_freshness_hours(var, 24.0), 12.0);

        // Invalid (non-numeric) ⇒ fallback.
        std::env::set_var(var, "not-a-number");
        assert_eq!(resolve_freshness_hours(var, 24.0), 24.0);

        // Non-positive ⇒ fallback.
        std::env::set_var(var, "0");
        assert_eq!(resolve_freshness_hours(var, 24.0), 24.0);
        std::env::set_var(var, "-5");
        assert_eq!(resolve_freshness_hours(var, 24.0), 24.0);

        // Empty ⇒ fallback.
        std::env::set_var(var, "");
        assert_eq!(resolve_freshness_hours(var, 24.0), 24.0);

        std::env::remove_var(var);
    }
}
