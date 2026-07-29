//! Part12 1-1-4 — port of `src/core/cycle/propose-takes.ts` →
//! `autopilot/phases/propose_takes.rs`.
//!
//! Scans brain pages, sends each page's prose to an LLM extractor, and writes
//! the extracted gradeable claims into the `take_proposals` queue. The queue
//! is a write-only proposal buffer; nothing here mutates the canonical
//! `takes` table (D17 auto-resolve-off-by-default).
//!
//! Porting notes vs the TS source:
//! - **LLM call is injected.** TS calls `gateway.chat` inside
//!   `defaultExtractor`; this port receives `&dyn ChatProvider` (mirrors
//!   `extract_atoms.rs`). Production wires the real provider via
//!   `CycleOpts.chat`; tests install `MockChatProvider`.
//! - **Idempotency via typed engine methods, not raw SQL.** TS uses
//!   `engine.executeRaw` against `take_proposals`. Per the zbrain-core design
//!   intent (engine.rs: the crate avoids `execute_raw` escapes), this port
//!   uses [`BrainEngine::take_proposal_exists`] + [`BrainEngine::add_take_proposal`]
//!   (implemented on all three engines). The composite unique index
//!   `(source_id, page_slug, content_hash, prompt_version)` keeps the INSERT
//!   conflict-safe.
//! - **Budget is a lightweight cumulative estimate** (same token-price model
//!   as `extract_atoms`), not the full `BaseCyclePhase.checkBudget` harness.
//!   It only gates whether we stop early; it does not block writes.

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::engine::{
    BrainEngine, EngineConfig, InMemoryEngine, PageFilters, PageInput, PageSort, TakeProposalInput,
};
use crate::error::{Error, Result as ZbResult};

/// Bump when the extractor prompt or the JSON output shape changes. Mirrors
/// TS `PROPOSE_TAKES_PROMPT_VERSION`. Old verdicts in `take_proposals` (composite
/// key includes prompt_version) stay valid as audit history; new runs
/// re-spend LLM tokens on every page.
pub const PROPOSE_TAKES_PROMPT_VERSION: &str = "v0.36.1.0-tuned-cat15";

/// Tuned extractor prompt (mirrors TS `EXTRACT_TAKES_PROMPT`). Validated against
/// the synthetic corpus at tests/unit/fixtures/calibration/ (cat15 eval).
const EXTRACT_TAKES_PROMPT: &str = r#"Extract gradeable claims from the prose below.

A "gradeable claim" is a prediction, recommendation, or interpretive judgment
that could turn out wrong over time. Examples:
- "X company will hit ARR milestone by Q3" (prediction)
- "Y founder is going to struggle with execution" (judgment)
- "Z market will compress in 18 months" (prediction)
- "I bet alice wins the round" (bet)

NOT gradeable (do NOT extract these):
- Pure facts ("X was founded in 2020")
- Direct quotes from others without endorsement
- Restatements of an earlier claim in the same page

For each gradeable claim, output a JSON object with:
- claim_text   (string, <=200 chars, paraphrase or near-verbatim from prose)
- kind         ('fact' | 'take' | 'bet' | 'hunch')
- holder       ('world' | 'people/<slug>' | 'companies/<slug>' | 'brain' — default 'brain' when author asserts the claim)
- weight       (number 0..1 inferred from hedging language: 'I bet'/'strong conviction'=0.7-0.85,
                'I think'/'moderate conviction'=0.5-0.7, 'maybe'/'I'd guess'=0.3-0.5)
- domain       (short tag — e.g. 'tactics', 'macro', 'hiring', 'geography', 'pricing')

Output ONLY a JSON array of these objects. No prose. No commentary. If no
gradeable claims, return [].

EXISTING FENCE ROWS (already captured — do NOT propose duplicates):
{EXISTING_TAKES_JSON}

PAGE PROSE:
{PAGE_BODY}
"#;

/// Options for [`run_propose_takes`]. Mirrors TS `ProposeTakesOpts`.
#[derive(Debug, Clone, Default)]
pub struct ProposeTakesOpts {
    pub dry_run: bool,
    pub source_id: Option<String>,
    /// Limit pages processed in this cycle (for triage / quick smoke).
    pub page_limit: Option<usize>,
    /// Override prompt_version (tests).
    pub prompt_version: Option<String>,
    /// Override model id (tests + config).
    pub model: Option<String>,
    /// Skip pages that already have a complete takes fence. TS code default: false.
    pub skip_pages_with_fence: bool,
}

/// Result of a single `propose_takes` run. Mirrors TS `ProposeTakesResult`.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposeTakesResult {
    pub pages_scanned: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub proposals_inserted: u64,
    pub budget_exhausted: bool,
    pub warnings: Vec<String>,
}

