//! Feature-recommendation engine (ported from `src/commands/features.ts`).
//!
//! Scope of this module: the **pure, side-effect-free** half of the TS
//! `features` command — the scanner that turns brain health/stats + configured
//! secrets into a list of recommendations, plus the `should_pitch` decline
//! filter. These need no `BrainEngine` and no disk IO, so they are exercised by
//! plain unit tests with dependency-injected inputs.
//!
//! Deliberately NOT ported here:
//!   * `feature-offers.json` persistence (load/save) — file IO, belongs to the
//!     CLI wiring slice.
//!   * `execute_auto_fix` — dispatches to `embed`/`extract` commands which have
//!     no Rust CLI equivalent yet.
//!   * `engine.get_stats()` returning `BrainStats` — Rust `BrainEngine` has no
//!     `BrainStats` accessor yet (the existing `get_stats` methods return admin
//!     `Stats` / minions `QueueStats`). Callers must build `BrainStatsSnapshot`
//!     from whatever source is available; wiring the real engine accessor is a
//!     separate prerequisite slice.
//!
//! Note: no roadmap node number is referenced in comments on purpose — the
//! Part11 roadmap JSON is a temporary working file cleared on completion, so
//! comments stay self-explanatory.

use serde::{Deserialize, Serialize};

// ── Embedded recipe metadata (binary-safe, no disk reads) ─────────────────
//
// Mirrors `RECIPE_META` in features.ts. Each recipe is "configured" only when
// every one of its secret env vars is present & non-empty.

/// A single integration recipe and the env-var secrets it needs.
pub struct RecipeMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub secrets: &'static [&'static str],
}

/// The canonical recipe catalog (verbatim from features.ts `RECIPE_META`).
pub const RECIPE_META: &[RecipeMeta] = &[
    RecipeMeta { id: "email-to-brain", name: "Email to Brain", secrets: &["GMAIL_APP_PASSWORD"] },
    RecipeMeta { id: "calendar-to-brain", name: "Calendar Sync", secrets: &["GOOGLE_CALENDAR_API_KEY"] },
    RecipeMeta { id: "x-to-brain", name: "X/Twitter to Brain", secrets: &["X_BEARER_TOKEN"] },
    RecipeMeta { id: "twilio-voice-brain", name: "Voice to Brain", secrets: &["TWILIO_AUTH_TOKEN"] },
    RecipeMeta { id: "meeting-sync", name: "Meeting Sync", secrets: &["CIRCLEBACK_API_KEY"] },
    RecipeMeta { id: "credential-gateway", name: "Credential Gateway", secrets: &["OAUTH_CLIENT_SECRET"] },
    RecipeMeta { id: "ngrok-tunnel", name: "Ngrok Tunnel", secrets: &["NGROK_AUTHTOKEN"] },
];

// ── Types ─────────────────────────────────────────────────────────────────

/// Recommendation priority. P1 = data-quality (always pitched); P2 = feature
/// adoption (respects the decline filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum FeaturePriority {
    /// Data quality — fix first, always pitched.
    P1,
    /// Unused feature — subject to decline filter.
    P2,
}

impl From<FeaturePriority> for u8 {
    fn from(p: FeaturePriority) -> u8 {
        match p {
            FeaturePriority::P1 => 1,
            FeaturePriority::P2 => 2,
        }
    }
}

impl TryFrom<u8> for FeaturePriority {
    type Error = String;
    fn try_from(v: u8) -> Result<Self, String> {
        match v {
            1 => Ok(FeaturePriority::P1),
            2 => Ok(FeaturePriority::P2),
            other => Err(format!("invalid feature priority: {other}")),
        }
    }
}

/// A single recommendation surfaced by the scanner. Field names/JSON shape
/// mirror the TS `FeatureRecommendation` for wire parity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureRecommendation {
    pub id: String,
    pub priority: FeaturePriority,
    pub title: String,
    pub pitch: String,
    pub command: String,
    pub auto_fixable: bool,
}

/// Persistent decline/accept ledger (`feature-offers.json`). Ported as a plain
/// data type so `should_pitch` can be tested without touching disk; the actual
/// load/save lives in the CLI wiring slice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FeatureOffers {
    #[serde(rename = "lastVersion", default)]
    pub last_version: String,
    #[serde(rename = "lastScan", default)]
    pub last_scan: String,
    #[serde(default)]
    pub declined: std::collections::HashMap<String, OfferStamp>,
    #[serde(default)]
    pub accepted: std::collections::HashMap<String, OfferStamp>,
}

