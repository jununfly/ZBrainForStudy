//! Fence parser/renderer for `## Facts` markdown tables.
//!
//! Port of `src/core/facts-fence.ts`. Markdown is the source of truth (git is
//! canonical). The DB `facts` table is a derived index. This module is the
//! boundary between them.
//!
//! Fence shape (HTML-comment markers):
//!
//! ```markdown
//! ## Facts
//!
//! <!--- zbrain:facts:begin -->
//! | # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
//! |---|-------|------|------------|------------|------------|------------|-------------|--------|---------|
//! | 1 | Founded Acme in 2017 | fact | 1.0 | world | high | 2017-01-01 |  | linkedin |  |
//! <!--- zbrain:facts:end -->
//! ```
//!
//! v0.35.4 widens to 14 columns when any row carries typed-claim fields:
//!
//! ```markdown
//! | # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context | claim_metric | claim_value | claim_unit | claim_period |
//! ```
//!
//! # Strikethrough semantics
//!
//! - `~~claim~~` + `context: superseded by #N` → active=false, superseded_by=N
//! - `~~claim~~` + `context: forgotten: <reason>` → active=false, forgotten=true
//! - `~~claim~~` + anything else in context → active=false

use std::collections::HashSet;

use crate::types::{FactKind, FactVisibility};

// ── Constants ───────────────────────────────────────────────────────────────

/// HTML-comment fence markers — verbatim per spec.
pub const FACTS_FENCE_BEGIN: &str = "<!--- zbrain:facts:begin -->";
pub const FACTS_FENCE_END: &str = "<!--- zbrain:facts:end -->";

const NOTABILITY_VALUES: &[&str] = &["high", "medium", "low"];

// ── Types ───────────────────────────────────────────────────────────────────

/// A single parsed fact row from the markdown fence.
///
/// Separate from `FactRow` (DB row) — this is the fence-level type that
/// carries strikethrough metadata (`active`, `superseded_by`, `forgotten`)
/// and is the round-trip currency between parse ↔ render.
#[derive(Debug, Clone, PartialEq)]
pub struct FenceFact {
    pub row_num: i32,
    pub claim: String,
    pub kind: FactKind,
    pub confidence: f64,
    pub visibility: FactVisibility,
    pub notability: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub source: Option<String>,
    pub context: Option<String>,
    pub active: bool,
    /// When set: this row was superseded by another fence row.
    /// `context` matched `/superseded by #(\d+)/i`.
    pub superseded_by: Option<i32>,
    /// When true: the user invoked "forget" on this row.
    /// `context` matched `/^forgotten\s*:/i`.
    pub forgotten: bool,
    // v0.35.4 typed-claim fields
    pub claim_metric: Option<String>,
    pub claim_value: Option<f64>,
    pub claim_unit: Option<String>,
    pub claim_period: Option<String>,
}

/// Result of parsing a facts fence.
#[derive(Debug, Clone, PartialEq)]
pub struct FactsParseResult {
    pub facts: Vec<FenceFact>,
    pub warnings: Vec<String>,
}

/// Input for `upsert_fact_row`.
#[derive(Debug, Clone)]
pub struct FenceFactInput {
    pub claim: String,
    pub kind: FactKind,
    pub confidence: f64,
    pub visibility: FactVisibility,
    pub notability: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub source: Option<String>,
    pub context: Option<String>,
    pub active: Option<bool>,
    pub row_num: Option<i32>,
    pub claim_metric: Option<String>,
    pub claim_value: Option<f64>,
    pub claim_unit: Option<String>,
    pub claim_period: Option<String>,
}

/// Result of `upsert_fact_row`.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertFactResult {
    pub body: String,
    pub row_num: i32,
}

/// Options for `strip_facts_fence`.
#[derive(Debug, Clone, Default)]
pub struct StripFactsFenceOpts {
    /// Visibility values to KEEP in the rendered output.
    /// When `None` or empty, the entire fence block is removed.
    /// When set to e.g. `["world"]`, keeps only world-visibility rows.
    pub keep_visibility: Option<Vec<FactVisibility>>,
}

// ── Shared row helpers (mirrors fence-shared.ts / takes_fence.rs) ───────────

/// Parse a pipe-separated table row into trimmed cells.
fn parse_row_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') {
        return None;
    }
    if !trimmed[1..].contains('|') {
        return None;
    }
    let inner = trimmed
        .strip_prefix('|')
        .and_then(|s| s.strip_suffix('|'))
        .unwrap_or(trimmed);
    let cells: Vec<String> = inner.split('|').map(|c| c.trim().to_string()).collect();
    if cells.is_empty() {
        return None;
    }
    Some(cells)
}

