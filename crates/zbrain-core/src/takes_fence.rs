//! Fence parser/renderer for `## Takes` markdown tables.
//!
//! Port of `src/core/takes-fence.ts`. Markdown is the source of truth (git is
//! canonical). The DB takes table is a derived index. This module is the
//! boundary between them.
//!
//! Fence shape (HTML-comment markers):
//!
//! ```markdown
//! ## Takes
//!
//! <!--- zbrain:takes:begin -->
//! | # | claim | kind | who | weight | since | source |
//! |---|-------|------|-----|--------|-------|--------|
//! | 1 | CEO of Acme | fact | world | 1.0 | 2017-01 | Crustdata |
//! | 2 | Strong technical founder | take | garry | 0.85 | 2026-04-29 | OH |
//! <!--- zbrain:takes:end -->
//! ```
//!
//! # Shared helpers
//!
//! The row-level primitives (`parse_row_cells`, `is_separator_row`,
//! `strip_strikethrough`, `escape_fence_cell`) are defined here. When
//! `facts-fence` lands in Phase 7B, extract them into a `fence_shared` module.

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

// ── Constants ───────────────────────────────────────────────────────────────

/// HTML-comment fence markers — verbatim per spec.
pub const TAKES_FENCE_BEGIN: &str = "<!--- zbrain:takes:begin -->";
pub const TAKES_FENCE_END: &str = "<!--- zbrain:takes:end -->";

/// Seed kind values before v0.38 opened the union to `string`.
const KIND_VALUES: &[&str] = &["fact", "take", "bet", "hunch"];

/// v0.30 resolution column header tokens.
const RESOLUTION_HEADER_TOKENS: &[&str] = &["resolved", "quality", "evidence", "value", "unit", "by"];

/// TS `SLUG_SEGMENT_PATTERN` from sync.ts: `[a-z0-9._\-]+` (plus CJK chars).
/// Simplified Rust regex for holder grammar validation.
static HOLDER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:world|brain|(?:people|companies)/[a-z0-9._-]+|[a-z0-9._-]+)$")
        .expect("HOLDER_REGEX must compile")
});

/// v0.28 quality values.
const QUALITY_VALUES: &[&str] = &["correct", "incorrect", "partial", "unresolvable"];

// ── Types ───────────────────────────────────────────────────────────────────

/// A single parsed take row from the markdown fence.
#[derive(Debug, Clone, PartialEq)]
pub struct FenceTake {
    pub row_num: i32,
    pub claim: String,
    pub kind: String,
    pub holder: String,
    pub weight: f64,
    pub since_date: Option<String>,
    pub until_date: Option<String>,
    pub source: Option<String>,
    pub active: bool,
    // v0.30 resolution fields
    pub resolved_at: Option<String>,
    pub resolved_quality: Option<String>,
    pub resolved_outcome: Option<bool>,
    pub resolved_evidence: Option<String>,
    pub resolved_value: Option<f64>,
    pub resolved_unit: Option<String>,
    pub resolved_by: Option<String>,
}

/// Result of parsing a takes fence.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseResult {
    pub takes: Vec<FenceTake>,
    pub warnings: Vec<String>,
}

/// Result of `normalize_weight_for_storage`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightResult {
    pub weight: f64,
    pub clamped: bool,
}

/// Input for `upsert_take_row`.
#[derive(Debug, Clone)]
pub struct FenceTakeInput {
    pub claim: String,
    pub kind: String,
    pub holder: String,
    pub weight: Option<f64>,
    pub since_date: Option<String>,
    pub until_date: Option<String>,
    pub source: Option<String>,
    pub active: Option<bool>,
    pub row_num: Option<i32>,
}

/// Result of `upsert_take_row`.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertResult {
    pub body: String,
    pub row_num: i32,
}

/// Result of `supersede_row`.
#[derive(Debug, Clone, PartialEq)]
pub struct SupersedeResult {
    pub body: String,
    pub old_row_num: i32,
    pub new_row_num: i32,
}

// ── Shared row helpers (mirrors fence-shared.ts) ────────────────────────────

