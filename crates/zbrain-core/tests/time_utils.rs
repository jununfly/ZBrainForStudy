use zbrain_core::time::{current_utc_iso8601, unix_seconds_to_utc_iso8601};

fn assert_iso8601_utc_seconds(ts: &str) {
    assert_eq!(ts.len(), "1970-01-01T00:00:00Z".len());
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[7..8], "-");
    assert_eq!(&ts[10..11], "T");
    assert_eq!(&ts[13..14], ":");
    assert_eq!(&ts[16..17], ":");
    assert_eq!(&ts[19..20], "Z");
}

#[test]
fn unix_seconds_to_utc_iso8601_handles_epoch_and_leap_days() {
    assert_eq!(unix_seconds_to_utc_iso8601(0), "1970-01-01T00:00:00Z");
    assert_eq!(unix_seconds_to_utc_iso8601(86_400), "1970-01-02T00:00:00Z");
    assert_eq!(
        unix_seconds_to_utc_iso8601(951_868_799),
        "2000-02-29T23:59:59Z"
    );
    assert_eq!(
        unix_seconds_to_utc_iso8601(1_709_164_800),
        "2024-02-29T00:00:00Z"
    );
}

#[test]
fn current_utc_iso8601_returns_timestamp_shape_without_old_sentinel() {
    let ts = current_utc_iso8601();

    assert_iso8601_utc_seconds(&ts);
    assert_ne!(ts, "2026-01-01T00:00:00Z");
}