/// Detect a markdown table separator row.
fn is_separator_row(cells: &[String]) -> bool {
    if cells.is_empty() {
        return false;
    }
    cells
        .iter()
        .all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch.is_whitespace()))
}

/// Parse `~~text~~` → stripped text + struck flag.
fn strip_strikethrough(s: &str) -> (String, bool) {
    let trimmed = s.trim();
    if let Some(inner) = trimmed
        .strip_prefix("~~")
        .and_then(|rest| rest.strip_suffix("~~"))
    {
        (inner.trim().to_string(), true)
    } else {
        (s.to_string(), false)
    }
}

/// Trim + empty → None mapping for optional string fields.
fn parse_string_cell(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Escape `|` in a cell value so the table layout stays intact.
fn escape_fence_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

// ── Facts-specific parsers ──────────────────────────────────────────────────

/// Parse a confidence cell (0..1 float).
fn parse_confidence_cell(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Parse a free-form numeric cell (tolerates comma thousand separators).
fn parse_numeric_cell(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = trimmed.replace(',', "");
    stripped.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Extract `superseded_by` from context: `superseded by #N`.
fn parse_superseded_by_from_context(context: &Option<String>) -> Option<i32> {
    let ctx = context.as_ref()?;
    // Match "superseded by #<digits>" case-insensitively
    let lower = ctx.to_lowercase();
    let needle = "superseded by #";
    let pos = lower.find(needle)?;
    let after = &lower[pos + needle.len()..];
    let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        return None;
    }
    num_str.parse::<i32>().ok().filter(|&n| n > 0)
}

/// Detect "forgotten:" prefix in context.
fn parse_forgotten_from_context(context: &Option<String>) -> bool {
    match context {
        Some(ctx) => {
            let trimmed = ctx.trim();
            let lower = trimmed.to_lowercase();
            lower.starts_with("forgotten:")
        }
        None => false,
    }
}

/// Parse a kind string into a FactKind, validating against allowed values.
fn parse_kind(raw: &str) -> Result<FactKind, String> {
    match raw.trim().to_lowercase().as_str() {
        "event" => Ok(FactKind::Event),
        "preference" => Ok(FactKind::Preference),
        "commitment" => Ok(FactKind::Commitment),
        "belief" => Ok(FactKind::Belief),
        "fact" => Ok(FactKind::Fact),
        other => Err(format!(
            "unknown kind \"{}\" (expected event|preference|commitment|belief|fact)",
            other
        )),
    }
}

/// Parse a visibility string.
fn parse_visibility(raw: &str) -> Result<FactVisibility, String> {
    match raw.trim().to_lowercase().as_str() {
        "private" => Ok(FactVisibility::Private),
        "world" => Ok(FactVisibility::World),
        other => Err(format!(
            "unknown visibility \"{}\" (expected private|world)",
            other
        )),
    }
}

// ── Formatting helpers ──────────────────────────────────────────────────────

fn format_confidence(c: f64) -> String {
    let frac = c.fract();
    if frac.abs() < f64::EPSILON || (c * 10.0).fract().abs() < f64::EPSILON {
        // integer or one decimal digit → keep one decimal
        format!("{:.1}", c)
    } else {
        let s = format!("{:.2}", c);
        let trimmed = s.trim_end_matches('0');
        if trimmed.ends_with('.') {
            format!("{}0", trimmed)
        } else {
            trimmed.to_string()
        }
    }
}

// ── Core parser ─────────────────────────────────────────────────────────────

/// Parse a facts fence from a markdown body.
///
/// Returns empty facts + empty warnings when no fence is present.
/// Malformed rows are skipped with a warning; the rest still parses.
///
/// Port of `parseFactsFence` from `facts-fence.ts`.
pub fn parse_facts_fence(body: &str) -> FactsParseResult {
    let begin_idx = match body.find(FACTS_FENCE_BEGIN) {
        Some(i) => i,
        None => {
            let end_idx = body.find(FACTS_FENCE_END);
            if end_idx.is_none() {
                return FactsParseResult {
                    facts: vec![],
                    warnings: vec![],
                };
            }
            // begin missing, end present → unbalanced
            return FactsParseResult {
                facts: vec![],
                warnings: vec!["FACTS_FENCE_UNBALANCED: missing begin or end marker".to_string()],
            };
        }
    };

    let end_idx = match body[begin_idx + FACTS_FENCE_BEGIN.len()..].find(FACTS_FENCE_END) {
        Some(rel) => begin_idx + FACTS_FENCE_BEGIN.len() + rel,
        None => {
            return FactsParseResult {
                facts: vec![],
                warnings: vec!["FACTS_FENCE_UNBALANCED: missing begin or end marker".to_string()],
            };
        }
    };

    if end_idx < begin_idx {
        return FactsParseResult {
            facts: vec![],
            warnings: vec![
                "FACTS_FENCE_UNBALANCED: end marker before begin".to_string(),
            ],
        };
    }

    let inner = &body[begin_idx + FACTS_FENCE_BEGIN.len()..end_idx];
    let lines: Vec<&str> = inner.lines().collect();
    let mut facts: Vec<FenceFact> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut saw_header = false;
    let mut seen_row_nums: HashSet<i32> = HashSet::new();

    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let cells = match parse_row_cells(line) {
            Some(c) => c,
            None => continue,
        };

        // Header detection
        if !saw_header {
            let lower: Vec<String> = cells.iter().map(|c| c.to_lowercase()).collect();
            if lower.contains(&"claim".to_string()) && lower.contains(&"kind".to_string()) {
                saw_header = true;
                continue;
            }
            warnings.push(format!(
                "FACTS_TABLE_MALFORMED: row before header: \"{}\"",
                line.trim()
            ));
            continue;
        }

        // Separator row
        if is_separator_row(&cells) {
            continue;
        }

        // Expect at least 9 cells (9-cell minimum tolerating missing trailing context)
        if cells.len() < 9 {
            warnings.push(format!(
                "FACTS_TABLE_MALFORMED: only {} cells in row \"{}\"",
                cells.len(),
                line.trim()
            ));
            continue;
        }

        // Pad cells to 14 for destructuring (10 base + 4 typed-claim)
        let mut padded = cells.clone();
        while padded.len() < 14 {
            padded.push(String::new());
        }

        let row_num: i32 = match padded[0].parse() {
            Ok(n) if n > 0 => n,
            _ => {
                warnings.push(format!(
                    "FACTS_TABLE_MALFORMED: invalid row_num \"{}\"",
                    padded[0]
                ));
                continue;
            }
        };
        if !seen_row_nums.insert(row_num) {
            warnings.push(format!(
                "FACTS_ROW_NUM_COLLISION: duplicate row_num {}",
                row_num
            ));
            continue;
        }

        // kind validation
        let kind = match parse_kind(&padded[2]) {
            Ok(k) => k,
            Err(e) => {
                warnings.push(format!("FACTS_TABLE_MALFORMED: {}", e));
                continue;
            }
        };

        // visibility validation
        let visibility = match parse_visibility(&padded[4]) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("FACTS_TABLE_MALFORMED: {}", e));
                continue;
            }
        };

        // notability validation
        let notability_raw = padded[5].trim().to_lowercase();
        if !NOTABILITY_VALUES.contains(&notability_raw.as_str()) {
            warnings.push(format!(
                "FACTS_TABLE_MALFORMED: unknown notability \"{}\" (expected high|medium|low)",
                padded[5].trim()
            ));
            continue;
        }

        // confidence validation
        let confidence = match parse_confidence_cell(&padded[3]) {
            Some(c) => c,
            None => {
                warnings.push(format!(
                    "FACTS_TABLE_MALFORMED: non-numeric confidence \"{}\" in row {}",
                    padded[3].trim(),
                    row_num
                ));
                continue;
            }
        };

        let (claim_text, struck) = strip_strikethrough(&padded[1]);
        let context = parse_string_cell(&padded[9]);
        let superseded_by = parse_superseded_by_from_context(&context);
        let forgotten = if struck {
            parse_forgotten_from_context(&context)
        } else {
            false
        };

        facts.push(FenceFact {
            row_num,
            claim: claim_text,
            kind,
            confidence,
            visibility,
            notability: notability_raw,
            valid_from: parse_string_cell(&padded[6]),
            valid_until: parse_string_cell(&padded[7]),
            source: parse_string_cell(&padded[8]),
            context,
            active: !struck,
            superseded_by,
            forgotten,
            claim_metric: parse_string_cell(&padded[10]),
            claim_value: parse_numeric_cell(&padded[11]),
            claim_unit: parse_string_cell(&padded[12]),
            claim_period: parse_string_cell(&padded[13]),
        });
    }

    if !saw_header && facts.is_empty() && lines.iter().any(|l| l.trim().starts_with('|')) {
        warnings.push(
            "FACTS_TABLE_MALFORMED: pipe-rows present but no recognizable header".to_string(),
        );
    }

    FactsParseResult { facts, warnings }
}