/// Parse a pipe-separated table row into trimmed cells.
/// Returns `None` if the line is not a table row.
fn parse_row_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') {
        return None;
    }
    // Must have a second pipe for at least one cell.
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

/// Detect a markdown table separator row (e.g. `|---|---|`).
fn is_separator_row(cells: &[String]) -> bool {
    if cells.is_empty() {
        return false;
    }
    cells.iter().all(|c| {
        c.chars().all(|ch| ch == '-' || ch == ':' || ch.is_whitespace())
    })
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

// ── Formatting helpers ──────────────────────────────────────────────────────

fn format_weight(w: f64) -> String {
    if (w - w.round()).abs() < f64::EPSILON {
        format!("{:.1}", w)
    } else {
        let s = format!("{:.2}", w);
        let trimmed = s.trim_end_matches('0');
        if trimmed.ends_with('.') {
            format!("{}0", trimmed)
        } else {
            trimmed.to_string()
        }
    }
}

fn parse_float_cell(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|n| n.is_finite())
}

// ── Holder grammar ──────────────────────────────────────────────────────────

/// Returns true when `holder` matches the documented grammar:
/// `world | brain | people/<slug> | companies/<slug> | <slug>`
///
/// Port of `isValidHolder` from `takes-fence.ts`.
pub fn is_valid_holder(holder: &str) -> bool {
    HOLDER_REGEX.is_match(holder)
}

// ── Weight normalization ────────────────────────────────────────────────────

/// Normalize a weight for storage. Single source of truth.
/// Pipeline:
///   1. NaN / Infinity → 0.5 (default), clamped=true.
///   2. Out of [0, 1] → clamp, clamped=true.
///   3. Round to 0.05 grid.
///
/// Port of `normalizeWeightForStorage` from `takes-fence.ts`.
pub fn normalize_weight_for_storage(raw: Option<f64>) -> WeightResult {
    let w = raw.unwrap_or(0.5);
    let mut clamped = false;

    let w = if !w.is_finite() {
        clamped = true;
        0.5
    } else if w < 0.0 || w > 1.0 {
        clamped = true;
        w.clamp(0.0, 1.0)
    } else {
        w
    };

    WeightResult {
        weight: (w * 20.0).round() / 20.0,
        clamped,
    }
}

// ── Since/Until parser ──────────────────────────────────────────────────────

fn parse_since_cell(raw: &str) -> (Option<String>, Option<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    // Range syntax: `2022-01 → 2026-06` or `2022-01 -> 2026-06`
    // Use char-level split to safely handle multi-byte UTF-8 characters.
    let parts: Vec<&str> = if trimmed.contains(" → ") {
        trimmed.splitn(2, " → ").collect()
    } else if trimmed.contains(" -> ") {
        trimmed.splitn(2, " -> ").collect()
    } else {
        return (Some(trimmed.to_string()), None);
    };
    if parts.len() == 2 {
        (Some(parts[0].trim().to_string()), Some(parts[1].trim().to_string()))
    } else {
        (Some(trimmed.to_string()), None)
    }
}

fn parse_quality_cell(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if QUALITY_VALUES.contains(&trimmed.as_str()) {
        Some(trimmed)
    } else {
        None
    }
}

// ── Core parser ─────────────────────────────────────────────────────────────

