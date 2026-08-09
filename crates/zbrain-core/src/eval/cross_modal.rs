//! cross-modal-eval — multi-model quality gate (Rust port of TS
//! `src/core/cross-modal-eval/*` + `src/core/eval-shared/json-repair.ts`).
//!
//! Faithful reimplementation of the v0.27.x cross-modal eval. Three
//! different-provider frontier models score an OUTPUT against a TASK on a
//! fixed dimension list; `aggregate()` produces a PASS / FAIL / INCONCLUSIVE
//! verdict. Receipts are bound to `(slug, content sha-8)` so a later audit
//! can tell whether the receipt is current or stale.
//!
//! The LLM transport is injected (`run_eval(opts, &chat)`) so the harness is
//! hermetic and unit-testable without a live provider (mirrors
//! `run_code_retrieval_eval`). The CLI wires `chat` to a real `ChatProvider`
//! resolved per `provider:model` slot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{anyhow, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Compiled regexes (module-level so a hot parse loop never recompiles them)
// ---------------------------------------------------------------------------

/// TS `FENCE_RE` — ```` ```json ... ``` ```` fences, case-insensitive.
static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)```(?:json)?\s*\n?(.*?)```").expect("FENCE_RE must compile")
});
/// Trailing commas before `}` or `]`.
static TRAILING_COMMA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r",(\s*[}\]])").expect("TRAILING_COMMA_RE must compile"));
/// Single-quoted delimiters between structural punctuation.
static SINGLE_QUOTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([:\{,\[]\s*)'([^']*?)'(\s*[,\}\]:])").expect("SINGLE_QUOTE_RE must compile")
});
/// Unescaped newline inside a double-quoted string.
static EMBEDDED_NL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"("(?:[^"\\]|\\.)*?)\n((?:[^"\\]|\\.)*?")"#)
        .expect("EMBEDDED_NL_RE must compile")
});
/// Nuclear option: `"<dim>": { ... "score": N }`.
static SCORE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"["']?(\w[\w_-]*)["']?\s*:\s*\{[^}]*?["']?score["']?\s*:\s*(\d+(?:\.\d+)?)"#)
        .expect("SCORE_RE must compile")
});
/// Nuclear option: numbered `"N. ..."` improvement strings.
static IMPROVEMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""(\d+\.\s[^"]{10,})""#).expect("IMPROVEMENT_RE must compile"));
/// Nuclear option: a loose `"overall": N`.
static OVERALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"["']?overall["']?\s*:\s*(\d+(?:\.\d+)?)"#).expect("OVERALL_RE must compile")
});
/// Receipt slug charset.
static SLUG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_-]*$").expect("SLUG_RE must compile"));
/// `skills/<slug>/SKILL.md` inside an arbitrary `--output` path.
static OUTPUT_SLUG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|/)skills/([^/]+)/SKILL\.md$").expect("OUTPUT_SLUG_RE must compile")
});
/// Canonical receipt filename `<slug>-<8hex>.json`.
static RECEIPT_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_-]*-[0-9a-f]{8}\.json$")
        .expect("RECEIPT_FILE_RE must compile")
});
/// The sha-8 tail of a receipt filename.
static SHA8_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{8}$").expect("SHA8_RE must compile"));

// ===========================================================================
// Shared JSON parsing (port of eval-shared/json-repair.ts: 4-strategy parser)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParsedScore {
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParsedModelResult {
    pub scores: BTreeMap<String, ParsedScore>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overall: Option<f64>,
    pub improvements: Vec<String>,
    /// True when reconstructed via the regex nuclear option.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repaired: bool,
}

/// Best-effort JSON parser for LLM output. Four-strategy fallback chain:
/// 1. strip ```json fences + JSON.parse
/// 2. extract first `{...}` substring
/// 3. repair common LLM-JSON mistakes (trailing commas, single quotes)
/// 4. nuclear regex extraction of scores + improvements.
///
/// Throws (returns Err) if no dimension scores are recoverable — better than
/// fabricating a fake PASS. The aggregator treats an Err here as "this model
/// contributed nothing this cycle".
pub fn parse_model_json(raw: &str) -> Result<ParsedModelResult> {
    if raw.trim().is_empty() {
        return Err(anyhow!("parseModelJSON: empty or non-string input"));
    }
    {
        let cleaned = strip_fences(raw).trim().to_string();
        if let Some(direct) = try_parse(&cleaned) {
            return shape(&direct);
        }
        // TS: /\{[\s\S]*\}/ — greedy, first `{` through the LAST `}`.
        let obj = match (cleaned.find('{'), cleaned.rfind('}')) {
            (Some(start), Some(end)) if end > start => &cleaned[start..=end],
            _ => return Err(anyhow!("parseModelJSON: no JSON object found in input")),
        };
        if let Some(second) = try_parse(obj) {
            return shape(&second);
        }
        let fixed = repair_json(obj);
        if let Some(third) = try_parse(&fixed) {
            return shape(&third);
        }
        if let Some(reconstructed) = regex_nuclear_option(obj) {
            return Ok(reconstructed);
        }
    }
    Err(anyhow!("parseModelJSON: all repair strategies failed"))
}

fn strip_fences(s: &str) -> String {
    // Lazily build the fence regex (``` optionally followed by "json").
    match FENCE_RE.captures(s) {
        Some(c) => c.get(1).map(|m| m.as_str().to_string()).unwrap_or_else(|| s.to_string()),
        None => s.to_string(),
    }
}

fn try_parse(s: &str) -> Option<Value> {
    serde_json::from_str::<Value>(s).ok()
}