/// A single proposed take as the extractor produces it.
#[derive(Debug, Clone)]
struct ProposedTake {
    claim_text: String,
    kind: String,
    holder: String,
    weight: f64,
    domain: Option<String>,
}

/// A row from an existing takes fence, used as dedup context for the extractor.
#[derive(Debug, Clone, serde::Serialize)]
struct ExistingTakeDedup {
    claim: String,
    kind: String,
    holder: String,
    weight: f64,
}

/// Default per-cycle LLM budget in USD (mirrors TS `budgetUsdDefault = 5.0`).
const DEFAULT_BUDGET_USD: f64 = 5.0;

/// Compute the content_hash idempotency key (SHA-256 of the page body).
fn content_hash(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Detect whether a page already has a `<!-- zbrain:takes:begin -->` …
/// `zbrain:takes:end -->` fence. Used for the optional skip-with-fence fast
/// pass. Cheap substring heuristic (TS uses a tighter regex).
fn has_complete_fence(body: &str) -> bool {
    match (body.find("zbrain:takes:begin"), body.find("zbrain:takes:end")) {
        (Some(b), Some(e)) => b < e,
        _ => false,
    }
}

/// Parse an existing takes fence into rows so the extractor can dedupe.
/// Returns [] when no fence is present. Best-effort.
fn extract_existing_takes_for_dedup(body: &str) -> Vec<ExistingTakeDedup> {
    let begin = match body.find("zbrain:takes:begin") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let end = match body[begin..].find("zbrain:takes:end") {
        Some(j) => begin + j,
        None => return Vec::new(),
    };
    let inner = &body[begin..end];
    let mut rows = Vec::new();
    for line in inner.split('\n') {
        let cells: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
        // Pipe table: ['', '#', claim, kind, holder, weight, ...]
        if cells.len() < 5 {
            continue;
        }
        if cells.get(1).map(|c| *c == "#").unwrap_or(false) {
            continue;
        }
        let claim = cells.get(2).unwrap_or(&"").to_string();
        if claim.is_empty() || claim.starts_with("~~") {
            continue;
        }
        let kind = cells.get(3).unwrap_or(&"take").to_string();
        let holder = cells.get(4).unwrap_or(&"brain").to_string();
        let weight = cells.get(5).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.5);
        rows.push(ExistingTakeDedup {
            claim,
            kind,
            holder,
            weight,
        });
    }
    rows
}

/// Build the extractor prompt by substituting the existing-fence JSON and the
/// page prose into [`EXTRACT_TAKES_PROMPT`].
fn build_prompt(existing_takes: &[ExistingTakeDedup], body: &str) -> String {
    let existing_json = serde_json::to_string(existing_takes).unwrap_or_else(|_| "[]".to_string());
    EXTRACT_TAKES_PROMPT
        .replace("{EXISTING_TAKES_JSON}", &existing_json)
        .replace("{PAGE_BODY}", body)
}