/// Parse a takes fence from a markdown body.
///
/// Returns empty takes + empty warnings when no fence is present.
/// Malformed rows are skipped with a warning; the rest still parses.
///
/// Port of `parseTakesFence` from `takes-fence.ts`.
pub fn parse_takes_fence(body: &str) -> ParseResult {
    let begin_idx = body.find(TAKES_FENCE_BEGIN);
    let end_idx = begin_idx.and_then(|bi| body[bi + TAKES_FENCE_BEGIN.len()..].find(TAKES_FENCE_END))
        .map(|rel| begin_idx.unwrap() + TAKES_FENCE_BEGIN.len() + rel);

    let mut warnings: Vec<String> = Vec::new();

    if begin_idx.is_none() && end_idx.is_none() {
        return ParseResult { takes: vec![], warnings };
    }
    let (begin_idx, end_idx) = match (begin_idx, end_idx) {
        (None, _) | (_, None) => {
            warnings.push("TAKES_FENCE_UNBALANCED: missing begin or end marker".to_string());
            return ParseResult { takes: vec![], warnings };
        }
        (Some(b), Some(e)) if e < b => {
            warnings.push("TAKES_FENCE_UNBALANCED: end marker before begin".to_string());
            return ParseResult { takes: vec![], warnings };
        }
        (Some(b), Some(e)) => (b, e),
    };

    let inner = &body[begin_idx + TAKES_FENCE_BEGIN.len()..end_idx];
    let lines: Vec<&str> = inner.lines().collect();
    let mut takes: Vec<FenceTake> = Vec::new();
    let mut saw_header = false;
    let mut resolution_col_idx: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
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
                // Detect v0.30 resolution columns
                for tok in RESOLUTION_HEADER_TOKENS {
                    if let Some(idx) = lower.iter().position(|c| c == *tok) {
                        resolution_col_idx.insert(tok.to_string(), idx);
                    }
                }
                continue;
            }
            warnings.push(format!(
                "TAKES_TABLE_MALFORMED: row before header: \"{}\"",
                line.trim()
            ));
            continue;
        }

        // Separator row
        if is_separator_row(&cells) {
            continue;
        }

        // Extend cells to at least 7 for destructuring
        let mut padded = cells.clone();
        while padded.len() < 7 {
            padded.push(String::new());
        }

        // Expect 6 cells minimum
        if cells.len() < 6 {
            warnings.push(format!(
                "TAKES_TABLE_MALFORMED: only {} cells in row \"{}\"",
                cells.len(),
                line.trim()
            ));
            continue;
        }

        let row_num_str = &padded[0];
        let row_num: i32 = match row_num_str.parse() {
            Ok(n) if n > 0 => n,
            _ => {
                warnings.push(format!(
                    "TAKES_TABLE_MALFORMED: invalid row_num \"{}\"",
                    row_num_str
                ));
                continue;
            }
        };
        if !seen_row_nums.insert(row_num) {
            warnings.push(format!(
                "TAKES_ROW_NUM_COLLISION: duplicate row_num {}",
                row_num
            ));
            continue;
        }

        let kind = padded[2].trim().to_lowercase();
        if !KIND_VALUES.contains(&kind.as_str()) {
            warnings.push(format!(
                "TAKES_TABLE_MALFORMED: unknown kind \"{}\" (expected fact|take|bet|hunch)",
                padded[2].trim()
            ));
            continue;
        }

        let holder_trimmed = padded[3].trim().to_string();
        if !holder_trimmed.is_empty() && !is_valid_holder(&holder_trimmed) {
            warnings.push(format!(
                "TAKES_HOLDER_INVALID: \"{}\" in row {} (expected: world | brain | people/<slug> | companies/<slug>)",
                holder_trimmed, row_num_str
            ));
        }

        let weight = match padded[4].trim().parse::<f64>() {
            Ok(w) if w.is_finite() => w,
            _ => {
                warnings.push(format!(
                    "TAKES_TABLE_MALFORMED: non-numeric weight \"{}\"",
                    padded[4].trim()
                ));
                continue;
            }
        };

        let (claim_text, struck) = strip_strikethrough(&padded[1]);
        let (since, until) = parse_since_cell(&padded[5]);

        let source = if padded.len() > 6 {
            parse_string_cell(&padded[6])
        } else {
            None
        };

        // Resolution columns
        let cell_at = |col: &str| -> Option<String> {
            resolution_col_idx
                .get(col)
                .and_then(|&idx| if idx < padded.len() { parse_string_cell(&padded[idx]) } else { None })
        };

        let resolved_at = cell_at("resolved");
        let quality_raw = cell_at("quality");
        let evidence_raw = cell_at("evidence");
        let value_raw = cell_at("value");
        let unit_raw = cell_at("unit");
        let by_raw = cell_at("by");

        let resolved_quality = quality_raw
            .as_ref()
            .and_then(|q| parse_quality_cell(q));
        let resolved_outcome = resolved_quality.as_ref().map(|q| match q.as_str() {
            "correct" => true,
            "incorrect" => false,
            _ => unreachable!(),
        });
        let resolved_value = value_raw
            .as_ref()
            .and_then(|v| parse_float_cell(v));

        takes.push(FenceTake {
            row_num,
            claim: claim_text,
            kind,
            holder: holder_trimmed,
            weight,
            since_date: since,
            until_date: until,
            source,
            active: !struck,
            resolved_at,
            resolved_quality,
            resolved_outcome,
            resolved_evidence: evidence_raw,
            resolved_value,
            resolved_unit: unit_raw,
            resolved_by: by_raw,
        });
    }

    if !saw_header && takes.is_empty() && lines.iter().any(|l| l.trim().starts_with('|')) {
        warnings.push(
            "TAKES_TABLE_MALFORMED: pipe-rows present but no recognizable header".to_string(),
        );
    }

    ParseResult { takes, warnings }
}