// ── Renderer ────────────────────────────────────────────────────────────────

/// Render a facts array back to a fenced markdown table.
///
/// Round-trip safe with `parse_facts_fence`. Widens to 14 columns when ANY
/// fact has a non-None typed-claim field; otherwise stays at 10 columns.
///
/// Port of `renderFactsTable` from `facts-fence.ts`.
pub fn render_facts_fence(facts: &[FenceFact]) -> String {
    let any_typed = facts.iter().any(|f| {
        f.claim_metric.is_some()
            || f.claim_value.is_some()
            || f.claim_unit.is_some()
            || f.claim_period.is_some()
    });

    let header: &str = if any_typed {
        "| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context | claim_metric | claim_value | claim_unit | claim_period |"
    } else {
        "| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |"
    };

    let separator: &str = if any_typed {
        "|---|-------|------|------------|------------|------------|------------|-------------|--------|---------|--------------|-------------|------------|--------------|"
    } else {
        "|---|-------|------|------------|------------|------------|------------|-------------|--------|---------|"
    };

    let rows: Vec<String> = facts
        .iter()
        .map(|f| {
            let claim_cell = if f.active {
                escape_fence_cell(&f.claim)
            } else {
                format!("~~{}~~", escape_fence_cell(&f.claim))
            };
            let base = format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                f.row_num,
                claim_cell,
                f.kind,
                format_confidence(f.confidence),
                f.visibility,
                f.notability,
                escape_fence_cell(f.valid_from.as_deref().unwrap_or("")),
                escape_fence_cell(f.valid_until.as_deref().unwrap_or("")),
                escape_fence_cell(f.source.as_deref().unwrap_or("")),
                escape_fence_cell(f.context.as_deref().unwrap_or("")),
            );
            if !any_typed {
                return base;
            }
            let value_cell = f.claim_value.map_or(String::new(), |v| v.to_string());
            format!(
                "{} {} | {} | {} | {} |",
                base,
                escape_fence_cell(f.claim_metric.as_deref().unwrap_or("")),
                escape_fence_cell(&value_cell),
                escape_fence_cell(f.claim_unit.as_deref().unwrap_or("")),
                escape_fence_cell(f.claim_period.as_deref().unwrap_or("")),
            )
        })
        .collect();

    let inner = format!("\n{}\n{}\n{}\n", header, separator, rows.join("\n"));
    format!("{}{}{}", FACTS_FENCE_BEGIN, inner, FACTS_FENCE_END)
}

