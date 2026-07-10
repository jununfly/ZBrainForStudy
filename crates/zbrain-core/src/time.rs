//! Dependency-free UTC timestamp helpers shared by test doubles and slice code.

use std::time::{SystemTime, UNIX_EPOCH};

#[must_use]
pub fn current_utc_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_secs();
    unix_seconds_to_utc_iso8601(secs)
}

/// Current wall-clock time as Unix epoch **milliseconds**.
///
/// Used by the minion job queue's scheduling columns (lock_until /
/// delay_until / timeout_at): on the SQLite backend these are stored as
/// INTEGER epoch-ms and all `now() + N ms` arithmetic happens in Rust
/// (SQLite has no interval type). Returns `i64` so it composes with the
/// signed durations the queue passes around.
#[must_use]
pub fn now_epoch_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after Unix epoch")
            .as_millis(),
    )
    .expect("current epoch milliseconds fits in i64")
}

#[must_use]
pub fn unix_seconds_to_utc_iso8601(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).expect("current timestamp day count fits in i64");
    let remaining = secs % 86_400;
    let hour = remaining / 3_600;
    let minute = (remaining % 3_600) / 60;
    let second = remaining % 60;
    let (year, month, day) = civil_from_unix_days(days);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_unix_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);

    (
        i32::try_from(year).expect("current timestamp year fits in i32"),
        u32::try_from(month).expect("calendar month fits in u32"),
        u32::try_from(day).expect("calendar day fits in u32"),
    )
}

/// Return an ISO 8601 timestamp `hours` after the given ISO 8601 timestamp.
/// Simple string-based arithmetic on the hour component. Suitable for
/// archive_expires_at (72h window) where sub-hour precision is unnecessary.
#[must_use]
pub fn add_hours(iso8601: &str, hours: u32) -> String {
    // Format: "YYYY-MM-DDTHH:MM:SSZ" — 20 chars.
    if iso8601.len() < 20 {
        // Fallback: compute from now.
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after Unix epoch")
            .as_secs()
            + u64::from(hours) * 3600;
        return unix_seconds_to_utc_iso8601(secs);
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_secs()
        + u64::from(hours) * 3600;
    unix_seconds_to_utc_iso8601(secs)
}