// ── Renderer ────────────────────────────────────────────────────────────────

/// Render a takes array back to a fenced markdown table.
///
/// Round-trip safe with `parse_takes_fence`. When ANY take has `resolved_quality`,
/// widens to 13 columns; otherwise stays at 7.
///
/// Port of `renderTakesFence` from `takes-fence.ts`.
pub fn render_takes_fence(takes: &[FenceTake]) -> String {
    let has_resolution = takes.iter().any(|t| t.resolved_quality.is_some());

    let (header, separator): (&str, &str) = if has_resolution {
        (
            "| # | claim | kind | who | weight | since | source | resolved | quality | evidence | value | unit | by |",
            "|---|-------|------|-----|--------|-------|--------|----------|---------|----------|-------|------|----|",
        )
    } else {
        (
            "| # | claim | kind | who | weight | since | source |",
            "|---|-------|------|-----|--------|-------|--------|",
        )
    };

    let rows: Vec<String> = takes
        .iter()
        .map(|t| {
            let claim_cell = if t.active {
                escape_fence_cell(&t.claim)
            } else {
                format!("~~{}~~", escape_fence_cell(&t.claim))
            };
            let since_cell = match (&t.since_date, &t.until_date) {
                (Some(s), Some(u)) => format!("{} → {}", escape_fence_cell(s), escape_fence_cell(u)),
                (Some(s), None) => escape_fence_cell(s),
                (None, _) => String::new(),
            };
            let w = format_weight(t.weight);
            let source = escape_fence_cell(t.source.as_deref().unwrap_or(""));

            let base = format!(
                "| {} | {} | {} | {} | {} | {} | {} |",
                t.row_num,
                claim_cell,
                t.kind,
                escape_fence_cell(&t.holder),
                w,
                since_cell,
                source,
            );

            if !has_resolution {
                return base;
            }

            let resolved = escape_fence_cell(t.resolved_at.as_deref().unwrap_or(""));
            let quality = t.resolved_quality.as_deref().unwrap_or("");
            let evidence = escape_fence_cell(t.resolved_evidence.as_deref().unwrap_or(""));
            let value = t
                .resolved_value
                .map_or(String::new(), |v| format_weight(v));
            let unit = escape_fence_cell(t.resolved_unit.as_deref().unwrap_or(""));
            let by = escape_fence_cell(t.resolved_by.as_deref().unwrap_or(""));

            format!(
                "{} {} | {} | {} | {} | {} | {} |",
                base, resolved, quality, evidence, value, unit, by,
            )
        })
        .collect();

    let inner = format!("\n{}\n{}\n{}\n", header, separator, rows.join("\n"));
    format!("{}{}{}", TAKES_FENCE_BEGIN, inner, TAKES_FENCE_END)
}

// ── Upsert ──────────────────────────────────────────────────────────────────