// ── Upsert ──────────────────────────────────────────────────────────────────

/// Append a new fact row to the body. If a fenced facts table exists, appends
/// at the end. Otherwise creates a new `## Facts` section + fence.
///
/// Append-only — row_num is (max existing + 1). Stable forever.
///
/// Port of `upsertFactRow` from `facts-fence.ts`.
pub fn upsert_fact_row(body: &str, new_row: &FenceFactInput) -> UpsertFactResult {
    let FactsParseResult { facts, warnings: _ } = parse_facts_fence(body);

    let next_row_num = new_row.row_num.unwrap_or_else(|| {
        facts
            .iter()
            .map(|f| f.row_num)
            .max()
            .map_or(1, |m| m + 1)
    });

    let mut all_rows: Vec<FenceFact> = facts;
    all_rows.push(FenceFact {
        row_num: next_row_num,
        claim: new_row.claim.clone(),
        kind: new_row.kind.clone(),
        confidence: new_row.confidence,
        visibility: new_row.visibility.clone(),
        notability: new_row.notability.clone(),
        valid_from: new_row.valid_from.clone(),
        valid_until: new_row.valid_until.clone(),
        source: new_row.source.clone(),
        context: new_row.context.clone(),
        active: new_row.active.unwrap_or(true),
        superseded_by: None,
        forgotten: false,
        claim_metric: new_row.claim_metric.clone(),
        claim_value: new_row.claim_value,
        claim_unit: new_row.claim_unit.clone(),
        claim_period: new_row.claim_period.clone(),
    });

    let new_fence = render_facts_fence(&all_rows);

    let out = if let (Some(bi), Some(ei)) =
        (body.find(FACTS_FENCE_BEGIN), body.find(FACTS_FENCE_END))
    {
        let ei_end = ei + FACTS_FENCE_END.len();
        format!("{}{}{}", &body[..bi], new_fence, &body[ei_end..])
    } else {
        let sep = if body.ends_with('\n') { "\n" } else { "\n\n" };
        format!("{}{}## Facts\n\n{}\n", body, sep, new_fence)
    };

    UpsertFactResult {
        body: out,
        row_num: next_row_num,
    }
}

