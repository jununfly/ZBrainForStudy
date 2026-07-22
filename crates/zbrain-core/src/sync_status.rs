//! Sync status & report types and pure builders (roadmap 1-6-7-13-4).
//!
//! Ports the read-only half of `src/commands/sync.ts`:
//! `buildSyncStatusReport` / `printSyncStatusReport`. The engine exposes a
//! typed `source_sync_stats` method (no raw-SQL escape hatch); this module
//! turns those raw per-source counts into the structured report and a
//! human-readable table.
//!
//! Reference TS contract: `src/commands/sync.ts:2223` (`SyncStatusReport*`),
//! `src/commands/sync.ts:2249` (`buildSyncStatusReport`),
//! `src/commands/sync.ts:2402` (`printSyncStatusReport`).

use chrono::DateTime;
use serde::{Deserialize, Serialize};

/// Raw per-source counts returned by `BrainEngine::source_sync_stats`.
///
/// Mirrors the `sources` projection of the TS `buildSyncStatusReport` SQL
/// aggregation (pages / chunks_total / chunks_unembedded), plus the source
/// identity + sync metadata needed to render the report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceSyncStat {
    pub source_id: String,
    pub name: String,
    pub local_path: Option<String>,
    /// Derived from `config.syncEnabled !== false` (default true).
    pub sync_enabled: bool,
    pub last_sync_at: Option<String>,
    pub last_commit: Option<String>,
    pub pages: u64,
    pub chunks_total: u64,
    pub chunks_unembedded: u64,
}

/// Staleness classification of a source relative to its last sync.
///
/// Serializes lowercase to match the TS `'fresh' | 'stale' | 'severe' | 'unknown'`
/// string union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StalenessClass {
    Fresh,
    Stale,
    Severe,
    Unknown,
}

impl StalenessClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StalenessClass::Fresh => "fresh",
            StalenessClass::Stale => "stale",
            StalenessClass::Severe => "severe",
            StalenessClass::Unknown => "unknown",
        }
    }
}

/// One source row in a [`SyncStatusReport`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatusReportSource {
    pub source_id: String,
    pub name: String,
    pub local_path: Option<String>,
    pub sync_enabled: bool,
    pub last_sync_at: Option<String>,
    pub staleness_hours: Option<f64>,
    pub staleness_class: StalenessClass,
    pub last_commit: Option<String>,
    pub pages: u64,
    pub chunks_total: u64,
    pub chunks_unembedded: u64,
    pub embedding_coverage_pct: f64,
}

/// Brain-wide sync status report (schema_version 1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatusReport {
    pub schema_version: u8,
    pub generated_at: String,
    pub sources: Vec<SyncStatusReportSource>,
    pub unacknowledged_failures: u64,
    /// Active embedding column name. Rust uses a single column; multimodal
    /// column resolution is a future concern (see TS `resolveEmbeddingColumn`).
    pub embedding_column: String,
}

/// Build a [`SyncStatusReport`] from raw per-source stats.
///
/// * `embedding_column` — active embedding column name (Rust: `"embedding"`).
/// * `unacknowledged_failures` — brain-wide count of unacked sync failures.
///   TS reads a JSONL failure log; the Rust op passes this in (TODO: wire the
///   failure-log reader). Kept as a parameter so the builder stays pure/testable.
#[must_use]
pub fn build_sync_status_report(
    stats: &[SourceSyncStat],
    embedding_column: &str,
    unacknowledged_failures: u64,
) -> SyncStatusReport {
    let now_ms = crate::time::now_epoch_ms();
    let sources = stats
        .iter()
        .map(|s| {
            let (staleness_hours, staleness_class) = match &s.last_sync_at {
                Some(iso) => match parse_iso8601_ms(iso) {
                    Some(ms) if ms > 0 => {
                        let hours = (now_ms - ms) as f64 / 3_600_000.0;
                        let class = if hours < 24.0 {
                            StalenessClass::Fresh
                        } else if hours < 72.0 {
                            StalenessClass::Stale
                        } else {
                            StalenessClass::Severe
                        };
                        (Some((hours * 10.0).round() / 10.0), class)
                    }
                    _ => (None, StalenessClass::Unknown),
                },
                None => (None, StalenessClass::Unknown),
            };
            let coverage = if s.chunks_total == 0 {
                100.0
            } else {
                ((s.chunks_total - s.chunks_unembedded) as f64 / s.chunks_total as f64 * 1000.0)
                    .round()
                    / 10.0
            };
            SyncStatusReportSource {
                source_id: s.source_id.clone(),
                name: s.name.clone(),
                local_path: s.local_path.clone(),
                sync_enabled: s.sync_enabled,
                last_sync_at: s.last_sync_at.clone(),
                staleness_hours,
                staleness_class,
                last_commit: s.last_commit.clone(),
                pages: s.pages,
                chunks_total: s.chunks_total,
                chunks_unembedded: s.chunks_unembedded,
                embedding_coverage_pct: coverage,
            }
        })
        .collect();
    SyncStatusReport {
        schema_version: 1,
        generated_at: crate::time::current_utc_iso8601(),
        sources,
        unacknowledged_failures,
        embedding_column: embedding_column.to_string(),
    }
}

/// Parse an ISO-8601 timestamp to epoch milliseconds, or `None` if unparseable.
fn parse_iso8601_ms(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.timestamp_millis())
        .or_else(|| {
            // Some stored values omit the offset; try naive UTC parse.
            chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S%.fZ")
                .ok()
                .map(|dt| dt.and_utc().timestamp_millis())
        })
}