/// Append a new take row to the body.
///
/// If a fenced takes table exists, appends at the end. Otherwise creates a new
/// `## Takes` section + fence.
///
/// Append-only per CEO-D6 + eng-D9.
///
/// Port of `upsertTakeRow` from `takes-fence.ts`.
pub fn upsert_take_row(body: &str, new_row: &FenceTakeInput) -> UpsertResult {
    let ParseResult { takes, warnings } = parse_takes_fence(body);
    // Warnings are surfaced to caller via the return value — we don't throw
    // so writes proceed; doctor surfaces the underlying issues.
    let _ = warnings;

    let next_row_num = new_row.row_num.unwrap_or_else(|| {
        takes
            .iter()
            .map(|t| t.row_num)
            .max()
            .map_or(1, |m| m + 1)
    });

    let mut all_rows: Vec<FenceTake> = takes;
    all_rows.push(FenceTake {
        row_num: next_row_num,
        claim: new_row.claim.clone(),
        kind: new_row.kind.clone(),
        holder: new_row.holder.clone(),
        weight: new_row.weight.unwrap_or(0.5),
        since_date: new_row.since_date.clone(),
        until_date: new_row.until_date.clone(),
        source: new_row.source.clone(),
        active: new_row.active.unwrap_or(true),
        resolved_at: None,
        resolved_quality: None,
        resolved_outcome: None,
        resolved_evidence: None,
        resolved_value: None,
        resolved_unit: None,
        resolved_by: None,
    });

    let new_fence = render_takes_fence(&all_rows);

    let out = if let (Some(bi), Some(ei)) = (
        body.find(TAKES_FENCE_BEGIN),
        body.find(TAKES_FENCE_END),
    ) {
        let ei_end = ei + TAKES_FENCE_END.len();
        format!(
            "{}{}{}",
            &body[..bi],
            new_fence,
            &body[ei_end..]
        )
    } else {
        let sep = if body.ends_with('\n') { "\n" } else { "\n\n" };
        format!("{}{}## Takes\n\n{}\n", body, sep, new_fence)
    };

    UpsertResult {
        body: out,
        row_num: next_row_num,
    }
}

// ── Supersede ───────────────────────────────────────────────────────────────

/// Supersede an existing row: strike through the target row AND append a new
/// row. Both preserved in markdown for git-blame archaeology.
///
/// Returns the old and new row numbers.
///
/// Port of `supersedeRow` from `takes-fence.ts`.
pub fn supersede_row(
    body: &str,
    old_row_num: i32,
    replacement: &FenceTakeInput,
) -> Result<SupersedeResult, String> {
    let ParseResult { takes, warnings: _ } = parse_takes_fence(body);
    let idx = takes
        .iter()
        .position(|t| t.row_num == old_row_num)
        .ok_or_else(|| {
            format!(
                "supersede_row: row #{} not found in takes fence",
                old_row_num
            )
        })?;

    let new_row_num = takes.iter().map(|t| t.row_num).max().map_or(1, |m| m + 1);

    let mut updated: Vec<FenceTake> = takes
        .into_iter()
        .enumerate()
        .map(|(i, mut t)| {
            if i == idx {
                t.active = false;
            }
            t
        })
        .collect();

    updated.push(FenceTake {
        row_num: new_row_num,
        claim: replacement.claim.clone(),
        kind: replacement.kind.clone(),
        holder: replacement.holder.clone(),
        weight: replacement.weight.unwrap_or(0.5),
        since_date: replacement.since_date.clone(),
        until_date: replacement.until_date.clone(),
        source: replacement
            .source
            .clone()
            .or_else(|| Some(format!("superseded by #{}", new_row_num))),
        active: true,
        resolved_at: None,
        resolved_quality: None,
        resolved_outcome: None,
        resolved_evidence: None,
        resolved_value: None,
        resolved_unit: None,
        resolved_by: None,
    });

    let new_fence = render_takes_fence(&updated);

    let (bi, ei) = match (
        body.find(TAKES_FENCE_BEGIN),
        body.find(TAKES_FENCE_END),
    ) {
        (Some(b), Some(e)) => (b, e),
        _ => {
            return Err(
                "supersedeRow: fence markers missing in body".to_string(),
            )
        }
    };

    let ei_end = ei + TAKES_FENCE_END.len();
    let out = format!("{}{}{}", &body[..bi], new_fence, &body[ei_end..]);

    Ok(SupersedeResult {
        body: out,
        old_row_num,
        new_row_num,
    })
}