// ── Strip ───────────────────────────────────────────────────────────────────

/// Strip facts content from the body. Two modes:
///
/// 1. No `keep_visibility` (or empty): drop the entire fence block.
/// 2. `keep_visibility` set: retain only matching visibility rows.
///
/// Port of `stripFactsFence` from `facts-fence.ts`.
pub fn strip_facts_fence(body: &str, opts: &StripFactsFenceOpts) -> String {
    let begin_idx = match body.find(FACTS_FENCE_BEGIN) {
        Some(i) => i,
        None => return body.to_string(),
    };
    let end_start = begin_idx + FACTS_FENCE_BEGIN.len();
    let end_rel = match body[end_start..].find(FACTS_FENCE_END) {
        Some(rel) => rel,
        None => return body.to_string(),
    };
    let end_idx = end_start + end_rel;

    // Whole-fence strip mode
    let keep = match &opts.keep_visibility {
        Some(v) if !v.is_empty() => v,
        _ => {
            let end = end_idx + FACTS_FENCE_END.len();
            return format!("{}{}", &body[..begin_idx], &body[end..]);
        }
    };

    // Selective row-level strip mode
    let FactsParseResult { facts, .. } = parse_facts_fence(body);
    let kept: Vec<FenceFact> = facts
        .into_iter()
        .filter(|f| keep.contains(&f.visibility))
        .collect();
    let replacement = render_facts_fence(&kept);
    let end = end_idx + FACTS_FENCE_END.len();
    format!("{}{}{}", &body[..begin_idx], replacement, &body[end..])
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Row helpers ────────────────────────────────────────────────────────

    #[test]
    fn parse_row_cells_standard() {
        let cells = parse_row_cells("| 1 | Founded Acme | fact | 1.0 | world | high | 2017-01 |  | src | ctx |").unwrap();
        assert_eq!(cells.len(), 10);
        assert_eq!(cells[0], "1");
        assert_eq!(cells[1], "Founded Acme");
        assert_eq!(cells[2], "fact");
    }

    #[test]
    fn parse_row_cells_rejects_non_pipe() {
        assert!(parse_row_cells("hello world").is_none());
    }

    #[test]
    fn is_separator_dashes() {
        assert!(is_separator_row(&["---".to_string(), "---".to_string()]));
    }

    #[test]
    fn is_separator_rejects_text() {
        assert!(!is_separator_row(&["claim".to_string(), "kind".to_string()]));
    }

    #[test]
    fn strip_strikethrough_active() {
        let (text, struck) = strip_strikethrough("hello world");
        assert_eq!(text, "hello world");
        assert!(!struck);
    }

    #[test]
    fn strip_strikethrough_struck() {
        let (text, struck) = strip_strikethrough("~~hello world~~");
        assert_eq!(text, "hello world");
        assert!(struck);
    }

    #[test]
    fn parse_confidence_valid() {
        assert_eq!(parse_confidence_cell("0.85"), Some(0.85));
        assert_eq!(parse_confidence_cell("1.0"), Some(1.0));
    }

    #[test]
    fn parse_confidence_empty() {
        assert_eq!(parse_confidence_cell(""), None);
    }

    #[test]
    fn parse_confidence_non_numeric() {
        assert_eq!(parse_confidence_cell("abc"), None);
    }

    #[test]
    fn parse_numeric_with_comma() {
        let v = parse_numeric_cell("50,000");
        assert_eq!(v, Some(50000.0));
    }

    #[test]
    fn parse_superseded_by_detected() {
        assert_eq!(
            parse_superseded_by_from_context(&Some("superseded by #4".to_string())),
            Some(4)
        );
    }

    #[test]
    fn parse_superseded_by_case_insensitive() {
        assert_eq!(
            parse_superseded_by_from_context(&Some("Superseded By #42".to_string())),
            Some(42)
        );
    }

    #[test]
    fn parse_superseded_by_none() {
        assert_eq!(
            parse_superseded_by_from_context(&Some("some context".to_string())),
            None
        );
    }

    #[test]
    fn parse_forgotten_detected() {
        assert!(parse_forgotten_from_context(&Some(
            "forgotten: user asked to remove".to_string()
        )));
    }

    #[test]
    fn parse_forgotten_not_detected() {
        assert!(!parse_forgotten_from_context(&Some("superseded by #3".to_string())));
    }

    // ── Parse / Render round-trip ───────────────────────────────────────────

    #[test]
    fn parse_empty_body() {
        let result = parse_facts_fence("no fence here");
        assert!(result.facts.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn parse_unbalanced_missing_end() {
        let body = r#"<!--- zbrain:facts:begin -->
some content"#;
        let result = parse_facts_fence(body);
        assert!(result.facts.is_empty());
        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].contains("UNBALANCED"));
    }

    #[test]
    fn parse_and_render_roundtrip_10col() {
        let body = r#"some content

## Facts

<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | Founded Acme in 2017 | fact | 1.0 | world | high | 2017-01-01 |  | linkedin |  |
| 2 | Prefers async over meetings | preference | 0.85 | private | medium | 2026-04-29 |  | OH |  |
<!--- zbrain:facts:end -->

more content"#;

        let result = parse_facts_fence(body);
        assert_eq!(result.facts.len(), 2);
        assert!(result.warnings.is_empty());

        assert_eq!(result.facts[0].row_num, 1);
        assert_eq!(result.facts[0].claim, "Founded Acme in 2017");
        assert_eq!(result.facts[0].kind, FactKind::Fact);
        assert_eq!(result.facts[0].confidence, 1.0);
        assert_eq!(result.facts[0].visibility, FactVisibility::World);
        assert_eq!(result.facts[0].notability, "high");
        assert_eq!(result.facts[0].valid_from.as_deref(), Some("2017-01-01"));
        assert_eq!(result.facts[0].valid_until, None);
        assert_eq!(result.facts[0].source.as_deref(), Some("linkedin"));
        assert!(result.facts[0].active);

        assert_eq!(result.facts[1].row_num, 2);
        assert_eq!(result.facts[1].kind, FactKind::Preference);
        assert_eq!(result.facts[1].visibility, FactVisibility::Private);

        // Render and re-parse
        let rendered = render_facts_fence(&result.facts);
        let reparsed = parse_facts_fence(&rendered);
        assert_eq!(reparsed.facts.len(), 2);
        assert_eq!(reparsed.facts[0].claim, "Founded Acme in 2017");
        assert_eq!(reparsed.facts[1].claim, "Prefers async over meetings");
    }

    #[test]
    fn parse_strikethrough_with_superseded() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | ~~Will hit $10M ARR~~ | commitment | 0.55 | world | medium | 2026-06-01 | 2026-12-31 | bo call | superseded by #2 |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert_eq!(result.facts.len(), 1);
        assert_eq!(result.facts[0].claim, "Will hit $10M ARR");
        assert!(!result.facts[0].active);
        assert_eq!(result.facts[0].superseded_by, Some(2));
        assert!(!result.facts[0].forgotten);
    }

    #[test]
    fn parse_strikethrough_with_forgotten() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | ~~Used to live in Tokyo~~ | fact | 0.9 | private | low | 2018-01-01 | 2026-05-10 | inferred | forgotten: user asked to remove |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert_eq!(result.facts.len(), 1);
        assert!(!result.facts[0].active);
        assert!(result.facts[0].forgotten);
        assert_eq!(result.facts[0].superseded_by, None);
    }

    #[test]
    fn parse_14col_typed_claim() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context | claim_metric | claim_value | claim_unit | claim_period |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | Revenue milestone | event | 0.95 | world | high | 2025-Q1 |  | earnings call |  | arr | 10000000 | USD | annual |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert_eq!(result.facts.len(), 1);
        assert_eq!(result.facts[0].claim_metric.as_deref(), Some("arr"));
        assert_eq!(result.facts[0].claim_value, Some(10_000_000.0));
        assert_eq!(result.facts[0].claim_unit.as_deref(), Some("USD"));
        assert_eq!(result.facts[0].claim_period.as_deref(), Some("annual"));

        // Round-trip: render should widen to 14 cols
        let rendered = render_facts_fence(&result.facts);
        assert!(rendered.contains("claim_metric"));
        assert!(rendered.contains("claim_value"));
        let reparsed = parse_facts_fence(&rendered);
        assert_eq!(reparsed.facts[0].claim_value, Some(10_000_000.0));
    }

    #[test]
    fn render_10col_when_no_typed_claims() {
        let facts = vec![FenceFact {
            row_num: 1,
            claim: "test".into(),
            kind: FactKind::Fact,
            confidence: 0.5,
            visibility: FactVisibility::World,
            notability: "medium".into(),
            valid_from: None,
            valid_until: None,
            source: None,
            context: None,
            active: true,
            superseded_by: None,
            forgotten: false,
            claim_metric: None,
            claim_value: None,
            claim_unit: None,
            claim_period: None,
        }];
        let rendered = render_facts_fence(&facts);
        // Should NOT contain typed-claim columns
        assert!(!rendered.contains("claim_metric"));
        assert!(!rendered.contains("claim_value"));
    }

    #[test]
    fn render_14col_when_any_typed_claim() {
        let mut facts = vec![FenceFact {
            row_num: 1,
            claim: "test".into(),
            kind: FactKind::Fact,
            confidence: 0.5,
            visibility: FactVisibility::World,
            notability: "medium".into(),
            valid_from: None,
            valid_until: None,
            source: None,
            context: None,
            active: true,
            superseded_by: None,
            forgotten: false,
            claim_metric: None,
            claim_value: None,
            claim_unit: None,
            claim_period: None,
        }];
        // Add a second fact with a typed claim
        facts.push(FenceFact {
            row_num: 2,
            claim: "has typed".into(),
            kind: FactKind::Event,
            confidence: 0.8,
            visibility: FactVisibility::World,
            notability: "high".into(),
            valid_from: None,
            valid_until: None,
            source: None,
            context: None,
            active: true,
            superseded_by: None,
            forgotten: false,
            claim_metric: Some("arr".into()),
            claim_value: Some(5_000_000.0),
            claim_unit: None,
            claim_period: None,
        });
        let rendered = render_facts_fence(&facts);
        assert!(rendered.contains("claim_metric"));
        assert!(rendered.contains("claim_value"));
    }

    #[test]
    fn roundtrip_strikethrough_preserves_inactive() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | ~~old fact~~ | fact | 0.5 | private | low | 2024 |  |  |  |