fn repair_json(s: &str) -> String {
    let mut out = TRAILING_COMMA_RE.replace_all(s, "$1").into_owned();
    // Single-quoted delimiters around keys/values, only between structural
    // punctuation (avoids touching apostrophes inside legit double-quoted
    // strings). TS uses lookbehind/lookahead; the `regex` crate has neither,
    // so we capture the boundaries and re-emit them — which means adjacent
    // pairs sharing a boundary char (`{'a':'b'}`) need more than one pass.
    // Bounded fixed point: each pass converts every non-overlapping match.
    for _ in 0..4 {
        let next = SINGLE_QUOTE_RE.replace_all(&out, "$1\"$2\"$3").into_owned();
        if next == out {
            break;
        }
        out = next;
    }
    EMBEDDED_NL_RE.replace_all(&out, "$1\\n$2").into_owned()
}

/// Last-resort: scan for `"<dim>": { ... "score": N }` and numbered
/// `"N. ..."` improvements. Returns None if zero scores recoverable.
fn regex_nuclear_option(obj: &str) -> Option<ParsedModelResult> {
    let mut scores: BTreeMap<String, ParsedScore> = BTreeMap::new();
    for m in SCORE_RE.captures_iter(obj) {
        let (Some(dim), Some(raw)) = (m.get(1), m.get(2)) else {
            continue;
        };
        // TS guards with Number.isFinite — a bad capture skips that dim only.
        let Ok(num) = raw.as_str().parse::<f64>() else {
            continue;
        };
        if !num.is_finite() {
            continue;
        }
        scores.insert(dim.as_str().to_string(), ParsedScore { score: num, feedback: None });
    }
    if scores.is_empty() {
        return None;
    }
    let improvements: Vec<String> = IMPROVEMENT_RE
        .captures_iter(obj)
        .filter_map(|m| m.get(1).map(|g| g.as_str().to_string()))
        .collect();
    let overall = OVERALL_RE
        .captures(obj)
        .and_then(|c| c.get(1))
        .and_then(|g| g.as_str().parse::<f64>().ok());
    Some(ParsedModelResult {
        scores,
        overall,
        improvements: if improvements.is_empty() {
            vec!["(could not parse improvements from malformed JSON)".to_string()]
        } else {
            improvements
        },
        repaired: true,
    })
}

fn shape(parsed: &Value) -> Result<ParsedModelResult> {
    if !parsed.is_object() {
        return Err(anyhow!("parseModelJSON: parsed value is not an object"));
    }
    let p = parsed.as_object().expect("object");
    let scores_raw = p.get("scores").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut scores: BTreeMap<String, ParsedScore> = BTreeMap::new();
    for (dim, v) in scores_raw {
        match v {
            Value::Number(n) => {
                if let Some(score) = n.as_f64() {
                    scores.insert(dim, ParsedScore { score, feedback: None });
                }
            }
            Value::Object(o) => {
                // TS: `typeof vv.score === 'number' ? vv.score : Number(vv.score)`
                // — a numeric *string* is coerced; anything non-finite is skipped.
                let score = match o.get("score") {
                    Some(Value::Number(n)) => n.as_f64(),
                    Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
                    _ => None,
                };
                let score = match score {
                    Some(s) if s.is_finite() => s,
                    _ => continue,
                };
                let feedback = o
                    .get("feedback")
                    .and_then(|f| f.as_str())
                    .map(|s| s.to_string());
                scores.insert(dim, ParsedScore { score, feedback });
            }
            _ => {}
        }
    }
    let improvements = p
        .get("improvements")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let overall = p.get("overall").and_then(|v| v.as_f64());
    if scores.is_empty() {
        return Err(anyhow!("parseModelJSON: parsed object has no usable scores"));
    }
    Ok(ParsedModelResult {
        scores,
        overall,
        improvements,
        repaired: false,
    })
}