/// A timestamped decline/accept record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfferStamp {
    pub at: String,
    pub version: String,
}

/// Brain health inputs the scanner reads. Mirrors the subset of `BrainHealth`
/// that features.ts actually consumes (`getHealth()`), so the caller can adapt
/// the real `BrainHealth` struct into this snapshot.
#[derive(Debug, Clone, Copy)]
pub struct HealthSnapshot {
    pub missing_embeddings: u64,
    pub dead_links: u64,
    /// 0.0..=1.0
    pub embed_coverage: f64,
    pub brain_score: u32,
}

/// Brain stats inputs the scanner reads. Mirrors the subset of `BrainStats`
/// consumed by features.ts (`getStats()`). Rust has no engine accessor for this
/// yet — the caller assembles it from available counts.
#[derive(Debug, Clone, Copy)]
pub struct BrainStatsSnapshot {
    pub page_count: u64,
    pub link_count: u64,
    pub timeline_entry_count: u64,
}

/// All dependency-injected inputs for a scan. Keeping this as an explicit
/// struct (rather than a `BrainEngine` handle) is what makes the scanner a
/// pure function.
#[derive(Debug, Clone)]
pub struct FeatureScanInputs {
    pub health: HealthSnapshot,
    pub stats: BrainStatsSnapshot,
    /// Whether each secret env var is present AND non-empty. The caller reads
    /// the real environment; the scanner only sees booleans, keeping it pure.
    pub secret_present: fn(&str) -> bool,
    /// Configured git sync repo path (`config sync.repo_path`), if any.
    pub sync_repo: Option<String>,
    /// Current CLI version string (for the scan report `version` field).
    pub version: String,
}

/// The scan output. Mirrors TS `FeatureScanResult` minus the timestamp (which
/// is a side effect and is stamped at the CLI layer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureScanResult {
    pub version: String,
    pub brain_score: u32,
    pub recommendations: Vec<FeatureRecommendation>,
}

// ── Scanner (pure) ─────────────────────────────────────────────────────────

/// Count recipes whose secrets are NOT all configured. Pure over the injected
/// `secret_present` predicate.
fn unconfigured_recipes(secret_present: fn(&str) -> bool) -> Vec<&'static RecipeMeta> {
    RECIPE_META
        .iter()
        .filter(|r| !r.secrets.iter().all(|s| secret_present(s)))
        .collect()
}