/// Render a [`SyncStatusReport`] as a human-readable table, mirroring the TS
/// `printSyncStatusReport`. Returns the table as a `String` so callers can
/// write it to any sink (stdout, buffer, test assertion).
#[must_use]
pub fn format_sync_status_report(report: &SyncStatusReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("\nSync status — generated {}", report.generated_at));
    out.push('\n');
    out.push_str(&format!("Embedding column: {}\n", report.embedding_column));

    if report.sources.is_empty() {
        out.push_str("  (no sources registered)");
        return out;
    }

    let headers = ["SOURCE", "STATE", "STALENESS", "PAGES", "EMBEDDED", "LAST SYNC"];
    let rows: Vec<Vec<String>> = report
        .sources
        .iter()
        .map(|s| {
            let stale = match s.staleness_hours {
                None => "never".to_string(),
                Some(h) => format!("{h:.1}h"),
            };
            let mut state_bits: Vec<&str> = Vec::new();
            if !s.sync_enabled {
                state_bits.push("disabled");
            }
            state_bits.push(s.staleness_class.as_str());
            vec![
                s.name.clone(),
                state_bits.join(","),
                stale,
                s.pages.to_string(),
                format!("{}%", s.embedding_coverage_pct),
                s.last_sync_at.clone().unwrap_or_else(|| "(never)".to_string()),
            ]
        })
        .collect();

    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            std::cmp::max(
                h.len(),
                rows.iter().map(|r| r[i].len()).max().unwrap_or(0),
            )
        })
        .collect();
    // Numeric columns (STALENESS=2, PAGES=3, EMBEDDED=4) right-aligned.
    let numeric: std::collections::HashSet<usize> = [2usize, 3, 4].into_iter().collect();
    let fmt = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if numeric.contains(&i) {
                    format!("{:>width$}", c, width = widths[i])
                } else {
                    format!("{:<width$}", c, width = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };

    out.push_str(&fmt(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>()));
    out.push('\n');
    out.push_str(&fmt(&widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>()));
    out.push('\n');
    for r in &rows {
        out.push_str(&fmt(r));
        out.push('\n');
    }
    out.push_str(&format!(
        "\nUnacknowledged sync failures (brain-wide): {}",
        report.unacknowledged_failures
    ));
    let severe = report
        .sources
        .iter()
        .filter(|s| s.staleness_class == StalenessClass::Severe)
        .count();
    if severe > 0 {
        out.push_str(&format!(
            "\nWARNING: {severe} source(s) are SEVERELY stale (>72h). Run `zbrain sync --all` to refresh."
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(last_sync_at: Option<&str>) -> SourceSyncStat {
        SourceSyncStat {
            source_id: "s1".to_string(),
            name: "Src".to_string(),
            local_path: None,
            sync_enabled: true,
            last_sync_at: last_sync_at.map(str::to_string),
            last_commit: None,
            pages: 5,
            chunks_total: 10,
            chunks_unembedded: 0,
        }
    }

    #[test]
    fn build_marks_source_unknown_when_never_synced() {
        let report = build_sync_status_report(&[stat(None)], "embedding", 0);
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.embedding_column, "embedding");
        let s = &report.sources[0];
        assert_eq!(s.staleness_class, StalenessClass::Unknown);
        assert_eq!(s.staleness_hours, None);
        // 10 chunks, 0 unembedded => 100% coverage.
        assert_eq!(s.embedding_coverage_pct, 100.0);
    }

    #[test]
    fn build_classifies_staleness_by_hours() {
        // 1h ago -> fresh
        let one_hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
        let fresh = build_sync_status_report(
            &[SourceSyncStat {
                last_sync_at: Some(one_hour_ago.to_rfc3339()),
                ..stat(None)
            }],
            "embedding",
            0,
        );
        assert_eq!(fresh.sources[0].staleness_class, StalenessClass::Fresh);

        let three_days = chrono::Utc::now() - chrono::Duration::hours(72);
        let severe = build_sync_status_report(
            &[SourceSyncStat {
                last_sync_at: Some(three_days.to_rfc3339()),
                ..stat(None)
            }],
            "embedding",
            0,
        );
        assert_eq!(severe.sources[0].staleness_class, StalenessClass::Severe);
    }

    #[test]
    fn build_computes_embedding_coverage() {
        let report = build_sync_status_report(
            &[SourceSyncStat {
                chunks_total: 10,
                chunks_unembedded: 2,
                ..stat(None)
            }],
            "embedding",
            0,
        );
        // (10 - 2) / 10 = 0.8 => 80.0%
        assert_eq!(report.sources[0].embedding_coverage_pct, 80.0);
    }

    #[test]
    fn format_renders_header_and_rows() {
        let report = build_sync_status_report(
            &[SourceSyncStat {
                name: "my-src".to_string(),
                sync_enabled: false,
                chunks_total: 4,
                chunks_unembedded: 1,
                ..stat(None)
            }],
            "embedding",
            2,
        );
        let table = format_sync_status_report(&report);
        assert!(table.contains("SOURCE"));
        assert!(table.contains("EMBEDDED"));
        assert!(table.contains("my-src"));
        assert!(table.contains("disabled"));
        // 4 chunks, 1 unembedded => 75%
        assert!(table.contains("75%"));
        assert!(table.contains("Unacknowledged sync failures (brain-wide): 2"));
    }

    #[test]
    fn format_handles_empty_sources() {
        let report = build_sync_status_report(&[], "embedding", 0);
        let table = format_sync_status_report(&report);
        assert!(table.contains("(no sources registered)"));
    }
}