// ===========================================================================
// Aggregate (port of cross-modal-eval/aggregate.ts)
// ===========================================================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    MeanBelow7,
    MinBelow5,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlotResult {
    pub ok: bool,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<ParsedModelResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DimensionRoll {
    pub mean: f64,
    pub min: f64,
    pub scores: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_reason: Option<FailReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AggregateResult {
    pub verdict: Verdict,
    pub successes: usize,
    pub failures: usize,
    pub dimensions: BTreeMap<String, DimensionRoll>,
    pub overall: Option<f64>,
    pub top_improvements: Vec<String>,
    pub errors: Vec<ReceiptError>,
    pub verdict_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReceiptError {
    pub model_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateInput {
    pub slots: Vec<SlotResult>,
}

const PASS_MEAN_THRESHOLD: f64 = 7.0;
const PASS_FLOOR_THRESHOLD: f64 = 5.0;
const MIN_SUCCESSES_FOR_VERDICT: usize = 2;
const TOP_IMPROVEMENTS_CAP: usize = 10;
const DEDUP_PREFIX_LEN: usize = 40;

/// Verdict logic for one cycle.
///
/// Pass criterion:
/// - At least 2 of N model calls succeeded with parseable scores.
/// - Every dimension's mean across successful models >= 7.
/// - For every dimension, no successful model scored < 5.
///
/// Inconclusive: fewer than 2 models succeeded.
pub fn aggregate(input: &AggregateInput) -> AggregateResult {
    let successes: Vec<&SlotResult> = input.slots.iter().filter(|s| s.ok).collect();
    let failures: Vec<&SlotResult> = input.slots.iter().filter(|s| !s.ok).collect();
    let errors = failures
        .iter()
        .filter_map(|f| f.error.clone().map(|e| ReceiptError { model_id: f.model_id.clone(), error: e }))
        .collect::<Vec<_>>();

    if successes.len() < MIN_SUCCESSES_FOR_VERDICT {
        return AggregateResult {
            verdict: Verdict::Inconclusive,
            successes: successes.len(),
            failures: failures.len(),
            dimensions: BTreeMap::new(),
            overall: None,
            top_improvements: vec![],
            errors,
            verdict_message: format!(
                "INCONCLUSIVE: only {} of {} models returned parseable scores (need >= {}). See receipt for per-slot errors.",
                successes.len(),
                input.slots.len(),
                MIN_SUCCESSES_FOR_VERDICT
            ),
        };
    }

    let mut all_dim_names: BTreeMap<String, ()> = BTreeMap::new();
    for s in &successes {
        if let Some(parsed) = &s.parsed {
            for dim in parsed.scores.keys() {
                all_dim_names.insert(dim.clone(), ());
            }
        }
    }

    let mut dimensions: BTreeMap<String, DimensionRoll> = BTreeMap::new();
    for dim in all_dim_names.keys() {
        let mut scores: Vec<f64> = vec![];
        for s in &successes {
            if let Some(parsed) = &s.parsed {
                if let Some(entry) = parsed.scores.get(dim) {
                    scores.push(entry.score);
                }
            }
        }
        if scores.is_empty() {
            continue;
        }
        let mean = round1(scores.iter().sum::<f64>() / scores.len() as f64);
        let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
        let mut roll = DimensionRoll { mean, min, scores, fail_reason: None };
        if roll.mean < PASS_MEAN_THRESHOLD {
            roll.fail_reason = Some(FailReason::MeanBelow7);
        } else if roll.min < PASS_FLOOR_THRESHOLD {
            roll.fail_reason = Some(FailReason::MinBelow5);
        }
        dimensions.insert(dim.clone(), roll);
    }

    let dim_rolls: Vec<&DimensionRoll> = dimensions.values().collect();
    let overall = if dim_rolls.is_empty() {
        None
    } else {
        Some(round1(dim_rolls.iter().map(|d| d.mean).sum::<f64>() / dim_rolls.len() as f64))
    };
    let all_dims_pass = dimensions.values().all(|d| d.fail_reason.is_none());
    let verdict = if all_dims_pass { Verdict::Pass } else { Verdict::Fail };

    let top_improvements = dedup_improvements(
        successes
            .iter()
            .filter_map(|s| s.parsed.as_ref().map(|p| p.improvements.clone()))
            .flatten()
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .take(TOP_IMPROVEMENTS_CAP)
    .collect::<Vec<_>>();

    let verdict_message = match verdict {
        Verdict::Pass => format!(
            "PASS: every dimension mean >={} and min >={} across {}/{} models. Overall {}/10.",
            PASS_MEAN_THRESHOLD as i32,
            PASS_FLOOR_THRESHOLD as i32,
            successes.len(),
            input.slots.len(),
            overall.map(|o| o as i32).unwrap_or(0)
        ),
        Verdict::Fail => describe_failure(&dimensions, successes.len(), input.slots.len(), overall),
        Verdict::Inconclusive => unreachable!("verdict is Pass/Fail when successes >= MIN"),
    };

    AggregateResult {
        verdict,
        successes: successes.len(),
        failures: failures.len(),
        dimensions,
        overall,
        top_improvements,
        errors,
        verdict_message,
    }
}

fn describe_failure(
    dimensions: &BTreeMap<String, DimensionRoll>,
    successes: usize,
    total: usize,
    overall: Option<f64>,
) -> String {
    let failed: Vec<(&String, &DimensionRoll)> =
        dimensions.iter().filter(|(_, d)| d.fail_reason.is_some()).collect();
    if failed.is_empty() {
        return "FAIL: aggregate failure with no dimension flagged (likely zero dimensions returned).".to_string();
    }
    let reasons = failed
        .iter()
        .map(|(name, d)| match d.fail_reason {
            Some(FailReason::MeanBelow7) => format!("{} mean={} (<{})", name, d.mean, PASS_MEAN_THRESHOLD as i32),
            Some(FailReason::MinBelow5) => {
                format!("{} min={} (<{}; scores=[{}])", name, d.min, PASS_FLOOR_THRESHOLD as i32, d.scores.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", "))
            }
            None => name.to_string(),
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "FAIL across {}/{} models. Overall {}/10. Failing: {}.",
        successes,
        total,
        overall.map(|o| o as i32).unwrap_or(0),
        reasons
    )
}

fn dedup_improvements(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for item in items {
        let key = item
            .chars()
            .take(DEDUP_PREFIX_LEN)
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect::<String>()
            .to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(item);
    }
    out
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

// ===========================================================================
// Receipt naming (port of cross-modal-eval/receipt-name.ts)
// ===========================================================================

/// SHA-256 of content, truncated to 8 hex chars.
pub fn sha8(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let hex = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
    hex[..8].to_string()
}

/// Canonical receipt filename: `<slug>-<sha8>.json`.
pub fn receipt_name(slug: &str, content: &str) -> Result<String> {
    if slug.is_empty() {
        return Err(anyhow!("receiptName: slug required"));
    }
    if !SLUG_RE.is_match(slug) {
        return Err(anyhow!("receiptName: slug must be alphanumeric/dash/underscore; got: {slug}"));
    }
    Ok(format!("{}-{}.json", slug, sha8(content)))
}

/// Pull the slug out of a SKILL.md path (immediate parent directory name).
pub fn infer_slug_from_skill_path(skill_md_path: &str) -> Result<String> {
    let normalized = skill_md_path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    let last = parts.last().ok_or_else(|| anyhow!("inferSlugFromSkillPath: empty path"))?;
    if *last != "SKILL.md" {
        return Err(anyhow!(
            "inferSlugFromSkillPath: expected path ending in SKILL.md; got: {skill_md_path}"
        ));
    }
    let parent = parts
        .get(parts.len().wrapping_sub(2))
        .ok_or_else(|| anyhow!("inferSlugFromSkillPath: cannot infer slug — no parent directory in: {skill_md_path}"))?;
    Ok(parent.to_string())
}

/// Infer a slug from an `--output` path shaped like `skills/<slug>/SKILL.md`.
///
/// Faithful port of the TS CLI helper `inferSlugFromOutputPath`: unlike
/// [`infer_slug_from_skill_path`] this is a *best-effort* lookup that returns
/// `None` (rather than an error) for any other shape, letting `run_eval` fall
/// back to a content-hash slug.
pub fn infer_slug_from_output_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    OUTPUT_SLUG_RE
        .captures(&normalized)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// True when the filename matches `<slug>-<8hex>.json`.
pub fn is_receipt_file(path: &str) -> bool {
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
    RECEIPT_FILE_RE.is_match(name)
}

/// All receipts for a slug in `dir`, ordered newest-first (by mtime).
pub fn list_receipts_for_slug(slug: &str, dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return vec![];
    }
    let prefix = format!("{slug}-");
    let mut out: Vec<(PathBuf, std::time::SystemTime)> = vec![];
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((path, mtime));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out.into_iter().map(|(p, _)| p).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReceiptStatus {
    Found { path: String, sha: String },
    Stale { latest_path: String, latest_sha: String, current_sha: String },
    Missing { current_sha: String },
}

/// Read the SKILL.md at `skill_path` and look in `receipt_dir` for a receipt
/// matching the slug embedded in the path.
pub fn find_receipt_for_skill(skill_md_path: &Path, receipt_dir: &Path) -> ReceiptStatus {
    if !skill_md_path.exists() {
        return ReceiptStatus::Missing { current_sha: String::new() };
    }
    let content = match std::fs::read_to_string(skill_md_path) {
        Ok(c) => c,
        Err(_) => return ReceiptStatus::Missing { current_sha: String::new() },
    };
    let slug = match infer_slug_from_skill_path(
        skill_md_path.to_str().unwrap_or(""),
    ) {
        Ok(s) => s,
        Err(_) => return ReceiptStatus::Missing { current_sha: sha8(&content) },
    };
    let current_sha = sha8(&content);
    let expected_name = format!("{slug}-{current_sha}.json");
    let expected_path = receipt_dir.join(&expected_name);

    if expected_path.exists() {
        return ReceiptStatus::Found {
            path: expected_path.to_string_lossy().to_string(),
            sha: current_sha,
        };
    }
    if !receipt_dir.exists() {
        return ReceiptStatus::Missing { current_sha };
    }
    let prefix = format!("{slug}-");
    let mut matches: Vec<(PathBuf, std::time::SystemTime)> = vec![];
    if let Ok(entries) = std::fs::read_dir(receipt_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with(&prefix) || !name.ends_with(".json") {
                continue;
            }
            let sha = name[prefix.len()..name.len() - ".json".len()].to_string();
            if sha == current_sha {
                continue;
            }
            if !SHA8_RE.is_match(&sha) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                matches.push((path, mtime));
            }
        }
    }
    if matches.is_empty() {
        return ReceiptStatus::Missing { current_sha };
    }
    matches.sort_by(|a, b| b.1.cmp(&a.1));
    let latest = &matches[0];
    ReceiptStatus::Stale {
        latest_path: latest.0.to_string_lossy().to_string(),
        latest_sha: latest
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n[prefix.len()..n.len() - ".json".len()].to_string())
            .unwrap_or_default(),
        current_sha,
    }
}

// ===========================================================================
// Receipt writing (port of cross-modal-eval/receipt-write.ts)
// ===========================================================================

/// Auto-mkdir receipt writer (the parent dir may not exist on first run).
pub fn write_receipt<T: Serialize>(path: &Path, content: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(content)?;
    std::fs::write(path, body)?;
    Ok(())
}

// ===========================================================================
// Runner (port of cross-modal-eval/runner.ts)
// ===========================================================================

pub const RECEIPT_SCHEMA_VERSION: u8 = 1;

/// Default dimensions match the v1.1.0 SKILL.md.
pub const DEFAULT_DIMENSIONS: &[&str] = &[
    "GOAL_ACHIEVEMENT — Does the output actually accomplish what the task asked for?",
    "DEPTH — Is the output substantive, or surface-level / thin?",
    "SOURCING — Are claims backed by evidence, links, or citations?",
    "SPECIFICITY — Are there concrete details, data, quotes, examples?",
    "USEFULNESS — Would the intended audience find this valuable?",
];

/// Default 3-provider slot configuration (distinct families = decorrelated
/// blind spots). Override via `--slot-a-model` / `--slot-b-model` / `--slot-c-model`.
pub const DEFAULT_SLOTS: &[(&str, &str)] = &[
    ("A", "openai:gpt-4o"),
    ("B", "anthropic:claude-opus-4-7"),
    ("C", "google:gemini-1.5-pro"),
];

/// Owned form of [`DEFAULT_DIMENSIONS`].
pub fn default_dimensions() -> Vec<String> {
    DEFAULT_DIMENSIONS.iter().map(|s| (*s).to_string()).collect()
}

/// Owned form of [`DEFAULT_SLOTS`].
pub fn default_slots() -> Vec<SlotConfig> {
    DEFAULT_SLOTS
        .iter()
        .map(|(id, model)| SlotConfig { id: (*id).to_string(), model: (*model).to_string() })
        .collect()
}

#[derive(Debug, Clone)]
pub struct SlotConfig {
    pub id: String,
    /// "<provider>:<modelId>" consumed by the chat resolver.
    pub model: String,
}

/// Note: no `Debug`/`Clone` derive — `on_progress` holds a boxed closure.
pub struct RunEvalOpts {
    pub task: String,
    pub output: String,
    /// Optional skill slug for receipt naming (falls back to a content sha).
    pub slug: Option<String>,
    /// Override default dimensions list.
    pub dimensions: Option<Vec<String>>,
    /// Override default 3 slots.
    pub slots: Option<Vec<SlotConfig>>,
    /// 1-3; defaults to 3 in TTY, 1 in non-TTY (handled by caller).
    pub cycles: Option<u32>,
    /// Where receipts are written.
    pub receipt_dir: PathBuf,
    /// Per-call max output tokens (default 4000).
    pub max_tokens: Option<u32>,
    /// Optional progress callback.
    pub on_progress: Option<Box<dyn Fn(ProgressEvent) + Send + Sync>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    CycleStart { cycle: u32, total: u32 },
    SlotDone { cycle: u32, slot_id: String, model_id: String, ok: bool, ms: u64 },
    CycleEnd { cycle: u32, verdict: Verdict },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptSlot {
    pub id: String,
    pub model: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<ParsedModelResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleReceipt {
    pub schema_version: u8,
    pub cycle: u32,
    pub task: String,
    pub output_sha8: String,
    pub slug: String,
    pub timestamp: String,
    pub dimensions: Vec<String>,
    pub slots: Vec<ReceiptSlot>,
    pub aggregate: AggregateResult,
    pub receipt_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEvalResult {
    /// Last cycle's aggregate (the verdict that drives exit code).
    pub final_aggregate: AggregateResult,
    /// Receipt for each cycle that ran.
    pub cycles: Vec<CycleReceipt>,
    /// Path of the LAST cycle's receipt (binds the current sha).
    pub final_receipt_path: String,
}

/// Single LLM call request handed to the injected `chat` function.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub prompt: String,
    pub max_tokens: u32,
}

/// Run up to `cycles` cycles. Stops early on PASS or INCONCLUSIVE.
pub async fn run_eval<F, Fut>(opts: &RunEvalOpts, chat: &F) -> Result<RunEvalResult>
where
    F: Fn(ChatRequest) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let dimensions = opts.dimensions.clone().unwrap_or_else(default_dimensions);
    let slots = opts.slots.clone().unwrap_or_else(default_slots);
    let cycles = clamp_cycles(opts.cycles);
    let out_sha = sha8(&opts.output);
    let slug = opts
        .slug
        .clone()
        .unwrap_or_else(|| format!("eval-{}", &out_sha[..6.min(out_sha.len())]));
    let max_tokens = opts.max_tokens.unwrap_or(4000);

    let mut cycle_receipts: Vec<CycleReceipt> = vec![];
    let mut final_aggregate: Option<AggregateResult> = None;
    let mut final_receipt_path = String::new();

    for cycle in 1..=cycles {
        if let Some(cb) = &opts.on_progress {
            cb(ProgressEvent::CycleStart { cycle, total: cycles });
        }

        let slot_results = run_one_cycle(opts, &dimensions, &slots, max_tokens, chat).await;

        let agg = aggregate(&AggregateInput { slots: slot_results.clone() });
        final_aggregate = Some(agg.clone());

        let base_name = receipt_name(&slug, &opts.output)?;
        let receipt_file = if cycle == 1 {
            base_name
        } else {
            base_name.replace(".json", &format!(".cycle{cycle}.json"))
        };
        let receipt_path = opts.receipt_dir.join(&receipt_file);

        let receipt = CycleReceipt {
            schema_version: RECEIPT_SCHEMA_VERSION,
            cycle,
            task: opts.task.clone(),
            output_sha8: sha8(&opts.output),
            slug: slug.clone(),
            timestamp: now_rfc3339(),
            dimensions: dimensions.clone(),
            slots: slot_results
                .iter()
                .map(|s| ReceiptSlot {
                    id: s.model_id.split(':').next().unwrap_or(&s.model_id).to_uppercase().chars().next().unwrap_or('?').to_string(),
                    model: s.model_id.clone(),
                    ok: s.ok,
                    error: if s.ok { None } else { s.error.clone() },
                    parsed: if s.ok { s.parsed.clone() } else { None },
                })
                .collect(),
            aggregate: agg.clone(),
            receipt_path: receipt_path.to_string_lossy().to_string(),
        };

        write_receipt(&receipt_path, &receipt)?;
        cycle_receipts.push(receipt);
        final_receipt_path = receipt_path.to_string_lossy().to_string();

        if let Some(cb) = &opts.on_progress {
            cb(ProgressEvent::CycleEnd { cycle, verdict: agg.verdict });
        }

        if matches!(agg.verdict, Verdict::Pass | Verdict::Inconclusive) {
            break;
        }
    }

    final_aggregate
        .map(|fa| RunEvalResult { final_aggregate: fa, cycles: cycle_receipts, final_receipt_path })
        .ok_or_else(|| anyhow!("runEval: no cycles ran"))
}

async fn run_one_cycle<F, Fut>(
    opts: &RunEvalOpts,
    dimensions: &[String],
    slots: &[SlotConfig],
    max_tokens: u32,
    chat: &F,
) -> Vec<SlotResult>
where
    F: Fn(ChatRequest) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let prompt = build_prompt(&opts.task, dimensions, &opts.output);
    let mut results = vec![];
    for slot in slots {
        results.push(call_slot(slot, &prompt, max_tokens, chat).await);
    }
    results
}

async fn call_slot<F, Fut>(slot: &SlotConfig, prompt: &str, max_tokens: u32, chat: &F) -> SlotResult
where
    F: Fn(ChatRequest) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let start = std::time::Instant::now();
    let req = ChatRequest {
        model: slot.model.clone(),
        system: SYSTEM_PROMPT.to_string(),
        prompt: prompt.to_string(),
        max_tokens,
    };
        match chat(req).await {
        Ok(text) => match parse_model_json(&text) {
            Ok(parsed) => {
                let _ms = start.elapsed().as_millis() as u64;
                SlotResult { ok: true, model_id: slot.model.clone(), parsed: Some(parsed), error: None }
            }
            Err(e) => SlotResult {
                ok: false,
                model_id: slot.model.clone(),
                parsed: None,
                error: Some(format!("parse error: {e}")),
            },
        },
        Err(e) => SlotResult {
            ok: false,
            model_id: slot.model.clone(),
            parsed: None,
            error: Some(format!("{e}")),
        },
    }
}

fn build_prompt(task: &str, dimensions: &[String], output: &str) -> String {
    let dim_list = dimensions
        .iter()
        .enumerate()
        .map(|(i, d)| format!("{}. {}", i + 1, d))
        .collect::<Vec<_>>()
        .join("\n");
    [
        "You are a strict quality evaluator. Given a TASK and an OUTPUT, evaluate whether the output achieves the task goals.",
        "",
        "TASK:",
        task,
        "",
        "Score the OUTPUT 1-10 on each dimension:",
        &dim_list,
        "",
        "Scoring calibration:",
        "  9-10: Exceptional — would impress a domain expert",
        "  7-8:  Solid — accomplishes the goal, no major gaps",
        "  5-6:  Mediocre — obvious weaknesses",
        "  3-4:  Poor — missing important elements",
        "  1-2:  Failed",
        "",
        "Then list exactly 10 specific, actionable improvements — concrete changes with examples, prioritized by impact.",
        "",
        "Respond in JSON only (no markdown fences):",
        "{",
        "  \"scores\": {",
        "    \"dim_1_name\": { \"score\": N, \"feedback\": \"...\" },",
        "    ...",
        "  },",
        "  \"overall\": N,",
        "  \"improvements\": [\"1. ...\", \"2. ...\", ... \"10. ...\"]",
        "}",
        "",
        "OUTPUT:",
        output,
    ]
    .join("\n")
}

const SYSTEM_PROMPT: &str = "You are a strict quality evaluator. Reply with JSON only. Do not wrap in markdown fences. \
Each score must be an integer 1-10. Improvements must be concrete and actionable.";

fn clamp_cycles(n: Option<u32>) -> u32 {
    match n {
        Some(v) if v >= 1 && v <= 3 => v,
        Some(v) if v > 3 => 3,
        _ => 1,
    }
}

fn now_rfc3339() -> String {
    // chrono is available in zbrain-core (Cargo.toml: chrono = { workspace = true }).
    chrono::Utc::now().to_rfc3339()
}

// ===========================================================================
// Cost estimation (port of cross-modal-eval/runner.ts estimateCost)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct CostEstimate {
    pub per_cycle_usd: f64,
    pub per_run_max_usd: f64,
    pub per_call_tokens: u32,
    pub notes: Vec<String>,
}

/// Per-call cost = (input_tokens × input_price + output_tokens × output_price) / 1e6.
/// Without knowing prompt size, estimate input ~5k tokens. Prices drift; this
/// is intentionally rough (mirrors TS inline PRICING table + anthropic map).
pub fn estimate_cost(slots: &[SlotConfig], cycles: u32, max_tokens: u32) -> CostEstimate {
    const ESTIMATED_INPUT_TOKENS: u32 = 5000;
    // (in, out) USD per 1M tokens.
    let pricing: &[(&str, (f64, f64))] = &[
        ("openai:gpt-4o", (2.5, 10.0)),
        ("openai:gpt-4o-mini", (0.15, 0.6)),
        ("anthropic:claude-opus-4-7", (15.0, 75.0)),
        ("anthropic:claude-sonnet-4-6", (3.0, 15.0)),
        ("anthropic:claude-haiku-4-5-20251001", (0.8, 4.0)),
        ("google:gemini-1.5-pro", (1.25, 5.0)),
        ("google:gemini-2.0-flash", (0.1, 0.4)),
        ("together:meta-llama/Llama-3.3-70B-Instruct-Turbo", (0.88, 0.88)),
        ("deepseek:deepseek-chat", (0.14, 0.28)),
    ];
    let mut notes = vec![];
    let mut per_cycle = 0.0;
    for slot in slots {
        let p = pricing.iter().find(|(m, _)| *m == slot.model).map(|(_, v)| *v);
        let Some((in_p, out_p)) = p else {
            notes.push(format!("({}): no pricing on file; cost estimate may be low", slot.model));
            continue;
        };
        let cost = (ESTIMATED_INPUT_TOKENS as f64 * in_p + max_tokens as f64 * out_p) / 1_000_000.0;
        per_cycle += cost;
    }
    CostEstimate {
        per_cycle_usd: round2(per_cycle),
        per_run_max_usd: round2(per_cycle * cycles as f64),
        per_call_tokens: ESTIMATED_INPUT_TOKENS + max_tokens,
        notes,
    }
}

fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

// ===========================================================================
// Tests (port of cross-modal-eval unit oracles)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_chat_ok() -> impl Fn(ChatRequest) -> std::future::Ready<Result<String>> {
        |_req: ChatRequest| {
            std::future::ready(Ok(
                r#"{"scores":{"GOAL_ACHIEVEMENT":{"score":9,"feedback":"good"},"DEPTH":{"score":8}},"overall":8.5,"improvements":["1. add examples","2. tighten intro"]}"#.to_string(),
            ))
        }
    }

    #[test]
    fn parse_plain_json() {
        let r = parse_model_json(r#"{"scores":{"a":{"score":7}},"overall":7,"improvements":["1. x"]}"#).unwrap();
        assert_eq!(r.scores.get("a").unwrap().score, 7.0);
        assert_eq!(r.overall, Some(7.0));
    }

    #[test]
    fn parse_fenced_json() {
        let raw = "```json\n{\"scores\":{\"a\":{\"score\":6}}}\n```";
        let r = parse_model_json(raw).unwrap();
        assert_eq!(r.scores.get("a").unwrap().score, 6.0);
    }

    #[test]
    fn parse_trailing_comma_repaired() {
        let raw = "{\"scores\":{\"a\":{\"score\":5,},},\"improvements\":[\"1. x\"]}";
        let r = parse_model_json(raw).unwrap();
        assert_eq!(r.scores.get("a").unwrap().score, 5.0);
    }

    #[test]
    fn parse_single_quotes_repaired_across_shared_boundaries() {
        // `{'a':{'score':8}}` — adjacent single-quoted pairs share the `:`/`{`
        // boundary chars, so the lookaround-free port needs >1 repair pass.
        let raw = "{'scores':{'a':{'score':8}}}";
        let r = parse_model_json(raw).unwrap();
        assert_eq!(r.scores.get("a").unwrap().score, 8.0);
    }

    #[test]
    fn parse_uppercase_fence_stripped() {
        let raw = "```JSON\n{\"scores\":{\"a\":{\"score\":9}}}\n```";
        let r = parse_model_json(raw).unwrap();
        assert_eq!(r.scores.get("a").unwrap().score, 9.0);
    }

    #[test]
    fn parse_string_score_coerced() {
        let r = parse_model_json(r#"{"scores":{"a":{"score":"7.5"}}}"#).unwrap();
        assert_eq!(r.scores.get("a").unwrap().score, 7.5);
    }

    #[test]
    fn parse_trailing_prose_after_object() {
        // Strategy 2 must slice first `{` .. LAST `}`, not to end-of-string.
        let raw = "Here you go: {\"scores\":{\"a\":{\"score\":6}}} hope that helps!";
        let r = parse_model_json(raw).unwrap();
        assert_eq!(r.scores.get("a").unwrap().score, 6.0);
        assert!(!r.repaired);
    }

    #[test]
    fn parse_empty_throws() {
        assert!(parse_model_json("   ").is_err());
        assert!(parse_model_json("not json at all").is_err());
    }

    #[test]
    fn parse_nuclear_recovers_scores() {
        // Unescaped quotes inside a string value: JSON.parse fails and none of
        // the three repair rules can fix it, so strategy 4 must salvage the
        // score rather than fabricate a PASS.
        let raw = r#"{"dim_x": { "score": 4, "feedback": "he said "hi" here" }, "overall": 4}"#;
        let r = parse_model_json(raw).unwrap();
        assert!(r.repaired);
        assert_eq!(r.scores.get("dim_x").unwrap().score, 4.0);
        assert_eq!(r.overall, Some(4.0));
        assert_eq!(r.improvements, vec!["(could not parse improvements from malformed JSON)"]);
    }

    #[test]
    fn parse_valid_object_without_scores_key_errors() {
        // Strategy 2 yields parseable JSON that has no `scores` map — TS throws
        // "no usable scores" here instead of falling through to the nuclear
        // option. Guards the first-`{`..last-`}` slice against regressing.
        let err = parse_model_json("prose {\"dim_x\": { \"score\": 4 }} more prose")
            .expect_err("must not fabricate scores");
        assert!(err.to_string().contains("no usable scores"), "got: {err}");
    }

    #[test]
    fn aggregate_pass_when_all_dims_high() {
        let input = AggregateInput {
            slots: vec![
                SlotResult { ok: true, model_id: "openai:gpt-4o".into(), parsed: Some(ParsedModelResult { scores: bt("a", 9.0), overall: Some(9.0), improvements: vec![], repaired: false }), error: None },
                SlotResult { ok: true, model_id: "anthropic:claude".into(), parsed: Some(ParsedModelResult { scores: bt("a", 8.0), overall: Some(8.0), improvements: vec![], repaired: false }), error: None },
                SlotResult { ok: true, model_id: "google:gemini".into(), parsed: Some(ParsedModelResult { scores: bt("a", 8.0), overall: Some(8.0), improvements: vec![], repaired: false }), error: None },
            ],
        };
        let agg = aggregate(&input);
        assert_eq!(agg.verdict, Verdict::Pass);
        assert_eq!(agg.successes, 3);
        assert_eq!(agg.overall, Some(8.3));
    }

    #[test]
    fn aggregate_inconclusive_when_fewer_than_two() {
        let input = AggregateInput {
            slots: vec![
                SlotResult { ok: true, model_id: "openai:gpt-4o".into(), parsed: Some(ParsedModelResult { scores: bt("a", 9.0), overall: Some(9.0), improvements: vec![], repaired: false }), error: None },
                SlotResult { ok: false, model_id: "anthropic:claude".into(), parsed: None, error: Some("boom".into()) },
                SlotResult { ok: false, model_id: "google:gemini".into(), parsed: None, error: Some("boom".into()) },
            ],
        };
        let agg = aggregate(&input);
        assert_eq!(agg.verdict, Verdict::Inconclusive);
        assert_eq!(agg.successes, 1);
    }

    #[test]
    fn aggregate_fail_when_mean_below_seven() {
        let input = AggregateInput {
            slots: vec![
                SlotResult { ok: true, model_id: "openai".into(), parsed: Some(ParsedModelResult { scores: bt("a", 6.0), overall: Some(6.0), improvements: vec![], repaired: false }), error: None },
                SlotResult { ok: true, model_id: "anthropic".into(), parsed: Some(ParsedModelResult { scores: bt("a", 6.0), overall: Some(6.0), improvements: vec![], repaired: false }), error: None },
                SlotResult { ok: true, model_id: "google".into(), parsed: Some(ParsedModelResult { scores: bt("a", 6.0), overall: Some(6.0), improvements: vec![], repaired: false }), error: None },
            ],
        };
        let agg = aggregate(&input);
        assert_eq!(agg.verdict, Verdict::Fail);
        assert_eq!(agg.dimensions.get("a").unwrap().fail_reason, Some(FailReason::MeanBelow7));
    }

    #[test]
    fn aggregate_fail_when_one_model_floors() {
        let input = AggregateInput {
            slots: vec![
                SlotResult { ok: true, model_id: "openai".into(), parsed: Some(ParsedModelResult { scores: bt("a", 9.0), overall: Some(9.0), improvements: vec![], repaired: false }), error: None },
                SlotResult { ok: true, model_id: "anthropic".into(), parsed: Some(ParsedModelResult { scores: bt("a", 9.0), overall: Some(9.0), improvements: vec![], repaired: false }), error: None },
                SlotResult { ok: true, model_id: "google".into(), parsed: Some(ParsedModelResult { scores: bt("a", 3.0), overall: Some(3.0), improvements: vec![], repaired: false }), error: None },
            ],
        };
        let agg = aggregate(&input);
        assert_eq!(agg.verdict, Verdict::Fail);
        assert_eq!(agg.dimensions.get("a").unwrap().fail_reason, Some(FailReason::MinBelow5));
    }

    #[test]
    fn receipt_name_validation() {
        assert!(receipt_name("skillify", "content").is_ok());
        assert!(receipt_name("", "content").is_err());
        assert!(receipt_name("bad slug!", "content").is_err());
        let name = receipt_name("skillify", "hello").unwrap();
        assert!(name.starts_with("skillify-"));
        assert!(name.ends_with(".json"));
        assert_eq!(name.len(), "skillify-".len() + 8 + ".json".len());
    }

    #[test]
    fn sha8_stable_and_short() {
        let a = sha8("hello world");
        let b = sha8("hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn infer_slug_from_skill_path_takes_parent_dir() {
        assert_eq!(
            infer_slug_from_skill_path("skills/my-skill/SKILL.md").unwrap(),
            "my-skill"
        );
        assert!(infer_slug_from_skill_path("foo/bar.txt").is_err());
    }

    #[test]
    fn infer_slug_from_output_path_is_best_effort() {
        assert_eq!(
            infer_slug_from_output_path("skills/my-skill/SKILL.md").as_deref(),
            Some("my-skill")
        );
        assert_eq!(
            infer_slug_from_output_path(r"C:\repo\skills\my-skill\SKILL.md").as_deref(),
            Some("my-skill")
        );
        assert_eq!(infer_slug_from_output_path("notes/draft.md"), None);
    }

    #[test]
    fn is_receipt_file_matches() {
        assert!(is_receipt_file("skillify-abcdef01.json"));
        assert!(!is_receipt_file("notes.txt"));
        assert!(!is_receipt_file("skillify-xyz.json"));
    }

    #[test]
    fn estimate_cost_sums_per_slot() {
        let slots = vec![
            SlotConfig { id: "A".into(), model: "openai:gpt-4o".into() },
            SlotConfig { id: "B".into(), model: "anthropic:claude-opus-4-7".into() },
            SlotConfig { id: "C".into(), model: "google:gemini-1.5-pro".into() },
        ];
        let c = estimate_cost(&slots, 3, 4000);
        // 0.0525 (gpt-4o) + 0.375 (opus) + 0.02625 (gemini) = 0.45375/cycle.
        assert_eq!(c.per_cycle_usd, 0.45);
        // TS rounds the RAW per-cycle × cycles (0.45375*3 = 1.36125 -> 1.36),
        // NOT the already-rounded per-cycle (which would give 1.35).
        assert_eq!(c.per_run_max_usd, 1.36);
        assert_eq!(c.per_call_tokens, 9000);
        assert!(c.notes.is_empty());
    }

    #[tokio::test]
    async fn run_eval_writes_receipt_and_passes() {
        let dir = std::env::temp_dir().join(format!("cm_test_{}", sha8("run_eval_writes")));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = RunEvalOpts {
            task: "write a good doc".into(),
            output: "some output content".into(),
            slug: Some("demo".into()),
            dimensions: None,
            slots: None,
            cycles: Some(1),
            receipt_dir: dir.clone(),
            max_tokens: Some(4000),
            on_progress: None,
        };
        let res = run_eval(&opts, &fake_chat_ok()).await.unwrap();
        assert_eq!(res.final_aggregate.verdict, Verdict::Pass);
        assert_eq!(res.cycles.len(), 1);
        assert!(dir.join(receipt_name("demo", "some output content").unwrap()).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn bt(dim: &str, score: f64) -> BTreeMap<String, ParsedScore> {
        let mut m = BTreeMap::new();
        m.insert(dim.to_string(), ParsedScore { score, feedback: None });
        m
    }
}