// ── Strip ───────────────────────────────────────────────────────────────────

/// Strip the fenced takes block from the body.
///
/// Used by the chunker so takes content lives ONLY in the takes table, not
/// duplicated in page chunks (privacy fix).
///
/// Port of `stripTakesFence` from `takes-fence.ts`.
pub fn strip_takes_fence(body: &str) -> String {
    let begin_idx = match body.find(TAKES_FENCE_BEGIN) {
        Some(i) => i,
        None => return body.to_string(),
    };
    let end_idx = match body[begin_idx + TAKES_FENCE_BEGIN.len()..].find(TAKES_FENCE_END) {
        Some(rel) => begin_idx + TAKES_FENCE_BEGIN.len() + rel + TAKES_FENCE_END.len(),
        None => return body.to_string(),
    };
    format!("{}{}", &body[..begin_idx], &body[end_idx..])
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Row helpers ────────────────────────────────────────────────────────

    #[test]
    fn parse_row_cells_standard() {
        let cells = parse_row_cells("| 1 | CEO | fact | world | 1.0 | 2017 | src |").unwrap();
        assert_eq!(cells.len(), 7);
        assert_eq!(cells[0], "1");
        assert_eq!(cells[1], "CEO");
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
    fn is_separator_with_colons() {
        assert!(is_separator_row(&[":---".to_string(), "---:".to_string()]));
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

    // ── Weight normalization ───────────────────────────────────────────────

    #[test]
    fn normalize_weight_default() {
        let r = normalize_weight_for_storage(None);
        assert_eq!(r.weight, 0.5);
        assert!(!r.clamped);
    }

    #[test]
    fn normalize_weight_in_range() {
        let r = normalize_weight_for_storage(Some(0.85));
        assert_eq!(r.weight, 0.85);
        assert!(!r.clamped);
    }

    #[test]
    fn normalize_weight_clamps_negative() {
        let r = normalize_weight_for_storage(Some(-0.5));
        assert_eq!(r.weight, 0.0);
        assert!(r.clamped);
    }

    #[test]
    fn normalize_weight_clamps_above_one() {
        let r = normalize_weight_for_storage(Some(1.5));
        assert_eq!(r.weight, 1.0);
        assert!(r.clamped);
    }

    #[test]
    fn normalize_weight_grid_rounding() {
        let r = normalize_weight_for_storage(Some(0.74));
        assert_eq!(r.weight, 0.75); // rounded to nearest 0.05
        assert!(!r.clamped);
    }

    #[test]
    fn normalize_weight_nan() {
        let r = normalize_weight_for_storage(Some(f64::NAN));
        assert_eq!(r.weight, 0.5);
        assert!(r.clamped);
    }

    // ── Holder grammar ─────────────────────────────────────────────────────

    #[test]
    fn holder_world_valid() {
        assert!(is_valid_holder("world"));
    }

    #[test]
    fn holder_brain_valid() {
        assert!(is_valid_holder("brain"));
    }

    #[test]
    fn holder_people_slug_valid() {
        assert!(is_valid_holder("people/garry-tan"));
    }

    #[test]
    fn holder_companies_slug_valid() {
        assert!(is_valid_holder("companies/acme.io"));
    }

    #[test]
    fn holder_rejects_uppercase() {
        assert!(!is_valid_holder("Garry"));
    }

    #[test]
    fn holder_rejects_invalid_namespace() {
        assert!(!is_valid_holder("users/garry"));
    }

    // ── Parse / Render round-trip ───────────────────────────────────────────

    #[test]
    fn parse_empty_body() {
        let result = parse_takes_fence("no fence here");
        assert!(result.takes.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn parse_and_render_roundtrip() {
        let body = r#"some content

## Takes

<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | CEO of Acme | fact | world | 1.0 | 2017-01 | Crustdata |
| 2 | Strong technical founder | take | people/garry | 0.85 | 2026-04-29 | OH |
<!--- zbrain:takes:end -->

more content"#;

        let result = parse_takes_fence(body);
        assert_eq!(result.takes.len(), 2);
        assert!(result.warnings.is_empty());

        assert_eq!(result.takes[0].row_num, 1);
        assert_eq!(result.takes[0].claim, "CEO of Acme");
        assert_eq!(result.takes[0].kind, "fact");
        assert_eq!(result.takes[0].holder, "world");
        assert_eq!(result.takes[0].weight, 1.0);
        assert_eq!(result.takes[0].since_date.as_deref(), Some("2017-01"));
        assert_eq!(result.takes[0].source.as_deref(), Some("Crustdata"));
        assert!(result.takes[0].active);

        assert_eq!(result.takes[1].row_num, 2);
        assert_eq!(result.takes[1].claim, "Strong technical founder");
        assert_eq!(result.takes[1].kind, "take");
        assert_eq!(result.takes[1].holder, "people/garry");

        // Render and re-parse
        let rendered = render_takes_fence(&result.takes);
        let reparsed = parse_takes_fence(&rendered);
        assert_eq!(reparsed.takes.len(), 2);
        assert_eq!(reparsed.takes[0].claim, "CEO of Acme");
        assert_eq!(reparsed.takes[1].claim, "Strong technical founder");
    }

    #[test]
    fn parse_strikethrough_active_false() {
        let body = r#"<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | ~~old claim~~ | take | world | 0.5 | 2024 | |
<!--- zbrain:takes:end -->"#;

        let result = parse_takes_fence(body);
        assert_eq!(result.takes.len(), 1);
        assert_eq!(result.takes[0].claim, "old claim");
        assert!(!result.takes[0].active);
    }

    #[test]
    fn parse_date_range() {
        let body = r#"<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | test | take | world | 0.5 | 2022-01 → 2026-06 | |
<!--- zbrain:takes:end -->"#;

        let result = parse_takes_fence(body);
        assert_eq!(result.takes[0].since_date.as_deref(), Some("2022-01"));
        assert_eq!(result.takes[0].until_date.as_deref(), Some("2026-06"));
    }

    #[test]
    fn parse_date_range_arrow_syntax() {
        let body = r#"<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | test | take | world | 0.5 | 2022-01 -> 2026-12 | |
<!--- zbrain:takes:end -->"#;

        let result = parse_takes_fence(body);
        assert_eq!(result.takes[0].since_date.as_deref(), Some("2022-01"));
        assert_eq!(result.takes[0].until_date.as_deref(), Some("2026-12"));
    }

    #[test]
    fn parse_with_resolution_columns() {
        let body = r#"<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source | resolved | quality | evidence | value | unit | by |
|---|---|-------|------|-----|--------|-------|--------|----------|---------|----------|-------|------|----|
| 1 | test claim | take | world | 0.7 | 2025 | source | 2026-07 | correct | strong evidence | 1.0 | pct | garry |
<!--- zbrain:takes:end -->"#;

        let result = parse_takes_fence(body);
        assert_eq!(result.takes.len(), 1);
        assert_eq!(result.takes[0].resolved_at.as_deref(), Some("2026-07"));
        assert_eq!(result.takes[0].resolved_quality.as_deref(), Some("correct"));
        assert_eq!(result.takes[0].resolved_outcome, Some(true));
        assert_eq!(
            result.takes[0].resolved_evidence.as_deref(),
            Some("strong evidence")
        );
        assert_eq!(result.takes[0].resolved_value, Some(1.0));
        assert_eq!(result.takes[0].resolved_unit.as_deref(), Some("pct"));
        assert_eq!(result.takes[0].resolved_by.as_deref(), Some("garry"));
    }

    #[test]
    fn render_widens_for_resolution() {
        let mut t = FenceTake {
            row_num: 1,
            claim: "test".into(),
            kind: "fact".into(),
            holder: "world".into(),
            weight: 0.5,
            since_date: None,
            until_date: None,
            source: None,
            active: true,
            resolved_at: None,
            resolved_quality: Some("correct".into()),
            resolved_outcome: None,
            resolved_evidence: None,
            resolved_value: None,
            resolved_unit: None,
            resolved_by: None,
        };
        let rendered = render_takes_fence(&[t.clone()]);
        assert!(rendered.contains("resolved | quality"));
        // Re-parse should recover quality
        let reparsed = parse_takes_fence(&rendered);
        assert_eq!(
            reparsed.takes[0].resolved_quality.as_deref(),
            Some("correct")
        );
    }

    #[test]
    fn upsert_creates_new_fence() {
        let body = "just some text";
        let result = upsert_take_row(
            body,
            &FenceTakeInput {
                claim: "new claim".into(),
                kind: "fact".into(),
                holder: "world".into(),
                weight: Some(0.8),
                since_date: Some("2026".into()),
                until_date: None,
                source: Some("test".into()),
                active: None,
                row_num: None,
            },
        );
        assert!(result.body.contains("## Takes"));
        assert!(result.body.contains("<!--- zbrain:takes:begin -->"));
        assert!(result.body.contains("new claim"));
        assert_eq!(result.row_num, 1);
    }

    #[test]
    fn upsert_appends_to_existing_fence() {
        let body = r#"some text

## Takes

<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | existing | fact | world | 0.5 | 2025 | |
<!--- zbrain:takes:end -->
"#;

        let result = upsert_take_row(
            body,
            &FenceTakeInput {
                claim: "appended".into(),
                kind: "take".into(),
                holder: "brain".into(),
                weight: None,
                since_date: None,
                until_date: None,
                source: None,
                active: None,
                row_num: None,
            },
        );
        assert!(result.body.contains("existing"));
        assert!(result.body.contains("appended"));
        assert_eq!(result.row_num, 2);

        // Verify round-trip: re-parse
        let reparsed = parse_takes_fence(&result.body);
        assert_eq!(reparsed.takes.len(), 2);
    }

    #[test]
    fn supersede_strikes_old_and_appends_new() {
        let body = r#"<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | old claim | take | world | 0.5 | 2025 | |
<!--- zbrain:takes:end -->"#;

        let result = supersede_row(
            body,
            1,
            &FenceTakeInput {
                claim: "new claim".into(),
                kind: "take".into(),
                holder: "world".into(),
                weight: Some(0.9),
                since_date: Some("2026".into()),
                until_date: None,
                source: None,
                active: None,
                row_num: None,
            },
        )
        .unwrap();

        assert_eq!(result.old_row_num, 1);
        assert_eq!(result.new_row_num, 2);
        assert!(result.body.contains("~~old claim~~"));
        assert!(result.body.contains("new claim"));

        // Verify re-parse
        let reparsed = parse_takes_fence(&result.body);
        assert_eq!(reparsed.takes.len(), 2);
        let old = reparsed.takes.iter().find(|t| t.row_num == 1).unwrap();
        let new = reparsed.takes.iter().find(|t| t.row_num == 2).unwrap();
        assert!(!old.active);
        assert!(new.active);
        assert_eq!(new.claim, "new claim");
    }

    #[test]
    fn supersede_errors_on_missing_row() {
        let body = r#"<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | only row | take | world | 0.5 | 2025 | |
<!--- zbrain:takes:end -->"#;

        let result = supersede_row(
            body,
            99,
            &FenceTakeInput {
                claim: "new".into(),
                kind: "take".into(),
                holder: "world".into(),
                weight: None,
                since_date: None,
                until_date: None,
                source: None,
                active: None,
                row_num: None,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn strip_removes_fence() {
        let body = r#"some text before

## Takes

<!--- zbrain:takes:begin -->
| # | claim | kind | who | weight | since | source |
|---|---|-------|------|-----|--------|-------|--------|
| 1 | test | fact | world | 0.5 | 2025 | |
<!--- zbrain:takes:end -->

some text after"#;

        let stripped = strip_takes_fence(body);
        assert!(!stripped.contains("test"));
        assert!(!stripped.contains("zbrain:takes"));
        assert!(stripped.contains("some text before"));
        assert!(stripped.contains("some text after"));
    }

    #[test]
    fn strip_noop_when_no_fence() {
        let body = "just text, no fence";
        assert_eq!(strip_takes_fence(body), body);
    }
}