/// Parse extractor output into [`ProposedTake`]s. Tolerant of common LLM
/// mistakes (markdown fence wrapping, leading/trailing prose, single object
/// instead of array). Returns [] on any unrecoverable parse error.
fn parse_extractor_output(raw: &str) -> Vec<ProposedTake> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let mut text = raw.trim().to_string();
    // Strip a markdown code fence wrapper if present.
    if let Some(stripped) = text.strip_prefix("```") {
        let inner = stripped.trim_start_matches("json").trim_start();
        if let Some(end) = inner.find("```") {
            text = inner[..end].trim().to_string();
        }
    }
    let first_arr = text.find('[').unwrap_or(usize::MAX);
    let first_obj = text.find('{').unwrap_or(usize::MAX);
    if first_arr == usize::MAX && first_obj == usize::MAX {
        return Vec::new();
    }
    let start = if first_arr != usize::MAX && (first_obj == usize::MAX || first_arr < first_obj) {
        first_arr
    } else {
        first_obj
    };
    let parsed: serde_json::Value = match serde_json::from_str(&text[start..]) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match parsed {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    let mut out = Vec::new();
    for item in &arr {
        let obj = match item {
            serde_json::Value::Object(o) => o,
            _ => continue,
        };
        let claim_text = match obj.get("claim_text").and_then(|v| v.as_str()) {
            Some(s) => s.trim().to_string(),
            None => continue,
        };
        if claim_text.is_empty() || claim_text.len() > 500 {
            continue;
        }
        let kind = match obj.get("kind").and_then(|v| v.as_str()) {
            Some(k) if ["fact", "take", "bet", "hunch"].contains(&k) => k.to_string(),
            _ => "take".to_string(),
        };
        let holder = match obj.get("holder").and_then(|v| v.as_str()) {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => "brain".to_string(),
        };
        let weight = match obj.get("weight").and_then(|v| v.as_f64()) {
            Some(w) => w,
            None => 0.5,
        }
        .clamp(0.0, 1.0);
        let domain = obj.get("domain").and_then(|v| v.as_str()).map(str::to_string);
        out.push(ProposedTake {
            claim_text,
            kind,
            holder,
            weight,
            domain,
        });
    }
    out
}

