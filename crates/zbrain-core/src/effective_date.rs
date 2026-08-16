//! Effective-date computation, ported from TS `src/core/effective-date.ts`.
//!
//! The "effective date" answers "when was this page *about*?" — distinct from
//! `updated_at` (churns on auto-link) and `created_at` (row insert time). It is
//! the user's stated content date, resolved through a frontmatter precedence
//! chain with a per-prefix filename override.
//!
//! Precedence (default order):
//!   1. `frontmatter.event_date`  — meeting / event pages
//!   2. `frontmatter.date`        — dated essays
//!   3. `frontmatter.published`   — writing
//!   4. filename-date             — leading `YYYY-MM-DD` in basename
//!   5. `updated_at`              — fallback
//!   6. `created_at`              — last resort (only if updated_at NULL)
//!
//! For `daily/` and `meetings/` slug prefixes the filename date jumps to
//! position 1 (the filename is the user's primary signal there).
//!
//! Range validation: parsed values must fall in `[1990-01-01, NOW + 1 year]`;
//! out-of-range / unparseable values are dropped and the chain falls through.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::Value;

use crate::types::EffectiveDateSource;

/// Slug prefixes where the filename date wins over frontmatter dates.
const FILENAME_FIRST_PREFIXES: &[&str] = &["daily/", "meetings/"];

fn min_date() -> DateTime<Utc> {
    // 1990-01-01T00:00:00Z — valid in all chrono editions.
    Utc.with_ymd_and_hms(1990, 1, 1, 0, 0, 0).unwrap()
}

fn max_date() -> DateTime<Utc> {
    // NOW + 1 year, computed at call time so the boundary moves with the clock
    // (matches the TS implementation's moving upper bound).
    Utc::now() + Duration::days(365)
}

/// Parse a frontmatter value as a `DateTime<Utc>`. Accepts RFC3339 / ISO
/// strings, bare `YYYY-MM-DD`, and epoch-millisecond numbers. Returns `None`
/// on any failure.
pub fn parse_date_loose(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Try full RFC3339 / ISO-8601 first.
            if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
                return Some(dt.with_timezone(&Utc));
            }
            // Fall back to a bare calendar date at midnight UTC.
            if let Ok(nd) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
                if let Some(dt) = nd.and_hms_opt(0, 0, 0) {
                    return Some(Utc.from_utc_datetime(&dt));
                }
            }
            None
        }
        Value::Number(n) => {
            // Mirror TS `new Date(number)`: treat as epoch milliseconds.
            let ms = n.as_i64()?;
            DateTime::from_timestamp_millis(ms)
        }
        _ => None,
    }
}

fn validate_in_range(d: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    let d = d?;
    let ms = d.timestamp_millis();
    if ms < min_date().timestamp_millis() {
        return None;
    }
    if ms > max_date().timestamp_millis() {
        return None;
    }
    Some(d)
}

/// Extract a leading `YYYY-MM-DD` date from a filename basename (no extension).
fn extract_filename_date(filename: &str) -> Option<DateTime<Utc>> {
    let b = filename.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let digits: Vec<u32> = b[0..10]
        .iter()
        .enumerate()
        .filter_map(|(i, &c)| {
            if i == 4 || i == 7 {
                None
            } else if c.is_ascii_digit() {
                Some((c - b'0') as u32)
            } else {
                None
            }
        })
        .collect();
    if digits.len() != 8 {
        return None;
    }
    let year = digits[0] * 1000 + digits[1] * 100 + digits[2] * 10 + digits[3];
    let month = digits[4] * 10 + digits[5];
    let day = digits[6] * 10 + digits[7];
    let nd = chrono::NaiveDate::from_ymd_opt(year as i32, month, day)?;
    let dt = nd.and_hms_opt(0, 0, 0)?;
    validate_in_range(Some(Utc.from_utc_datetime(&dt)))
}

fn has_filename_first_prefix(slug: &str) -> bool {
    FILENAME_FIRST_PREFIXES.iter().any(|p| slug.starts_with(p))
}

/// Result of [`compute_effective_date`]: the resolved date (if any) and the
/// source label (used by the doctor's `effective_date_health` check to flag
/// rows that fell through to `fallback`).
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveDateResult {
    pub date: Option<DateTime<Utc>>,
    pub source: EffectiveDateSource,
}