| 2 | active fact | fact | 0.9 | world | high | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert_eq!(result.facts.len(), 2);
        assert!(!result.facts[0].active);
        assert!(result.facts[1].active);

        let rendered = render_facts_fence(&result.facts);
        assert!(rendered.contains("~~old fact~~"));
        assert!(rendered.contains("| 2 | active fact"));

        let reparsed = parse_facts_fence(&rendered);
        assert!(!reparsed.facts[0].active);
        assert!(reparsed.facts[1].active);
    }

    #[test]
    fn roundtrip_confidence_formatting() {
        // 1.0 → "1.0", 0.85 → "0.85", 0.5 → "0.5"
        let facts = vec![FenceFact {
            row_num: 1,
            claim: "test".into(),
            kind: FactKind::Fact,
            confidence: 1.0,
            visibility: FactVisibility::World,
            notability: "medium".into(),
            valid_from: None,
            valid_until: None,
            source: None,
            context: None,
            active: true,
            superseded_by: None,
            forgotten: false,
            claim_metric: None,
            claim_value: None,
            claim_unit: None,
            claim_period: None,
        }];
        let rendered = render_facts_fence(&facts);
        assert!(rendered.contains("| 1.0 |"));
        let reparsed = parse_facts_fence(&rendered);
        assert_eq!(reparsed.facts[0].confidence, 1.0);
    }

    // ── Validation ─────────────────────────────────────────────────────────

    #[test]
    fn parse_rejects_invalid_kind() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | test | invalid_kind | 0.5 | world | high | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert!(result.facts.is_empty());
        assert!(result.warnings.iter().any(|w| w.contains("unknown kind")));
    }

    #[test]
    fn parse_rejects_invalid_visibility() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | test | fact | 0.5 | public | high | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert!(result.facts.is_empty());
        assert!(result.warnings.iter().any(|w| w.contains("unknown visibility")));
    }

    #[test]
    fn parse_rejects_invalid_notability() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | test | fact | 0.5 | world | mega | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert!(result.facts.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("unknown notability")));
    }

    #[test]
    fn parse_rejects_duplicate_row_num() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | first | fact | 0.5 | world | high | 2025 |  |  |  |