/// Generate feature recommendations from injected brain state. This is a
/// verbatim port of `scanFeatures` in features.ts, minus the async engine
/// calls (which are hoisted into `FeatureScanInputs`).
///
/// Ordering matters — tests pin the exact sequence, mirroring TS push order:
///   P1 missing-embeddings, P1 dead-links,
///   then (only when page_count >= 3):
///   P2 zero-links, zero-timeline, low-coverage, no-integrations, no-sync.
pub fn recommend_features(inputs: &FeatureScanInputs) -> FeatureScanResult {
    let h = &inputs.health;
    let s = &inputs.stats;
    let mut recs: Vec<FeatureRecommendation> = Vec::new();

    // P1: Missing embeddings
    if h.missing_embeddings > 0 {
        recs.push(FeatureRecommendation {
            id: "missing-embeddings".into(),
            priority: FeaturePriority::P1,
            title: "Fix Missing Embeddings".into(),
            pitch: format!(
                "{} chunks invisible to semantic search. One command fixes it.",
                h.missing_embeddings
            ),
            command: "zbrain embed --stale".into(),
            auto_fixable: true,
        });
    }

    // P1: Dead links
    if h.dead_links > 0 {
        recs.push(FeatureRecommendation {
            id: "dead-links".into(),
            priority: FeaturePriority::P1,
            title: "Fix Dead Links".into(),
            pitch: format!("{} links pointing to non-existent pages.", h.dead_links),
            command: "zbrain check-backlinks fix".into(),
            auto_fixable: false,
        });
    }

    // P2 block — skip entirely if brain too new (< 3 pages).
    if s.page_count >= 3 {
        // Zero links (only when > 5 pages)
        if s.link_count == 0 && s.page_count > 5 {
            recs.push(FeatureRecommendation {
                id: "zero-links".into(),
                priority: FeaturePriority::P2,
                title: "Build Link Graph".into(),
                pitch: format!(
                    "{} pages but 0 links. Your brain is a flat file cabinet, not a knowledge graph.",
                    s.page_count
                ),
                command: "zbrain extract links".into(),
                auto_fixable: true,
            });
        }

        // Zero timeline (only when > 5 pages)
        if s.timeline_entry_count == 0 && s.page_count > 5 {
            recs.push(FeatureRecommendation {
                id: "zero-timeline".into(),
                priority: FeaturePriority::P2,
                title: "Extract Timeline".into(),
                pitch: "No structured timeline entries. Your brain can't answer \"when did X happen?\"".into(),
                command: "zbrain extract timeline".into(),
                auto_fixable: true,
            });
        }

        // Low embed coverage (0 < coverage < 0.9)
        if h.embed_coverage < 0.9 && h.embed_coverage > 0.0 {
            let pct = (h.embed_coverage * 100.0).round() as i64;
            recs.push(FeatureRecommendation {
                id: "low-coverage".into(),
                priority: FeaturePriority::P2,
                title: "Improve Embedding Coverage".into(),
                pitch: format!(
                    "{}% embed coverage. {} chunks invisible to semantic search.",
                    pct, h.missing_embeddings
                ),
                command: "zbrain embed --stale".into(),
                auto_fixable: true,
            });
        }

        // Unconfigured integrations
        let unconfigured = unconfigured_recipes(inputs.secret_present);
        if !unconfigured.is_empty() {
            let names = unconfigured
                .iter()
                .map(|r| r.name)
                .collect::<Vec<_>>()
                .join(", ");
            recs.push(FeatureRecommendation {
                id: "no-integrations".into(),
                priority: FeaturePriority::P2,
                title: "Set Up Integrations".into(),
                pitch: format!(
                    "{} integration recipes available but not configured: {}.",
                    unconfigured.len(),
                    names
                ),
                command: "zbrain integrations list".into(),
                auto_fixable: false,
            });
        }

        // No sync configured
        if inputs.sync_repo.as_deref().unwrap_or("").is_empty() {
            recs.push(FeatureRecommendation {
                id: "no-sync".into(),
                priority: FeaturePriority::P2,
                title: "Configure Sync".into(),
                pitch: "Brain not syncing from git. Changes in your repo don't reach your brain.".into(),
                command: "zbrain sync --repo <path>".into(),
                auto_fixable: false,
            });
        }
    }

    FeatureScanResult {
        version: inputs.version.clone(),
        brain_score: h.brain_score,
        recommendations: recs,
    }
}

// ── Decline filter (pure) ──────────────────────────────────────────────────

/// Decide whether a recommendation should be pitched given the decline ledger.
///
/// Verbatim port of `shouldPitch`:
///   * P1 recommendations are ALWAYS pitched.
///   * P2 is suppressed if it was declined at a version sharing the current
///     `major.minor` prefix.
pub fn should_pitch(
    rec: &FeatureRecommendation,
    offers: &FeatureOffers,
    current_version: &str,
) -> bool {
    if rec.priority == FeaturePriority::P1 {
        return true;
    }
    let major_minor = current_version
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    if let Some(declined) = offers.declined.get(&rec.id) {
        if declined.version.starts_with(&major_minor) {
            return false;
        }
    }
    true
}