/// Run the precedence chain. Returns the first valid (in-range) date and its
/// source label, falling through to `updated_at` / `created_at` as `fallback`.
pub fn compute_effective_date(
    slug: &str,
    frontmatter: &Value,
    filename: Option<&str>,
    updated_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
) -> EffectiveDateResult {
    let filename_first = has_filename_first_prefix(slug);

    let fm_event = validate_in_range(parse_date_loose(
        frontmatter.get("event_date").unwrap_or(&Value::Null),
    ));
    let fm_date =
        validate_in_range(parse_date_loose(frontmatter.get("date").unwrap_or(&Value::Null)));
    let fm_published = validate_in_range(parse_date_loose(
        frontmatter.get("published").unwrap_or(&Value::Null),
    ));
    let filename_date = extract_filename_date(filename.unwrap_or(""));

    // Build the ordered candidate list. For filename-first prefixes
    // (daily/, meetings/) the filename moves to the head of the chain.
    let candidates: [(Option<DateTime<Utc>>, EffectiveDateSource); 4] = if filename_first {
        [
            (filename_date, EffectiveDateSource::Filename),
            (fm_event, EffectiveDateSource::EventDate),
            (fm_date, EffectiveDateSource::Date),
            (fm_published, EffectiveDateSource::Published),
        ]
    } else {
        [
            (fm_event, EffectiveDateSource::EventDate),
            (fm_date, EffectiveDateSource::Date),
            (fm_published, EffectiveDateSource::Published),
            (filename_date, EffectiveDateSource::Filename),
        ]
    };

    for (date, source) in candidates.iter() {
        if date.is_some() {
            return EffectiveDateResult {
                date: *date,
                source: *source,
            };
        }
    }

    // Fallback chain: updated_at, then created_at.
    if let Some(upd) = validate_in_range(Some(updated_at)) {
        return EffectiveDateResult {
            date: Some(upd),
            source: EffectiveDateSource::Fallback,
        };
    }
    if let Some(cre) = validate_in_range(Some(created_at)) {
        return EffectiveDateResult {
            date: Some(cre),
            source: EffectiveDateSource::Fallback,
        };
    }

    EffectiveDateResult {
        date: None,
        source: EffectiveDateSource::Fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fm(pairs: &[(&str, &str)]) -> Value {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), Value::String((*v).to_string()));
        }
        Value::Object(m)
    }

    #[test]
    fn event_date_wins_over_date() {
        let r = compute_effective_date(
            "notes/x",
            &fm(&[("date", "2020-01-01"), ("event_date", "2021-06-15")]),
            None,
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        );
        assert_eq!(r.source, EffectiveDateSource::EventDate);
        assert_eq!(
            r.date,
            Some(Utc.with_ymd_and_hms(2021, 6, 15, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn filename_first_for_daily_prefix() {
        let r = compute_effective_date(
            "daily/2024-03-15-acme",
            &fm(&[("date", "2020-01-01")]),
            Some("2024-03-15-acme"),
            Utc.with_ymd_and_hms(2024, 5, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2024, 5, 1, 0, 0, 0).unwrap(),
        );
        assert_eq!(r.source, EffectiveDateSource::Filename);
        assert_eq!(
            r.date,
            Some(Utc.with_ymd_and_hms(2024, 3, 15, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn falls_back_to_updated_at_when_frontmatter_empty() {
        let upd = Utc.with_ymd_and_hms(2023, 2, 2, 0, 0, 0).unwrap();
        let r = compute_effective_date("notes/x", &fm(&[]), None, upd, upd);
        assert_eq!(r.source, EffectiveDateSource::Fallback);
        assert_eq!(r.date, Some(upd));
    }

    #[test]
    fn out_of_range_date_is_dropped() {
        // 1969 is before MIN_DATE (1990) → dropped, falls through to fallback.
        let upd = Utc.with_ymd_and_hms(2023, 2, 2, 0, 0, 0).unwrap();
        let r = compute_effective_date(
            "notes/x",
            &fm(&[("date", "1969-01-01")]),
            None,
            upd,
            upd,
        );
        assert_eq!(r.source, EffectiveDateSource::Fallback);
        assert_eq!(r.date, Some(upd));
    }
}
