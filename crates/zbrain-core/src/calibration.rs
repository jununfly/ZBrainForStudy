//! Calibration algorithm layer (Part11 1-3, Phase 1 — zero-dependency pure
//! functions ported from `src/core/calibration/*.ts`).
//!
//! Phase 1 scope: the pure, side-effect-free formatters/parsers that need no
//! `BrainEngine` and no LLM. These mirror the TS templates verbatim so the
//! web admin / CLI can call Rust instead of the TS module. Engine-backed and
//! LLM-backed calibration functions (e.g. `forecastForTake`, `gateVoice`,
//! `runAbTrial`) are Phase 2 and may surface as KNOWN-GAPs.
//!
//! Note: no roadmap node number is referenced here on purpose — the Part11
//! roadmap JSON is a temporary working file and will be cleared on completion,
//! so comments must stay self-explanatory.

// ── voice-gate fallback templates (templates.ts) ──────────────────────────

/// Voice-gate modes. Every mode MUST have a template in this module; the
/// web admin pins parity against this list.
pub const VOICE_GATE_MODES: &[&str] = &[
    "pattern_statement",
    "nudge",
    "forecast_blurb",
    "dashboard_caption",
    "morning_pulse",
];

#[derive(Debug, Clone, PartialEq)]
pub struct PatternStatementSlots {
    pub domain: String,
    pub n_right: u32,
    pub n_wrong: u32,
    /// Optional one-word direction tag e.g. "over-confident" / "late".
    pub direction: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NudgeSlots {
    pub domain: String,
    pub conviction: f64,
    pub n_recent_misses: u32,
    pub n_recent_total: u32,
    pub hush_pattern: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForecastBlurbSlots {
    pub domain: String,
    pub conviction: f64,
    pub bucket_brier: f64,
    pub overall_brier: f64,
    pub bucket_n: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardCaptionSlots {
    /// e.g. "Brier trend" or "Per-domain accuracy".
    pub surface: String,
    /// Single short fact for the chart caption.
    pub fact: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MorningPulseTrend {
    Improving,
    Declining,
    Stable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MorningPulseSlots {
    pub brier: f64,
    pub trend: MorningPulseTrend,
    pub top_pattern: String,
}

/// Pattern statement template — what `calibration_profile` writes when the
/// voice gate fails on an LLM narrative.
pub fn pattern_statement_template(s: &PatternStatementSlots) -> String {
    let total = s.n_right + s.n_wrong;
    if total == 0 {
        return format!(
            "Not enough resolved {} calls yet to spot a pattern.",
            s.domain
        );
    }
    let direction = match &s.direction {
        Some(d) => d.clone(),
        None => {
            if s.n_wrong > s.n_right {
                "mixed"
            } else {
                "mostly right"
            }
            .to_string()
        }
    };
    format!(
        "Your {} calls have a {} record — {} of {} held up.",
        s.domain, direction, s.n_right, total
    )
}

/// Nudge template — stderr line on sync after a take is committed.
pub fn nudge_template(s: &NudgeSlots) -> String {
    format!(
        "[zbrain] You just committed a {} take at conviction {:.2}. \
         Recent record on similar calls: {} of {} missed. \
         Hush this pattern for 14 days: zbrain takes nudge --hush {}",
        s.domain, s.conviction, s.n_recent_misses, s.n_recent_total, s.hush_pattern
    )
}

/// Inline forecast on a new take (queue + takes show).
pub fn forecast_blurb_template(s: &ForecastBlurbSlots) -> String {
    if s.bucket_n < 5 {
        return format!(
            "Forecast unavailable: only {} resolved {} takes at this conviction yet.",
            s.bucket_n, s.domain
        );
    }
    let note = if s.bucket_brier > s.overall_brier {
        "worse than your average"
    } else {
        "on par with your average"
    };
    format!(
        "Predicted Brier in {} at conviction {:.2}: {:.2} ({}, n={}).",
        s.domain, s.conviction, s.bucket_brier, note, s.bucket_n
    )
}

/// Dashboard chart caption.
pub fn dashboard_caption_template(s: &DashboardCaptionSlots) -> String {
    format!("{}: {}", s.surface, s.fact)
}

/// Recall morning pulse Brier+pattern line.
pub fn morning_pulse_template(s: &MorningPulseSlots) -> String {
    let trend_word = match s.trend {
        MorningPulseTrend::Improving => "improving",
        MorningPulseTrend::Declining => "declining",
        MorningPulseTrend::Stable => "stable",
    };
    let tail = if !s.top_pattern.is_empty() {
        format!("Top pattern: {}.", s.top_pattern)
    } else {
        String::new()
    };
    format!("Brier {:.2} ({}). {}", s.brier, trend_word, tail)
}

// ── recall calibration footer (recall-footer.ts) ──────────────────────────

/// The three fields `build_recall_calibration_footer` actually reads from a
/// calibration profile. Kept separate from the DB-layer `CalibrationProfileRow`
/// (which lacks `total_resolved`) so this pure formatter has no engine
/// dependency; the Phase-2 caller constructs it from the DB row + a count.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallFooterProfile {
    pub total_resolved: u32,
    pub brier: Option<f64>,
    pub pattern_statements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbandonedThreadSummary {
    pub claim: String,
    pub months_silent: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecallFooterOpts {
    pub profile: Option<RecallFooterProfile>,
    pub abandoned_threads: Vec<AbandonedThreadSummary>,
    /// Width hint for column alignment on threads. Pass 0 to use the default 50.
    pub thread_column_width: usize,
}

/// Pure formatter for the `zbrain recall` morning-pulse calibration block.
/// Cold-brain branch returns an empty string when no profile or insufficient
/// resolved takes.
pub fn build_recall_calibration_footer(opts: &RecallFooterOpts) -> String {
    let profile = match &opts.profile {
        Some(p) => p,
        None => return String::new(),
    };
    if profile.total_resolved < 5 {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push("Calibration this quarter:".to_string());

    if let Some(brier) = profile.brier {
        lines.push(format!("  Brier {:.2} {}", brier, trend_note(brier)));
    }
    for p in profile.pattern_statements.iter().take(4) {
        lines.push(format!("  {}", p));
    }

    if !opts.abandoned_threads.is_empty() {
        lines.push(String::new());
        lines.push("Threads you opened and never came back to:".to_string());
        let col_width = if opts.thread_column_width == 0 {
            50
        } else {
            opts.thread_column_width
        };
        for t in opts.abandoned_threads.iter().take(5) {
            let claim = if t.claim.chars().count() > col_width {
                let mut truncated: String = t.claim.chars().take(col_width - 1).collect();
                truncated.push('…');
                truncated
            } else {
                t.claim.clone()
            };
            let padded = format!("{:<width$}", claim, width = col_width);
            lines.push(format!("  · {} ({} months silent)", padded, t.months_silent));
        }
    }

    lines.join("\n")
}

fn trend_note(brier: f64) -> &'static str {
    // Map Brier to a conversational anchor. No history yet so we describe the
    // absolute value rather than trend.
    if brier <= 0.1 {
        "(strong calibration)."
    } else if brier <= 0.2 {
        "(solid)."
    } else if brier <= 0.25 {
        "(near baseline)."
    } else {
        "(worse than always-50% baseline — review your high-conviction calls)."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_statement_zero_total() {
        let s = PatternStatementSlots {
            domain: "market".into(),
            n_right: 0,
            n_wrong: 0,
            direction: None,
        };
        assert_eq!(
            pattern_statement_template(&s),
            "Not enough resolved market calls yet to spot a pattern."
        );
    }

    #[test]
    fn pattern_statement_default_direction() {
        // n_wrong (3) > n_right (2) -> "mixed"
        let s = PatternStatementSlots {
            domain: "market".into(),
            n_right: 2,
            n_wrong: 3,
            direction: None,
        };
        assert_eq!(
            pattern_statement_template(&s),
            "Your market calls have a mixed record — 2 of 5 held up."
        );
        // n_right > n_wrong -> "mostly right"
        let s2 = PatternStatementSlots {
            domain: "market".into(),
            n_right: 4,
            n_wrong: 1,
            direction: None,
        };
        assert_eq!(
            pattern_statement_template(&s2),
            "Your market calls have a mostly right record — 4 of 5 held up."
        );
    }

    #[test]
    fn pattern_statement_explicit_direction() {
        let s = PatternStatementSlots {
            domain: "macro".into(),
            n_right: 5,
            n_wrong: 1,
            direction: Some("late".into()),
        };
        assert_eq!(
            pattern_statement_template(&s),
            "Your macro calls have a late record — 5 of 6 held up."
        );
    }

    #[test]
    fn nudge_template_exact() {
        let s = NudgeSlots {
            domain: "team-exec".into(),
            conviction: 0.83,
            n_recent_misses: 2,
            n_recent_total: 9,
            hush_pattern: "team-exec".into(),
        };
        assert_eq!(
            nudge_template(&s),
            "[zbrain] You just committed a team-exec take at conviction 0.83. \
             Recent record on similar calls: 2 of 9 missed. \
             Hush this pattern for 14 days: zbrain takes nudge --hush team-exec"
        );
    }

    #[test]
    fn forecast_blurb_unavailable_when_small_bucket() {
        let s = ForecastBlurbSlots {
            domain: "market".into(),
            conviction: 0.7,
            bucket_brier: 0.2,
            overall_brier: 0.18,
            bucket_n: 4,
        };
        assert_eq!(
            forecast_blurb_template(&s),
            "Forecast unavailable: only 4 resolved market takes at this conviction yet."
        );
    }

    #[test]
    fn forecast_blurb_worse_and_on_par() {
        let worse = ForecastBlurbSlots {
            domain: "market".into(),
            conviction: 0.7,
            bucket_brier: 0.25,
            overall_brier: 0.18,
            bucket_n: 12,
        };
        assert_eq!(
            forecast_blurb_template(&worse),
            "Predicted Brier in market at conviction 0.70: 0.25 (worse than your average, n=12)."
        );
        let par = ForecastBlurbSlots {
            domain: "market".into(),
            conviction: 0.7,
            bucket_brier: 0.18,
            overall_brier: 0.18,
            bucket_n: 12,
        };
        assert_eq!(
            forecast_blurb_template(&par),
            "Predicted Brier in market at conviction 0.70: 0.18 (on par with your average, n=12)."
        );
    }

    #[test]
    fn dashboard_caption() {
        let s = DashboardCaptionSlots {
            surface: "Brier trend".into(),
            fact: "down 0.04 this quarter".into(),
        };
        assert_eq!(dashboard_caption_template(&s), "Brier trend: down 0.04 this quarter");
    }

    #[test]
    fn morning_pulse_trends() {
        let improving = MorningPulseSlots {
            brier: 0.18,
            trend: MorningPulseTrend::Improving,
            top_pattern: "over-confident on execution".into(),
        };
        assert_eq!(
            morning_pulse_template(&improving),
            "Brier 0.18 (improving). Top pattern: over-confident on execution."
        );
        let stable = MorningPulseSlots {
            brier: 0.22,
            trend: MorningPulseTrend::Stable,
            top_pattern: String::new(),
        };
        assert_eq!(morning_pulse_template(&stable), "Brier 0.22 (stable). ");
        let declining = MorningPulseSlots {
            brier: 0.3,
            trend: MorningPulseTrend::Declining,
            top_pattern: String::new(),
        };
        assert_eq!(morning_pulse_template(&declining), "Brier 0.30 (declining). ");
    }

    #[test]
    fn recall_footer_null_profile() {
        let opts = RecallFooterOpts {
            profile: None,
            abandoned_threads: vec![],
            thread_column_width: 0,
        };
        assert_eq!(build_recall_calibration_footer(&opts), "");
    }

    #[test]
    fn recall_footer_too_few_resolved() {
        let opts = RecallFooterOpts {
            profile: Some(RecallFooterProfile {
                total_resolved: 3,
                brier: Some(0.18),
                pattern_statements: vec![],
            }),
            abandoned_threads: vec![],
            thread_column_width: 0,
        };
        assert_eq!(build_recall_calibration_footer(&opts), "");
    }

    #[test]
    fn recall_footer_full_render() {
        // Use deterministic claim lengths so column padding/truncation is
        // exactly assertable: thread1 = 10 chars (pads to 50), thread2 = 60
        // chars (truncates to 49 + ellipsis = 50, no pad).
        let opts = RecallFooterOpts {
            profile: Some(RecallFooterProfile {
                total_resolved: 42,
                brier: Some(0.18),
                pattern_statements: vec![
                    "Right on early-stage tactics".into(),
                    "Late on macro by 18 months".into(),
                ],
            }),
            abandoned_threads: vec![
                AbandonedThreadSummary {
                    claim: "a".repeat(10),
                    months_silent: 17,
                },
                AbandonedThreadSummary {
                    claim: "b".repeat(60),
                    months_silent: 12,
                },
            ],
            thread_column_width: 50,
        };
        let out = build_recall_calibration_footer(&opts);
        // Build the expected string with the same padding/truncation logic so
        // the assertion stays exact without hand-counting column spaces.
        let thread1 = format!("  · {:<50} (17 months silent)", "a".repeat(10));
        let truncated2 = format!("{}{}", "b".repeat(49), "…"); // 49 + ellipsis = 50 chars
        let thread2 = format!("  · {:<50} (12 months silent)", truncated2);
        let expected = format!(
            "Calibration this quarter:\n  Brier 0.18 (solid).\n  Right on early-stage tactics\n  Late on macro by 18 months\n\nThreads you opened and never came back to:\n{}\n{}",
            thread1, thread2
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn voice_gate_modes_parity() {
        // Every mode in VOICE_GATE_MODES must have a template function; this
        // guards the "mode parity" contract from the TS module.
        assert_eq!(VOICE_GATE_MODES.len(), 5);
        assert!(VOICE_GATE_MODES.contains(&"pattern_statement"));
        assert!(VOICE_GATE_MODES.contains(&"nudge"));
        assert!(VOICE_GATE_MODES.contains(&"forecast_blurb"));
        assert!(VOICE_GATE_MODES.contains(&"dashboard_caption"));
        assert!(VOICE_GATE_MODES.contains(&"morning_pulse"));
    }
}
