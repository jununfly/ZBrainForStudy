// zbrain-svg — server-rendered calibration SVG charts.
// Zero-dependency crate. Pure functions: data → SVG string.

/// Escape XML special characters for safe SVG text / attribute content.
pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Design token colors — must match admin SPA CSS.
pub mod tokens {
    pub const BG_PRIMARY: &str = "#0a0a0f";
    pub const BG_SECONDARY: &str = "#14141f";
    pub const TEXT_PRIMARY: &str = "#e0e0e0";
    pub const TEXT_SECONDARY: &str = "#888";
    pub const TEXT_MUTED: &str = "#777";
    pub const ACCENT: &str = "#3b82f6";
}

/// Generate a placeholder SVG when there is no data to chart.
pub fn svg_empty(width: u32, height: u32, message: &str) -> String {
    let msg = escape_xml(message);
    let w = width;
    let h = height;
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img" aria-label="Empty chart">
  <rect width="{w}" height="{h}" fill="{bg}"/>
  <text x="{cx}" y="{cy}" font-size="12" fill="{muted}" text-anchor="middle">{msg}</text>
</svg>"#,
        w = w,
        h = h,
        bg = tokens::BG_PRIMARY,
        muted = tokens::TEXT_MUTED,
        cx = w / 2,
        cy = h / 2,
        msg = msg,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_xml_ampersand() {
        assert_eq!(escape_xml("a & b"), "a &amp; b");
    }

    #[test]
    fn escape_xml_less_than() {
        assert_eq!(escape_xml("a < b"), "a &lt; b");
    }

    #[test]
    fn escape_xml_greater_than() {
        assert_eq!(escape_xml("a > b"), "a &gt; b");
    }

    #[test]
    fn escape_xml_double_quote() {
        assert_eq!(escape_xml(r#"she said "hi""#), "she said &quot;hi&quot;");
    }

    #[test]
    fn escape_xml_single_quote() {
        assert_eq!(escape_xml("it's"), "it&#39;s");
    }

    #[test]
    fn escape_xml_multiple_specials() {
        assert_eq!(
            escape_xml(r#"<a href="x" title='y'>foo & bar</a>"#),
            "&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;foo &amp; bar&lt;/a&gt;"
        );
    }

    #[test]
    fn escape_xml_empty() {
        assert_eq!(escape_xml(""), "");
    }

    #[test]
    fn escape_xml_plain_text() {
        assert_eq!(escape_xml("hello world"), "hello world");
    }

    // ─── svg_empty ────────────────────────────────────────────────

    #[test]
    fn svg_empty_contains_message() {
        let svg = svg_empty(600, 80, "No data yet");
        assert!(svg.contains("No data yet"));
    }

    #[test]
    fn svg_empty_is_valid_svg() {
        let svg = svg_empty(400, 200, "Empty");
        assert!(svg.starts_with("<svg "));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains(r#"xmlns="http://www.w3.org/2000/svg""#));
    }

    #[test]
    fn svg_empty_has_correct_dimensions() {
        let svg = svg_empty(300, 150, "N/A");
        assert!(svg.contains(r#"width="300""#));
        assert!(svg.contains(r#"height="150""#));
        assert!(svg.contains(r#"viewBox="0 0 300 150""#));
    }

    #[test]
    fn svg_empty_has_bg_and_role() {
        let svg = svg_empty(100, 50, "x");
        assert!(svg.contains(r#"role="img""#));
        assert!(svg.contains(tokens::BG_PRIMARY));
    }

    #[test]
    fn svg_empty_escapes_message() {
        let svg = svg_empty(200, 60, "a < b & c");
        assert!(svg.contains("a &lt; b &amp; c"));
        assert!(!svg.contains("a < b & c"));
    }

    // ─── render_brier_trend ───────────────────────────────────────

    #[test]
    fn brier_trend_empty_returns_svg_empty() {
        let opts = BrierTrendOpts {
            series: vec![],
            ..Default::default()
        };
        let svg = render_brier_trend(&opts);
        assert!(svg.contains("No Brier-trend data"));
    }

    #[test]
    fn brier_trend_single_point() {
        let opts = BrierTrendOpts {
            series: vec![BrierTrendPoint {
                date: "2025-01-01".into(),
                brier: 0.15,
            }],
            ..Default::default()
        };
        let svg = render_brier_trend(&opts);
        // Single point still renders polyline; no date labels (need 2+ points)
        assert!(svg.contains("<polyline"));
        assert!(!svg.contains("2025-01-01")); // no date label for single point
    }

    #[test]
    fn brier_trend_multiple_points_has_baseline() {
        let opts = BrierTrendOpts {
            series: vec![
                BrierTrendPoint {
                    date: "2025-01-01".into(),
                    brier: 0.3,
                },
                BrierTrendPoint {
                    date: "2025-01-02".into(),
                    brier: 0.1,
                },
            ],
            ..Default::default()
        };
        let svg = render_brier_trend(&opts);
        // Baseline at Brier=0.25
        assert!(svg.contains("stroke-dasharray"));
        assert!(svg.contains("\"2,3\""));
    }

    #[test]
    fn brier_trend_y_axis_labels() {
        let opts = BrierTrendOpts {
            series: vec![
                BrierTrendPoint {
                    date: "2025-06-01".into(),
                    brier: 0.0,
                },
                BrierTrendPoint {
                    date: "2025-06-02".into(),
                    brier: 0.4,
                },
            ],
            ..Default::default()
        };
        let svg = render_brier_trend(&opts);
        assert!(svg.contains("0.0"));
        assert!(svg.contains("0.2"));
        assert!(svg.contains("0.4"));
    }

    #[test]
    fn brier_trend_custom_dimensions() {
        let opts = BrierTrendOpts {
            series: vec![BrierTrendPoint {
                date: "2025-01-01".into(),
                brier: 0.2,
            }],
            width: Some(400),
            height: Some(120),
        };
        let svg = render_brier_trend(&opts);
        assert!(svg.contains(r#"width="400""#));
        assert!(svg.contains(r#"height="120""#));
    }

    #[test]
    fn brier_trend_has_title() {
        let opts = BrierTrendOpts {
            series: vec![BrierTrendPoint {
                date: "2025-01-01".into(),
                brier: 0.1,
            }],
            ..Default::default()
        };
        let svg = render_brier_trend(&opts);
        assert!(svg.contains("Brier (lower is better)"));
    }

    // ─── render_domain_bars ────────────────────────────────────────

    #[test]
    fn domain_bars_empty_returns_svg_empty() {
        let opts = DomainBarsOpts {
            bars: vec![],
            ..Default::default()
        };
        let svg = render_domain_bars(&opts);
        assert!(svg.contains("No per-domain scorecard data"));
    }

    #[test]
    fn domain_bars_renders_labels() {
        let opts = DomainBarsOpts {
            bars: vec![
                DomainBar {
                    label: "macro tech".into(),
                    accuracy: 0.75,
                    n: 42,
                },
                DomainBar {
                    label: "health".into(),
                    accuracy: 0.5,
                    n: 20,
                },
            ],
            ..Default::default()
        };
        let svg = render_domain_bars(&opts);
        assert!(svg.contains("macro tech"));
        assert!(svg.contains("health"));
        assert!(svg.contains("75%"));
        assert!(svg.contains("50%"));
        assert!(svg.contains("n=42"));
        assert!(svg.contains("n=20"));
    }

    #[test]
    fn domain_bars_custom_dimensions() {
        let opts = DomainBarsOpts {
            bars: vec![DomainBar {
                label: "x".into(),
                accuracy: 0.5,
                n: 1,
            }],
            width: Some(400),
            row_height: Some(32),
        };
        let svg = render_domain_bars(&opts);
        assert!(svg.contains(r#"width="400""#));
    }

    #[test]
    fn domain_bars_has_title() {
        let opts = DomainBarsOpts {
            bars: vec![DomainBar {
                label: "test".into(),
                accuracy: 0.8,
                n: 10,
            }],
            ..Default::default()
        };
        let svg = render_domain_bars(&opts);
        assert!(svg.contains("Per-domain accuracy"));
    }

    #[test]
    fn domain_bars_escapes_labels() {
        let opts = DomainBarsOpts {
            bars: vec![DomainBar {
                label: "<script>alert(1)</script>".into(),
                accuracy: 0.5,
                n: 1,
            }],
            ..Default::default()
        };
        let svg = render_domain_bars(&opts);
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;script&gt;"));
    }

    // ─── render_abandoned_threads_card ─────────────────────────────

    #[test]
    fn abandoned_threads_empty_returns_svg_empty() {
        let svg = render_abandoned_threads_card(&[], 600);
        assert!(svg.contains("No abandoned high-conviction threads"));
    }

    #[test]
    fn abandoned_threads_renders_claims() {
        let threads = vec![AbandonedThread {
            take_id: 1,
            page_slug: "macro-tech".into(),
            claim: "AI will surpass human intelligence by 2030".into(),
            months_silent: 6,
            conviction: 0.85,
            revisit_href: None,
        }];
        let svg = render_abandoned_threads_card(&threads, 600);
        assert!(svg.contains("AI will surpass human intelligence by 2030"));
        assert!(svg.contains("conviction 0.85"));
        assert!(svg.contains("6 months silent"));
        assert!(svg.contains("revisit now"));
    }

    #[test]
    fn abandoned_threads_truncates_long_claims() {
        let long = "a".repeat(80);
        let threads = vec![AbandonedThread {
            take_id: 1,
            page_slug: "test".into(),
            claim: long.clone(),
            months_silent: 1,
            conviction: 0.5,
            revisit_href: None,
        }];
        let svg = render_abandoned_threads_card(&threads, 600);
        // Should contain truncated version with ellipsis
        assert!(svg.contains("…"));
        assert!(!svg.contains(&long)); // full text not in SVG
    }

    #[test]
    fn abandoned_threads_escapes_html_in_claim() {
        let threads = vec![AbandonedThread {
            take_id: 1,
            page_slug: "test".into(),
            claim: "<b>bold</b>".into(),
            months_silent: 1,
            conviction: 0.5,
            revisit_href: None,
        }];
        let svg = render_abandoned_threads_card(&threads, 600);
        assert!(!svg.contains("<b>"));
        assert!(svg.contains("&lt;b&gt;"));
    }

    // ─── render_pattern_statements_card ───────────────────────────

    #[test]
    fn pattern_statements_empty_returns_svg_empty() {
        let svg = render_pattern_statements_card(&[], 600);
        assert!(svg.contains("No active patterns yet"));
    }

    #[test]
    fn pattern_statements_renders_text() {
        let stmts = vec![
            PatternStatementsCardItem {
                text: "Overconfident on macro predictions".into(),
                drill_href: None,
            },
            PatternStatementsCardItem {
                text: "Hedge too aggressive".into(),
                drill_href: Some("/admin/calibration/pattern/2".into()),
            },
        ];
        let svg = render_pattern_statements_card(&stmts, 600);
        assert!(svg.contains("Overconfident on macro predictions"));
        assert!(svg.contains("Hedge too aggressive"));
        assert!(svg.contains("drill down"));
    }

    #[test]
    fn pattern_statements_truncates_long_text() {
        let long = "a".repeat(100);
        let stmts = vec![PatternStatementsCardItem {
            text: long.clone(),
            drill_href: None,
        }];
        let svg = render_pattern_statements_card(&stmts, 600);
        assert!(svg.contains("…"));
        assert!(!svg.contains(&long));
    }

    #[test]
    fn pattern_statements_escapes_xml() {
        let stmts = vec![PatternStatementsCardItem {
            text: "<img onerror=alert(1)>".into(),
            drill_href: None,
        }];
        let svg = render_pattern_statements_card(&stmts, 600);
        assert!(!svg.contains("<img"));
        assert!(svg.contains("&lt;img"));
    }

    #[test]
    fn pattern_statements_uses_custom_href() {
        let stmts = vec![PatternStatementsCardItem {
            text: "test".into(),
            drill_href: Some("/custom/path".into()),
        }];
        let svg = render_pattern_statements_card(&stmts, 600);
        assert!(svg.contains("/custom/path"));
    }

    #[test]
    fn pattern_statements_default_href_is_1_based() {
        let stmts = vec![
            PatternStatementsCardItem {
                text: "first".into(),
                drill_href: None,
            },
            PatternStatementsCardItem {
                text: "second".into(),
                drill_href: None,
            },
        ];
        let svg = render_pattern_statements_card(&stmts, 600);
        assert!(svg.contains("/admin/calibration/pattern/1"));
        assert!(svg.contains("/admin/calibration/pattern/2"));
    }
}

// ─── Types ─────────────────────────────────────────────────────────

/// A single data point on the Brier trend sparkline.
#[derive(Debug, Clone)]
pub struct BrierTrendPoint {
    pub date: String,
    pub brier: f64,
}

/// Options for `render_brier_trend`.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct BrierTrendOpts {
    pub series: Vec<BrierTrendPoint>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}


/// Render a Brier-score trend sparkline SVG.
pub fn render_brier_trend(opts: &BrierTrendOpts) -> String {
    let w = opts.width.unwrap_or(600);
    let h = opts.height.unwrap_or(180);
    let pad_l: f64 = 40.0;
    let pad_r: f64 = 16.0;
    let pad_t: f64 = 20.0;
    let pad_b: f64 = 28.0;
    let plot_w = w as f64 - pad_l - pad_r;
    let plot_h = h as f64 - pad_t - pad_b;

    if opts.series.is_empty() {
        return svg_empty(w, h, "No Brier-trend data yet (need 5+ resolved takes)");
    }

    let y_max: f64 = 0.4;

    let x_scale = |i: usize| -> f64 {
        if opts.series.len() == 1 {
            pad_l + plot_w / 2.0
        } else {
            pad_l + (i as f64 / (opts.series.len() - 1) as f64) * plot_w
        }
    };
    let y_scale = |brier: f64| -> f64 {
        pad_t + plot_h - (brier.min(y_max) / y_max) * plot_h
    };

    let points: Vec<String> = opts
        .series
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{:.1},{:.1}", x_scale(i), y_scale(p.brier)))
        .collect();
    let points_str = points.join(" ");

    let baseline_y = format!("{:.1}", y_scale(0.25));

    let mut labels: Vec<String> = Vec::new();
    if opts.series.len() >= 2 {
        let first = &opts.series[0];
        let last = &opts.series[opts.series.len() - 1];
        labels.push(format!(
            r#"<text x="{pad_l}" y="{yb}" font-size="11" fill="{muted}">{d}</text>"#,
            pad_l = pad_l,
            yb = h as f64 - 8.0,
            muted = tokens::TEXT_MUTED,
            d = escape_xml(&first.date),
        ));
        labels.push(format!(
            r#"<text x="{xr}" y="{yb}" font-size="11" fill="{muted}" text-anchor="end">{d}</text>"#,
            xr = w as f64 - pad_r,
            yb = h as f64 - 8.0,
            muted = tokens::TEXT_MUTED,
            d = escape_xml(&last.date),
        ));
    }
    for y_val in &[0.0_f64, 0.2, 0.4] {
        labels.push(format!(
            r#"<text x="{x}" y="{y}" font-size="11" fill="{muted}" text-anchor="end">{val}</text>"#,
            x = pad_l - 6.0,
            y = y_scale(*y_val) + 4.0,
            muted = tokens::TEXT_MUTED,
            val = format_args!("{:.1}", y_val),
        ));
    }

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img" aria-label="Brier trend">
  <rect width="{w}" height="{h}" fill="{bg}"/>
  <text x="{pad_l}" y="14" font-size="12" fill="{text2}">Brier (lower is better)</text>
  <line x1="{pad_l}" y1="{by}" x2="{xr}" y2="{by}" stroke="{muted}" stroke-dasharray="2,3" stroke-width="1"/>
  <polyline points="{pts}" fill="none" stroke="{accent}" stroke-width="2"/>
  {labels}
</svg>"#,
        w = w,
        h = h,
        bg = tokens::BG_PRIMARY,
        text2 = tokens::TEXT_SECONDARY,
        pad_l = pad_l,
        by = baseline_y,
        xr = w as f64 - pad_r,
        muted = tokens::TEXT_MUTED,
        accent = tokens::ACCENT,
        pts = points_str,
        labels = labels.join("\n  "),
    )
}

// ─── DomainBars ────────────────────────────────────────────────────

/// A single per-domain accuracy bar.
#[derive(Debug, Clone)]
pub struct DomainBar {
    pub label: String,
    pub accuracy: f64,
    pub n: u32,
}

/// Options for `render_domain_bars`.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct DomainBarsOpts {
    pub bars: Vec<DomainBar>,
    pub width: Option<u32>,
    pub row_height: Option<u32>,
}


/// Render per-domain accuracy horizontal bar chart SVG.
pub fn render_domain_bars(opts: &DomainBarsOpts) -> String {
    let w = opts.width.unwrap_or(600);
    let row_h = opts.row_height.unwrap_or(28);
    let pad_l: u32 = 140;
    let pad_r: u32 = 50;
    let pad_t: u32 = 24;
    let h = pad_t + opts.bars.len() as u32 * row_h + 12;

    if opts.bars.is_empty() {
        return svg_empty(w, 60, "No per-domain scorecard data yet");
    }

    let plot_w = (w - pad_l - pad_r) as f64;
    let rows: Vec<String> = opts
        .bars
        .iter()
        .enumerate()
        .map(|(i, bar)| {
            let y = pad_t as f64 + i as f64 * row_h as f64;
            let bar_w = (bar.accuracy.clamp(0.0, 1.0) * plot_w).max(0.0);
            let acc_pct = format!("{:.0}%", bar.accuracy * 100.0);
            format!(
                concat!(
                    "\n  <text x=\"{xl}\" y=\"{yt}\" font-size=\"12\" fill=\"{tp}\" text-anchor=\"end\">{label}</text>",
                    "\n  <rect x=\"{pad_l}\" y=\"{yr}\" width=\"{pw}\" height=\"16\" fill=\"{bg2}\" />",
                    "\n  <rect x=\"{pad_l}\" y=\"{yr}\" width=\"{bw}\" height=\"16\" fill=\"{accent}\" />",
                    "\n  <text x=\"{xr}\" y=\"{yt2}\" font-size=\"11\" fill=\"{muted}\">{pct} · n={n}</text>",
                ),
                xl = pad_l as f64 - 8.0,
                yt = y + 18.0,
                tp = tokens::TEXT_PRIMARY,
                label = escape_xml(&bar.label),
                pad_l = pad_l,
                yr = y + 6.0,
                pw = format!("{:.1}", plot_w),
                bg2 = tokens::BG_SECONDARY,
                bw = format!("{:.1}", bar_w),
                accent = tokens::ACCENT,
                xr = pad_l as f64 + plot_w + 6.0,
                yt2 = y + 18.0,
                muted = tokens::TEXT_MUTED,
                pct = acc_pct,
                n = bar.n,
            )
        })
        .collect();

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img" aria-label="Per-domain accuracy">
  <rect width="{w}" height="{h}" fill="{bg}"/>
  <text x="{xl}" y="{yt}" font-size="12" fill="{text2}" text-anchor="end">Per-domain accuracy</text>{rows}
</svg>"#,
        w = w,
        h = h,
        bg = tokens::BG_PRIMARY,
        xl = pad_l as f64 - 8.0,
        yt = pad_t as f64 - 8.0,
        text2 = tokens::TEXT_SECONDARY,
        rows = rows.join(""),
    )
}

// ─── AbandonedThreads ──────────────────────────────────────────────

/// A high-conviction take that hasn't been revisited.
#[derive(Debug, Clone)]
pub struct AbandonedThread {
    pub take_id: u32,
    pub page_slug: String,
    pub claim: String,
    pub months_silent: u32,
    pub conviction: f64,
    pub revisit_href: Option<String>,
}

/// Render abandoned high-conviction threads card SVG.
pub fn render_abandoned_threads_card(threads: &[AbandonedThread], width: u32) -> String {
    let pad_t: u32 = 24;
    let row_h: u32 = 44;
    let h = pad_t + (threads.len().max(1) as u32) * row_h + 12;

    if threads.is_empty() {
        return svg_empty(width, 80, "No abandoned high-conviction threads — clean slate");
    }

    let rows: Vec<String> = threads
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let y = pad_t as f64 + i as f64 * row_h as f64;
            let claim = if t.claim.len() > 70 {
                format!("{}…", &t.claim[..70])
            } else {
                t.claim.clone()
            };
            let meta = format!(
                "conviction {:.2} · {} months silent",
                t.conviction, t.months_silent
            );
            let href = t
                .revisit_href
                .clone()
                .unwrap_or_else(|| format!("/admin/calibration/revisit/{}", t.take_id));
            format!(
                concat!(
                    "\n  <text x=\"16\" y=\"{yc}\" font-size=\"13\" fill=\"{tp}\">{claim}</text>",
                    "\n  <text x=\"16\" y=\"{ym}\" font-size=\"11\" fill=\"{muted}\">{meta}</text>",
                    "\n  <a href=\"{href}\"><text x=\"{xr}\" y=\"{yr}\" font-size=\"11\" fill=\"{accent}\" text-anchor=\"end\">revisit now</text></a>",
                ),
                yc = y + 16.0,
                tp = tokens::TEXT_PRIMARY,
                claim = escape_xml(&claim),
                ym = y + 32.0,
                muted = tokens::TEXT_MUTED,
                meta = escape_xml(&meta),
                href = escape_xml(&href),
                xr = width as f64 - 16.0,
                yr = y + 24.0,
                accent = tokens::ACCENT,
            )
        })
        .collect();

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img" aria-label="Abandoned threads">
  <rect width="{w}" height="{h}" fill="{bg}"/>
  <text x="16" y="{yt}" font-size="12" fill="{text2}">You committed to these and never revisited</text>{rows}
</svg>"#,
        w = width,
        h = h,
        bg = tokens::BG_PRIMARY,
        yt = pad_t as f64 - 8.0,
        text2 = tokens::TEXT_SECONDARY,
        rows = rows.join(""),
    )
}

// ─── PatternStatementsCard ─────────────────────────────────────────

/// A single clickable pattern-statement card item.
#[derive(Debug, Clone)]
pub struct PatternStatementsCardItem {
    pub text: String,
    pub drill_href: Option<String>,
}

/// Render a pattern-statements clickable card SVG.
pub fn render_pattern_statements_card(
    statements: &[PatternStatementsCardItem],
    width: u32,
) -> String {
    let pad_t: u32 = 24;
    let row_h: u32 = 36;
    let h = pad_t + (statements.len().max(1) as u32) * row_h + 12;

    if statements.is_empty() {
        return svg_empty(width, 60, "No active patterns yet");
    }

    let rows: Vec<String> = statements
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let y = pad_t as f64 + i as f64 * row_h as f64;
            let txt = if s.text.len() > 90 {
                format!("{}…", &s.text[..90])
            } else {
                s.text.clone()
            };
            let href = s
                .drill_href
                .clone()
                .unwrap_or_else(|| format!("/admin/calibration/pattern/{}", i + 1));
            format!(
                r#"
  <a href="{href}"><text x="16" y="{y}" font-size="14" fill="{tp}">{txt}</text></a>"#,
                href = escape_xml(&href),
                y = y + 22.0,
                tp = tokens::TEXT_PRIMARY,
                txt = escape_xml(&txt),
            )
        })
        .collect();

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img" aria-label="Calibration pattern statements">
  <rect width="{w}" height="{h}" fill="{bg}"/>
  <text x="16" y="{yt}" font-size="12" fill="{text2}">Active patterns (click to drill down)</text>{rows}
</svg>"#,
        w = width,
        h = h,
        bg = tokens::BG_PRIMARY,
        yt = pad_t as f64 - 8.0,
        text2 = tokens::TEXT_SECONDARY,
        rows = rows.join(""),
    )
}