/// Run the propose-takes phase.
///
/// - Lists source-scoped pages (`list_pages`, `updated_desc`).
/// - For each page with non-empty prose: computes `content_hash`, checks the
///   idempotency cache via [`BrainEngine::take_proposal_exists`]; on a miss,
///   calls the LLM extractor and writes each proposal via
///   [`BrainEngine::add_take_proposal`] (conflict-safe).
/// - A single page error logs a warning and does not abort the phase.
pub async fn run_propose_takes(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    opts: &ProposeTakesOpts,
) -> ZbResult<ProposeTakesResult> {
    let source_id = opts
        .source_id
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let prompt_version = opts
        .prompt_version
        .clone()
        .unwrap_or_else(|| PROPOSE_TAKES_PROMPT_VERSION.to_string());
    let page_limit = opts.page_limit.unwrap_or(100);
    let skip_pages_with_fence = opts.skip_pages_with_fence;
    let proposal_run_id = format!("propose-{}", uuid::Uuid::new_v4().simple());

    let mut result = ProposeTakesResult::default();
    let mut estimated_spend = 0.0f64;

    let pages = engine
        .list_pages(&PageFilters {
            limit: Some(page_limit),
            sort: Some(PageSort::UpdatedDesc),
            source_id: Some(source_id.clone()),
            ..Default::default()
        })
        .await?;

    for page in &pages {
        result.pages_scanned += 1;
        let body = &page.compiled_truth;
        if body.trim().is_empty() {
            continue;
        }
        if skip_pages_with_fence && has_complete_fence(body) {
            continue;
        }
        let ch = content_hash(body);
        let existing_takes = extract_existing_takes_for_dedup(body);

        // Idempotency cache check.
        match engine
            .take_proposal_exists(&source_id, &page.slug, &ch, &prompt_version)
            .await
        {
            Ok(true) => {
                result.cache_hits += 1;
                continue;
            }
            Ok(false) => {
                result.cache_misses += 1;
            }
            Err(e) => {
                // One bad cache read must not abort the phase; treat as miss.
                result.warnings.push(format!(
                    "take_proposal_exists failed on {}: {}",
                    page.slug, e
                ));
                result.cache_misses += 1;
            }
        }

        // Budget pre-check (lightweight cumulative estimate, mirrors TS).
        if estimated_spend >= DEFAULT_BUDGET_USD {
            result.budget_exhausted = true;
            result.warnings.push(format!(
                "budget exhausted at page {}/{} (cumulative ${:.4} / cap ${:.2})",
                result.pages_scanned,
                pages.len(),
                estimated_spend,
                DEFAULT_BUDGET_USD
            ));
            break;
        }

        let prompt = build_prompt(&existing_takes, body);
        let chat_result = match chat
            .chat(ChatOpts {
                model: opts.model.clone(),
                system: None,
                messages: vec![ChatMessage::text(ChatRole::User, prompt)],
                tools: vec![],
                max_tokens: Some(2048),
                cache_system: false,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                result
                    .warnings
                    .push(format!("extractor failed on {}: {}", page.slug, e));
                continue;
            }
        };

        let usage = &chat_result.usage;
        estimated_spend +=
            (usage.input_tokens as f64 * 0.8 + usage.output_tokens as f64 * 4.0) / 1_000_000.0;

        let proposals = parse_extractor_output(&chat_result.text);
        let dedup_json =
            serde_json::to_string(&existing_takes).unwrap_or_else(|_| "[]".to_string());
        let model_id = opts
            .model
            .clone()
            .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
        for p in &proposals {
            let input = TakeProposalInput {
                source_id: source_id.clone(),
                page_slug: page.slug.clone(),
                content_hash: ch.clone(),
                prompt_version: prompt_version.clone(),
                proposal_run_id: proposal_run_id.clone(),
                claim_text: p.claim_text.clone(),
                kind: p.kind.clone(),
                holder: p.holder.clone(),
                weight: p.weight,
                domain: p.domain.clone(),
                dedup_against_fence_rows: Some(dedup_json.clone()),
                model_id: model_id.clone(),
            };
            if !opts.dry_run {
                if let Err(e) = engine.add_take_proposal(&input).await {
                    result.warnings.push(format!(
                        "add_take_proposal failed on {}: {}",
                        page.slug, e
                    ));
                    continue;
                }
            }
            result.proposals_inserted += 1;
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatUsage, MockChatProvider, StopReason};

    async fn setup() -> InMemoryEngine {
        let e = InMemoryEngine::new();
        e.connect(&EngineConfig::default()).await.unwrap();
        e
    }

    async fn put_page(e: &InMemoryEngine, slug: &str, body: &str) {
        e.put_page(
            slug,
            Some("default"),
            &PageInput {
                page_type: "note".to_string(),
                title: slug.to_string(),
                compiled_truth: body.to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_propose_takes_empty_brain_inmemory() {
        let engine = setup().await;
        let chat = MockChatProvider::new("[]");
        let result = run_propose_takes(
            &engine,
            &chat,
            &ProposeTakesOpts {
                source_id: Some("default".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.pages_scanned, 0);
        assert_eq!(result.proposals_inserted, 0);
        assert!(!result.budget_exhausted);
    }

    #[tokio::test]
    async fn run_propose_takes_inserts_and_idempotent_inmemory() {
        let engine = setup().await;
        put_page(
            &engine,
            "page-a",
            "Alice predicts the market will compress in 18 months. I bet she is right.",
        )
        .await;
        let chat = MockChatProvider::new(
            r#"[{"claim_text":"market will compress in 18 months","kind":"prediction","holder":"brain","weight":0.7,"domain":"macro"}]"#,
        );
        let result = run_propose_takes(
            &engine,
            &chat,
            &ProposeTakesOpts {
                source_id: Some("default".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.pages_scanned, 1);
        assert_eq!(result.cache_misses, 1);
        assert_eq!(result.cache_hits, 0);
        assert_eq!(result.proposals_inserted, 1);

        // Idempotency: re-run hits the cache, inserts nothing new.
        let chat2 = MockChatProvider::new("[]");
        let result2 = run_propose_takes(
            &engine,
            &chat2,
            &ProposeTakesOpts {
                source_id: Some("default".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result2.cache_hits, 1);
        assert_eq!(result2.cache_misses, 0);
        assert_eq!(result2.proposals_inserted, 0);
    }

    #[test]
    fn parse_extractor_output_variants() {
        assert_eq!(parse_extractor_output("").len(), 0);
        assert_eq!(parse_extractor_output("no json").len(), 0);
        let a = parse_extractor_output(
            r#"[{"claim_text":"x","kind":"prediction","holder":"brain","weight":0.7}]"#,
        );
        assert_eq!(a.len(), 1);
        // TS parseExtractorOutput normalizes kinds outside ['fact','take','bet','hunch']
        // to 'take' (the prompt asks for prediction/judgment but the parser enum is
        // fact/take/bet/hunch) — mirror that quirk faithfully.
        assert_eq!(a[0].kind, "take");
        // invalid kind defaults to "take"
        let b = parse_extractor_output(r#"[{"claim_text":"y","kind":"bogus","weight":0.5}]"#);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, "take");
        // fenced wrapper
        let c = parse_extractor_output("```json\n[{\"claim_text\":\"z\",\"weight\":0.5}]\n```");
        assert_eq!(c.len(), 1);
        // over-length claim_text filtered
        let d = parse_extractor_output(r#"[{"claim_text":"","weight":0.5}]"#);
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn content_hash_stable_and_distinct() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("world"));
    }
}