/// Filter a scan's recommendations through the decline ledger. Convenience over
/// `should_pitch` for the common "which recs to show" call.
pub fn pitchable<'a>(
    scan: &'a FeatureScanResult,
    offers: &FeatureOffers,
) -> Vec<&'a FeatureRecommendation> {
    scan.recommendations
        .iter()
        .filter(|r| should_pitch(r, offers, &scan.version))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_secrets(_: &str) -> bool {
        false
    }
    fn all_secrets(_: &str) -> bool {
        true
    }

    fn base_inputs() -> FeatureScanInputs {
        FeatureScanInputs {
            health: HealthSnapshot {
                missing_embeddings: 0,
                dead_links: 0,
                embed_coverage: 1.0,
                brain_score: 100,
            },
            stats: BrainStatsSnapshot {
                page_count: 0,
                link_count: 0,
                timeline_entry_count: 0,
            },
            secret_present: all_secrets,
            sync_repo: Some("/repo".into()),
            version: "1.4.0".into(),
        }
    }

    fn ids(scan: &FeatureScanResult) -> Vec<&str> {
        scan.recommendations.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn healthy_brain_new_no_recommendations() {
        // All healthy + < 3 pages => nothing at all.
        let scan = recommend_features(&base_inputs());
        assert!(scan.recommendations.is_empty());
        assert_eq!(scan.brain_score, 100);
        assert_eq!(scan.version, "1.4.0");
    }

    #[test]
    fn p1_missing_embeddings_and_dead_links_always_emitted() {
        let mut i = base_inputs();
        i.health.missing_embeddings = 42;
        i.health.dead_links = 3;
        // page_count stays < 3 so no P2 leaks in.
        let scan = recommend_features(&i);
        assert_eq!(ids(&scan), vec!["missing-embeddings", "dead-links"]);
        assert_eq!(scan.recommendations[0].priority, FeaturePriority::P1);
        assert!(scan.recommendations[0].auto_fixable);
        assert!(!scan.recommendations[1].auto_fixable);
        assert!(scan.recommendations[0].pitch.contains("42 chunks"));
        assert!(scan.recommendations[1].pitch.contains("3 links"));
    }

    #[test]
    fn p2_block_skipped_below_three_pages() {
        let mut i = base_inputs();
        i.stats.page_count = 2; // < 3
        i.stats.link_count = 0;
        i.health.embed_coverage = 0.5;
        i.secret_present = no_secrets;
        i.sync_repo = None;
        let scan = recommend_features(&i);
        // Despite lots of "problems", the P2 block is entirely skipped.
        assert!(scan.recommendations.is_empty());
    }

    #[test]
    fn zero_links_needs_more_than_five_pages() {
        let mut i = base_inputs();
        i.stats.page_count = 4; // >=3 but <=5
        i.stats.link_count = 0;
        i.stats.timeline_entry_count = 5; // suppress zero-timeline
        let scan = recommend_features(&i);
        // 4 pages: P2 block runs, but zero-links requires > 5 pages.
        assert!(!ids(&scan).contains(&"zero-links"));
    }

    #[test]
    fn zero_links_and_timeline_emitted_above_five_pages() {
        let mut i = base_inputs();
        i.stats.page_count = 10;
        i.stats.link_count = 0;
        i.stats.timeline_entry_count = 0;
        let scan = recommend_features(&i);
        assert!(ids(&scan).contains(&"zero-links"));
        assert!(ids(&scan).contains(&"zero-timeline"));
        assert!(scan.recommendations.iter().any(|r| r.pitch.contains("10 pages but 0 links")));
    }

    #[test]
    fn low_coverage_boundary() {
        let mut i = base_inputs();
        i.stats.page_count = 10;
        i.stats.link_count = 5;
        i.stats.timeline_entry_count = 5;
        // coverage exactly 0.9 => NOT emitted (< 0.9 is strict)
        i.health.embed_coverage = 0.9;
        assert!(!ids(&recommend_features(&i)).contains(&"low-coverage"));
        // coverage 0.0 => NOT emitted (> 0.0 is strict; means "no data")
        i.health.embed_coverage = 0.0;
        assert!(!ids(&recommend_features(&i)).contains(&"low-coverage"));
        // coverage 0.85 => emitted with rounded pct
        i.health.embed_coverage = 0.85;
        i.health.missing_embeddings = 7;
        let scan = recommend_features(&i);
        let rec = scan.recommendations.iter().find(|r| r.id == "low-coverage").unwrap();
        assert!(rec.pitch.starts_with("85% embed coverage. 7 chunks"));
    }

    #[test]
    fn unconfigured_integrations_lists_missing_recipes() {
        let mut i = base_inputs();
        i.stats.page_count = 10;
        i.stats.link_count = 5;
        i.stats.timeline_entry_count = 5;
        // Only GMAIL configured; the other 6 recipes are unconfigured.
        fn only_gmail(s: &str) -> bool {
            s == "GMAIL_APP_PASSWORD"
        }
        i.secret_present = only_gmail;
        let scan = recommend_features(&i);
        let rec = scan.recommendations.iter().find(|r| r.id == "no-integrations").unwrap();
        assert!(rec.pitch.contains("6 integration recipes"));
        assert!(!rec.pitch.contains("Email to Brain")); // gmail configured -> excluded
        assert!(rec.pitch.contains("Calendar Sync"));
    }

    #[test]
    fn all_integrations_configured_no_integration_rec() {
        let mut i = base_inputs();
        i.stats.page_count = 10;
        i.stats.link_count = 5;
        i.stats.timeline_entry_count = 5;
        i.secret_present = all_secrets;
        assert!(!ids(&recommend_features(&i)).contains(&"no-integrations"));
    }

    #[test]
    fn no_sync_when_repo_empty_or_absent() {
        let mut i = base_inputs();
        i.stats.page_count = 10;
        i.stats.link_count = 5;
        i.stats.timeline_entry_count = 5;
        i.sync_repo = None;
        assert!(ids(&recommend_features(&i)).contains(&"no-sync"));
        i.sync_repo = Some(String::new());
        assert!(ids(&recommend_features(&i)).contains(&"no-sync"));
        i.sync_repo = Some("/some/repo".into());
        assert!(!ids(&recommend_features(&i)).contains(&"no-sync"));
    }

    #[test]
    fn full_ordering_pinned() {
        // Trigger every recommendation, assert exact push order.
        let mut i = base_inputs();
        i.health.missing_embeddings = 5;
        i.health.dead_links = 2;
        i.health.embed_coverage = 0.5;
        i.stats.page_count = 10;
        i.stats.link_count = 0;
        i.stats.timeline_entry_count = 0;
        i.secret_present = no_secrets;
        i.sync_repo = None;
        let scan = recommend_features(&i);
        assert_eq!(
            ids(&scan),
            vec![
                "missing-embeddings",
                "dead-links",
                "zero-links",
                "zero-timeline",
                "low-coverage",
                "no-integrations",
                "no-sync",
            ]
        );
    }

    #[test]
    fn should_pitch_p1_always() {
        let rec = FeatureRecommendation {
            id: "missing-embeddings".into(),
            priority: FeaturePriority::P1,
            title: "t".into(),
            pitch: "p".into(),
            command: "c".into(),
            auto_fixable: true,
        };
        let mut offers = FeatureOffers::default();
        offers.declined.insert(
            "missing-embeddings".into(),
            OfferStamp { at: "2026-01-01".into(), version: "1.4.0".into() },
        );
        // Even when declined at same version, P1 is always pitched.
        assert!(should_pitch(&rec, &offers, "1.4.0"));
    }

    #[test]
    fn should_pitch_p2_declined_same_major_minor_suppressed() {
        let rec = FeatureRecommendation {
            id: "no-sync".into(),
            priority: FeaturePriority::P2,
            title: "t".into(),
            pitch: "p".into(),
            command: "c".into(),
            auto_fixable: false,
        };
        let mut offers = FeatureOffers::default();
        offers.declined.insert(
            "no-sync".into(),
            OfferStamp { at: "2026-01-01".into(), version: "1.4.9".into() },
        );
        // Declined at 1.4.9, current 1.4.0 => same major.minor "1.4" => suppress.
        assert!(!should_pitch(&rec, &offers, "1.4.0"));
        // Current 1.5.0 => different major.minor => pitch again.
        assert!(should_pitch(&rec, &offers, "1.5.0"));
        // Never declined => pitch.
        assert!(should_pitch(&rec, &FeatureOffers::default(), "1.4.0"));
    }

    #[test]
    fn pitchable_filters_declined_p2_but_keeps_p1() {
        let scan = FeatureScanResult {
            version: "1.4.0".into(),
            brain_score: 50,
            recommendations: vec![
                FeatureRecommendation {
                    id: "dead-links".into(),
                    priority: FeaturePriority::P1,
                    title: "t".into(),
                    pitch: "p".into(),
                    command: "c".into(),
                    auto_fixable: false,
                },
                FeatureRecommendation {
                    id: "no-sync".into(),
                    priority: FeaturePriority::P2,
                    title: "t".into(),
                    pitch: "p".into(),
                    command: "c".into(),
                    auto_fixable: false,
                },
            ],
        };
        let mut offers = FeatureOffers::default();
        offers.declined.insert(
            "no-sync".into(),
            OfferStamp { at: "2026-01-01".into(), version: "1.4.0".into() },
        );
        let shown = pitchable(&scan, &offers);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].id, "dead-links");
    }

    #[test]
    fn priority_serde_roundtrips_as_number() {
        let json = serde_json::to_string(&FeaturePriority::P1).unwrap();
        assert_eq!(json, "1");
        let p: FeaturePriority = serde_json::from_str("2").unwrap();
        assert_eq!(p, FeaturePriority::P2);
    }

    #[test]
    fn recommendation_json_shape_matches_ts() {
        let rec = FeatureRecommendation {
            id: "missing-embeddings".into(),
            priority: FeaturePriority::P1,
            title: "Fix Missing Embeddings".into(),
            pitch: "10 chunks invisible to semantic search. One command fixes it.".into(),
            command: "zbrain embed --stale".into(),
            auto_fixable: true,
        };
        let v: serde_json::Value = serde_json::to_value(&rec).unwrap();
        assert_eq!(v["id"], "missing-embeddings");
        assert_eq!(v["priority"], 1);
        assert_eq!(v["auto_fixable"], true);
    }
}