| 1 | dup | fact | 0.5 | world | high | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let result = parse_facts_fence(body);
        assert_eq!(result.facts.len(), 1);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("ROW_NUM_COLLISION")));
    }

    // ── Upsert ──────────────────────────────────────────────────────────────

    #[test]
    fn upsert_creates_new_fence() {
        let body = "just some text";
        let result = upsert_fact_row(
            body,
            &FenceFactInput {
                claim: "new claim".into(),
                kind: FactKind::Fact,
                confidence: 0.8,
                visibility: FactVisibility::World,
                notability: "high".into(),
                valid_from: Some("2026".into()),
                valid_until: None,
                source: Some("test".into()),
                context: None,
                active: None,
                row_num: None,
                claim_metric: None,
                claim_value: None,
                claim_unit: None,
                claim_period: None,
            },
        );
        assert!(result.body.contains("## Facts"));
        assert!(result.body.contains("<!--- zbrain:facts:begin -->"));
        assert!(result.body.contains("new claim"));
        assert_eq!(result.row_num, 1);
    }

    #[test]
    fn upsert_appends_to_existing_fence() {
        let body = r#"some text

## Facts

<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | existing | fact | 0.5 | world | medium | 2025 |  |  |  |
<!--- zbrain:facts:end -->
"#;

        let result = upsert_fact_row(
            body,
            &FenceFactInput {
                claim: "appended".into(),
                kind: FactKind::Preference,
                confidence: 0.75,
                visibility: FactVisibility::Private,
                notability: "low".into(),
                valid_from: None,
                valid_until: None,
                source: None,
                context: None,
                active: None,
                row_num: None,
                claim_metric: None,
                claim_value: None,
                claim_unit: None,
                claim_period: None,
            },
        );
        assert!(result.body.contains("existing"));
        assert!(result.body.contains("appended"));
        assert_eq!(result.row_num, 2);

        let reparsed = parse_facts_fence(&result.body);
        assert_eq!(reparsed.facts.len(), 2);
    }

    #[test]
    fn upsert_respects_explicit_row_num() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | existing | fact | 0.5 | world | medium | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let result = upsert_fact_row(
            body,
            &FenceFactInput {
                claim: "explicit".into(),
                kind: FactKind::Fact,
                confidence: 0.5,
                visibility: FactVisibility::World,
                notability: "medium".into(),
                valid_from: None,
                valid_until: None,
                source: None,
                context: None,
                active: None,
                row_num: Some(10),
                claim_metric: None,
                claim_value: None,
                claim_unit: None,
                claim_period: None,
            },
        );
        assert_eq!(result.row_num, 10);
    }

    // ── Strip ───────────────────────────────────────────────────────────────

    #[test]
    fn strip_removes_entire_fence() {
        let body = r#"some text before

## Facts

<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | private fact | fact | 0.5 | private | low | 2025 |  |  |  |
| 2 | world fact | fact | 0.9 | world | high | 2025 |  |  |  |
<!--- zbrain:facts:end -->

some text after"#;

        let stripped = strip_facts_fence(
            body,
            &StripFactsFenceOpts {
                keep_visibility: None,
            },
        );
        assert!(!stripped.contains("private fact"));
        assert!(!stripped.contains("world fact"));
        assert!(!stripped.contains("zbrain:facts"));
        assert!(stripped.contains("some text before"));
        assert!(stripped.contains("some text after"));
    }

    #[test]
    fn strip_keeps_only_world() {
        let body = r#"<!--- zbrain:facts:begin -->
| # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |
|---|---|---|---|---|---|---|---|---|---|
| 1 | private fact | fact | 0.5 | private | low | 2025 |  |  |  |
| 2 | world fact | fact | 0.9 | world | high | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let stripped = strip_facts_fence(
            body,
            &StripFactsFenceOpts {
                keep_visibility: Some(vec![FactVisibility::World]),
            },
        );
        assert!(!stripped.contains("private fact"));
        assert!(stripped.contains("world fact"));
        assert!(stripped.contains("<!--- zbrain:facts:begin -->"));
    }

    #[test]
    fn strip_noop_when_no_fence() {
        let body = "just text, no fence";
        assert_eq!(
            strip_facts_fence(body, &StripFactsFenceOpts::default()),
            body
        );
    }

    #[test]
    fn strip_empty_keep_visibility_removes_whole_fence() {
        let body = r#"<!--- zbrain:facts:begin -->
| 1 | test | fact | 0.5 | world | high | 2025 |  |  |  |
<!--- zbrain:facts:end -->"#;

        let stripped = strip_facts_fence(
            body,
            &StripFactsFenceOpts {
                keep_visibility: Some(vec![]),
            },
        );
        assert!(!stripped.contains("test"));
        assert!(!stripped.contains("zbrain:facts"));
    }
}
