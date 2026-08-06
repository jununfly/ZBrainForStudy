//! v0.31 Hot Memory — confidence decay helper (Rust port of
//! `src/core/facts/decay.ts`).
//!
//! Single source of truth for the per-kind halflife table. Recall,
//! supersession audit, `facts_health`, and the MCP `_meta.brain_hot_memory`
//! injector all call [`effective_confidence`].
//!
//! Formula: `confidence × exp(-age_days / halflife_days)`, clamped to `[0, 1]`.
//! If `valid_until` is set and we're past it, decay returns 0 regardless.

use chrono::{DateTime, NaiveDate, Utc};
use crate::types::{FactKind, FactRow};

/// Halflife in days per fact kind. Exported as a const table so tests can pin
/// the exact values (mirrors TS `HALFLIFE_DAYS: Record<FactKind, number>`).
pub const HALFLIFE_DAYS: &[(FactKind, f64)] = &[
    (FactKind::Event, 7.0),
    (FactKind::Commitment, 90.0),
    (FactKind::Preference, 90.0),
    (FactKind::Belief, 365.0),
    (FactKind::Fact, 365.0),
];

/// Halflife in days for a given fact kind. Panics-free: unknown kinds fall
/// back to the `fact` halflife (365d) — matches the TS `Record` default
/// access semantics where an unknown key would be `undefined` (treated as
/// "no decay multiplier" → caller would have divided by NaN). We instead use
/// the most conservative real value.
#[must_use]
pub fn halflife_days(kind: &FactKind) -> f64 {
    for (k, v) in HALFLIFE_DAYS {
        if k == kind {
            return *v;
        }
    }
    HALFLIFE_DAYS
        .iter()
        .find(|(k, _)| k == &FactKind::Fact)
        .map(|(_, v)| *v)
        .unwrap_or(365.0)
}

/// Compute effective confidence for a fact at a given moment.
///
///   - If the fact is expired (`expired_at` in the past), returns 0.
///   - If `valid_until` is set and `now` is past it, returns 0.
///   - Otherwise: `confidence × exp(-age_days / halflife_days)` clamped to [0,1].
///
/// Pure function. No side effects. No I/O.
///
/// `FactRow` stores the dates as `Option<String>` (RFC3339 or `YYYY-MM-DD`).
/// Unparseable / missing `valid_from` is treated as "age 0" (returns the
/// clamped base confidence) rather than throwing, which is more robust than
/// the TS `Date` access while preserving the same numeric result on real rows.
#[must_use]
pub fn effective_confidence(fact: &FactRow, now: DateTime<Utc>) -> f64 {
    if let Some(exp) = fact.expired_at.as_deref().and_then(parse_date) {
        if exp <= now {
            return 0.0;
        }
    }
    if let Some(vu) = fact.valid_until.as_deref().and_then(parse_date) {
        if vu <= now {
            return 0.0;
        }
    }

    let valid_from = match fact.valid_from.as_deref().and_then(parse_date) {
        Some(t) => t,
        None => return clamp01(fact.confidence),
    };

    let age_days = (now - valid_from).num_seconds() as f64 / 86_400.0;
    if age_days < 0.0 {
        return clamp01(fact.confidence);
    }

    let halflife = halflife_days(&fact.kind);
    let decayed = fact.confidence * (-age_days / halflife).exp();
    clamp01(decayed)
}

/// Convenience wrapper using the current time. Mirrors the TS default
/// `now = new Date()` argument.
#[must_use]
pub fn effective_confidence_now(fact: &FactRow) -> f64 {
    effective_confidence(fact, Utc::now())
}

fn clamp01(x: f64) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else {
        x
    }
}

/// Parse an RFC3339 timestamp or a `YYYY-MM-DD` date into `DateTime<Utc>`.
fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(naive) = d.and_hms_opt(0, 0, 0) {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn mk_fact(kind: FactKind, valid_from: &str, confidence: f64) -> FactRow {
        FactRow {
            id: 1,
            source_id: "default".into(),
            entity_slug: None,
            fact: "x".into(),
            kind,
            visibility: crate::types::FactVisibility::Private,
            notability: "low".into(),
            context: None,
            valid_from: Some(valid_from.into()),
            valid_until: None,
            expired_at: None,
            superseded_by: None,
            consolidated_at: None,
            consolidated_into: None,
            source: "test".into(),
            source_session: None,
            confidence,
            created_at: Some(valid_from.into()),
            row_num: None,
            source_markdown_slug: None,
        }
    }

    #[test]
    fn halflife_table_matches_ts() {
        assert_eq!(halflife_days(&FactKind::Event), 7.0);
        assert_eq!(halflife_days(&FactKind::Commitment), 90.0);
        assert_eq!(halflife_days(&FactKind::Preference), 90.0);
        assert_eq!(halflife_days(&FactKind::Belief), 365.0);
        assert_eq!(halflife_days(&FactKind::Fact), 365.0);
    }

    #[test]
    fn fresh_fact_keeps_confidence() {
        // Age 0 → decay factor 1 → confidence unchanged.
        let now = Utc.with_ymd_and_hms(2026, 8, 6, 0, 0, 0).unwrap();
        let fact = mk_fact(FactKind::Fact, "2026-08-06T00:00:00Z", 0.9);
        assert!((effective_confidence(&fact, now) - 0.9).abs() < 1e-9);
    }

    #[test]
    fn fact_one_year_has_known_decay() {
        // fact halflife 365d; 365 days old → ~0.368 of original.
        let now = Utc.with_ymd_and_hms(2027, 8, 6, 0, 0, 0).unwrap();
        let fact = mk_fact(FactKind::Fact, "2026-08-06T00:00:00Z", 1.0);
        let e = effective_confidence(&fact, now);
        assert!((e - (-1.0f64).exp()).abs() < 1e-6, "got {e}");
    }

    #[test]
    fn expired_at_returns_zero() {
        let now = Utc.with_ymd_and_hms(2026, 8, 6, 12, 0, 0).unwrap();
        let mut fact = mk_fact(FactKind::Fact, "2026-08-01T00:00:00Z", 0.9);
        fact.expired_at = Some("2026-08-05T00:00:00Z".into());
        assert_eq!(effective_confidence(&fact, now), 0.0);
    }

    #[test]
    fn valid_until_past_returns_zero() {
        let now = Utc.with_ymd_and_hms(2026, 8, 6, 12, 0, 0).unwrap();
        let mut fact = mk_fact(FactKind::Fact, "2026-08-01T00:00:00Z", 0.9);
        fact.valid_until = Some("2026-08-05".into());
        assert_eq!(effective_confidence(&fact, now), 0.0);
    }

    #[test]
    fn event_decays_fast() {
        // event halflife 7d; 7 days old → ~0.368 of original.
        let now = Utc.with_ymd_and_hms(2026, 8, 8, 0, 0, 0).unwrap();
        let fact = mk_fact(FactKind::Event, "2026-08-01T00:00:00Z", 1.0);
        let e = effective_confidence(&fact, now);
        assert!((e - (-1.0f64).exp()).abs() < 1e-6, "got {e}");
    }

    #[test]
    fn negative_age_clamps_to_base() {
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let fact = mk_fact(FactKind::Fact, "2026-08-06T00:00:00Z", 0.7);
        assert!((effective_confidence(&fact, now) - 0.7).abs() < 1e-9);
    }
}
