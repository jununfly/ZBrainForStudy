//! Synthesize phase (v0.32) — fan-out orchestration.
//!
//! Port of TS `src/core/cycle/synthesize.ts` `runPhaseSynthesize` (the
//! fan-out core). This module covers the orchestration loop:
//!
//! 1. discover transcripts ([`transcript_discovery`], 1-3-4-2)
//! 2. judge significance via the wired `ChatProvider` and cache the verdict in
//!    `dream_verdicts` ([`BrainEngine::get_dream_verdict`] / `put_dream_verdict`,
//!    1-3-4-1)
//! 3. split each transcript into budget-sized chunks
//!    ([`context_budget`], 1-3-4-3)
//! 4. fan out one `"subagent"` minion job per worth-processing transcript (or
//!    per chunk for chunked transcripts) via [`MinionQueue`] + poll to
//!    completion via [`wait_for_completion`] (mirrors the `patterns` phase)
//! 5. best-effort slug collection (the `subagent_tool_executions` table is not
//!    mirrored in Rust; 1-3-4-6 decision)
//!
//! ## Rust deviations (documented so the port stays honest)
//!
//! - **Config**: TS reads `dream.synthesize.*` (corpus_dir, model,
//!   verdict_model, cooldown, …) from the engine config store via
//!   `engine.getConfig`. Rust still has **no engine config store** (1-3-4-6
//!   will wire it), so [`load_synth_config`] returns the TS *defaults*.
//!   `corpus_dir` defaults to `None` → the phase returns `Skipped(
//!   "not_configured")` unless an override is supplied via
//!   [`SynthesizePhaseOpts`] (ad-hoc path, mirrors TS `inputFile`). This is the
//!   documented seam to wire real config lookup.
//! - **Disk dual-write**: TS `reverseWriteRefs` + `writeSummaryPage` render the
//!   synthesized pages back to `brainDir/*.md`. That is **1-3-4-5** — omitted
//!   here. The subagent writes pages to the engine DB directly (canonical), so
//!   `pages_written` is best-effort harvested from `subagent_tool_executions`.
//! - **Cooldown**: TS `checkCooldown` gates re-runs. That is **1-3-4-6** —
//!   omitted here.
//! - **Prior contradictions block**: TS surfaces `loadPriorContradictionsBlock`
//!   into the subagent prompt. Best-effort informational only; omitted here.
//! - **Worker execution**: the subagent itself runs in the minion worker (wired
//!   with a chat provider), so the cycle phase does **not** need chat for the
//!   synthesis step — only for the significance *judge* (which runs in-phase).
//!   Matches the TS design where the phase enqueues rather than calling the
//!   LLM directly for synthesis.

use serde_json::{json, Value};

use crate::ai::chat::{ChatError, ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::autopilot::phases::context_budget::{
    compute_chunk_char_budget, split_transcript_by_budget, DEFAULT_MAX_CHUNKS,
};
use crate::autopilot::phases::transcript_discovery::{
    discover_transcripts, DiscoveredTranscript, DiscoverOpts,
};
use crate::engine::{BrainEngine, DreamVerdict, DreamVerdictInput};
use crate::minions::queue::MinionQueue;
use crate::minions::types::{ChildFailPolicy, MinionJobInput};
use crate::minions::wait_for_completion::{wait_for_completion, WaitError, WaitOpts};
use crate::Result;

/// Options for [`run_phase_synthesize`]. Mirrors TS `SynthesizePhaseOpts`.
pub struct SynthesizePhaseOpts {
    /// Brain directory for disk reverse-write (unused here — 1-3-4-5). Kept for
    /// parity with the cycle arm.
    pub brain_dir: Option<String>,
    /// If true, judge significance + enqueue nothing (mirrors TS `--dry-run`).
    pub dry_run: bool,
    /// Override the per-child wait timeout (ms). Default 35min. Test seam.
    pub wait_timeout_ms: Option<u64>,
    /// Ad-hoc corpus dir override (mirrors TS `inputFile` ad-hoc path; bypasses
    /// config reads). `None` → use config `corpus_dir` (unset until 1-3-4-6,
    /// so the phase skips `not_configured`).
    pub corpus_dir: Option<String>,
    pub date: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    /// Disable the self-consumption guard.
    pub bypass_dream_guard: bool,
    /// Optional config overrides (testing / CLI). 1-3-4-6 wires these from the
    /// engine config store.
    pub model: Option<String>,
    pub verdict_model: Option<String>,
    pub min_chars: Option<usize>,
    pub max_chunks_per_transcript: Option<usize>,
    pub max_prompt_tokens: Option<u32>,
}

impl Default for SynthesizePhaseOpts {
    fn default() -> Self {
        Self {
            brain_dir: None,
            dry_run: false,
            wait_timeout_ms: None,
            corpus_dir: None,
            date: None,
            from: None,
            to: None,
            bypass_dream_guard: false,
            model: None,
            verdict_model: None,
            min_chars: None,
            max_chunks_per_transcript: None,
            max_prompt_tokens: None,
        }
    }
}

/// Result of a `synthesize` run. Mirrors the TS `PhaseResult` summary/details.
#[derive(Debug, Clone, Default)]
pub struct SynthesizePhaseResult {
    /// `"ok"`, `"warn"` or `"skipped"`.
    pub status: String,
    pub summary: String,
    pub reason: Option<String>,
    pub transcripts_discovered: u64,
    pub transcripts_processed: u64,
    pub pages_written: u64,
    pub children_submitted: u64,
    pub dry_run: bool,
    pub verdicts: Vec<VerdictRecord>,
    pub child_outcomes: Vec<ChildOutcome>,
    pub skips: Vec<SkipReport>,
}

impl SynthesizePhaseResult {
    fn skipped(reason: &str, summary: &str) -> Self {
        Self {
            status: "skipped".into(),
            summary: summary.into(),
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
    fn ok(summary: &str) -> Self {
        Self {
            status: "ok".into(),
            summary: summary.into(),
            ..Default::default()
        }
    }
}

/// One significance verdict, cached or freshly judged.
#[derive(Debug, Clone)]
pub struct VerdictRecord {
    pub file_path: String,
    pub worth: bool,
    pub reasons: Vec<String>,
    pub cached: bool,
}

/// Terminal outcome of one child subagent job.
#[derive(Debug, Clone)]
pub struct ChildOutcome {
    pub job_id: i64,
    pub status: String,
}

/// A transcript skipped before fan-out (oversize after split, etc.).
#[derive(Debug, Clone)]
pub struct SkipReport {
    pub file_path: String,
    pub reason: String,
}

/// In-process synth config. Defaults mirror the TS `SynthConfig`.
struct SynthConfig {
    enabled: bool,
    corpus_dir: Option<String>,
    meeting_transcripts_dir: Option<String>,
    min_chars: usize,
    exclude_patterns: Vec<String>,
    model: String,
    verdict_model: String,
    cooldown_hours: u64,
    max_prompt_tokens: Option<u32>,
    max_chunks_per_transcript: usize,
}

/// Load synth config.
///
/// Rust has no engine config store yet (see module docs) — returns TS
/// defaults, with `corpus_dir` taken from the ad-hoc override when present.
/// This is the documented seam to wire real `getConfig` in 1-3-4-6.
fn load_synth_config(opts: &SynthesizePhaseOpts) -> SynthConfig {
    SynthConfig {
        enabled: true,
        corpus_dir: opts.corpus_dir.clone(),
        meeting_transcripts_dir: None,
        min_chars: opts.min_chars.unwrap_or(2000),
        exclude_patterns: Vec::new(),
        model: opts
            .model
            .clone()
            .unwrap_or_else(|| "sonnet".to_string()),
        verdict_model: opts
            .verdict_model
            .clone()
            .unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string()),
        cooldown_hours: 24,
        max_prompt_tokens: opts.max_prompt_tokens,
        max_chunks_per_transcript: opts
            .max_chunks_per_transcript
            .unwrap_or(DEFAULT_MAX_CHUNKS),
    }
}

/// Allowed slug prefixes for the synthesis subagent.
///
/// TS loads these from `skills/_brain-filing-rules.json`
/// (`dream_synthesize_paths.globs`); missing → `failed("NO_ALLOWLIST")`. Rust
/// has no filing-rules loader yet, so we default to a permissive `wiki/`
/// allow-list. The worker enforces the real allow-list, so this is a safe
/// placeholder; tighten in 1-3-4-6.
fn allowed_slug_prefixes() -> Vec<String> {
    vec!["wiki/".to_string()]
}

/// Normalize a model id to `provider:model` shape for the subagent queue.
/// Mirrors TS `resolveModel` + prefix logic.
fn normalize_subagent_model(model: &str) -> String {
    if model.contains(':') {
        return model.to_string();
    }
    if model.to_ascii_lowercase().starts_with("claude-") {
        return format!("anthropic:{model}");
    }
    model.to_string()
}

/// Sanitize a basename into a slug segment. Minimal port of TS
/// `sanitizeForSlug` (lowercase, alnum + hyphens, collapse repeats).
fn sanitize_for_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c.is_whitespace() || c == '-' || c == '_' || c == '/' {
            out.push('-');
        }
    }
    // collapse repeated hyphens
    let collapsed: String = out
        .split('-')
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    collapsed
}

/// Build the prompt for one synthesis subagent chunk. Port of TS
/// `buildSynthesisPrompt`.
fn build_synthesis_prompt(
    t: &DiscoveredTranscript,
    chunk_text: &str,
    chunk_idx: usize,
    chunk_total: usize,
) -> String {
    let date_hint = t
        .inferred_date
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    let base_slug_segment = sanitize_for_slug(&t.basename);
    let is_chunked = chunk_total > 1;
    let hash_suffix = if is_chunked {
        format!("{}-c{chunk_idx}", &t.content_hash[..t.content_hash.len().min(6)])
    } else {
        t.content_hash[..t.content_hash.len().min(6)].to_string()
    };
    let chunk_banner = if is_chunked {
        format!(
            "\n- This is CHUNK {} of {} from the same transcript. Different chunks process \
             different sections; do not assume continuity with other chunks.",
            chunk_idx + 1,
            chunk_total
        )
    } else {
        String::new()
    };
    let transcript_header = if is_chunked {
        format!("{} (chunk {}/{})", t.file_path.display(), chunk_idx + 1, chunk_total)
    } else {
        t.file_path.display().to_string()
    };
    format!(
        "You are synthesizing a conversation transcript into the user's personal knowledge brain.\n\n\
         CONTEXT\n\
         - Today's date: {date_hint}\n\
         - Transcript hash suffix (USE THIS in slugs): {hash_suffix}\n\
         - Source file basename: {base_slug_segment}{chunk_banner}\n\n\
         OUTPUT POLICY (ALL of these are required)\n\
         1. Quote the user verbatim. Do not paraphrase memorable phrasings.\n\
         2. Cross-reference compulsively: every new page MUST contain at least one wikilink \
            (e.g. `[ref](people/jane-doe)` or `[[people/jane-doe]]`) to existing brain content. \
            Use the search tool to find existing pages first.\n\
         3. Do NOT write to any path outside the allow-list shown in the put_page schema.\n\
         4. Slug discipline: lowercase alphanumeric and hyphens only, slash-separated segments. \
            NO underscores, NO file extensions.\n\n\
         TASKS\n\
         A. Reflections (self-knowledge, pattern recognition, emotional processing):\n\
            slug: `wiki/personal/reflections/{date_hint}-<topic-slug>-{hash_suffix}`\n\n\
         B. Originals (new ideas, frames, theses, mental models):\n\
            slug: `wiki/originals/ideas/{date_hint}-<idea-slug>-{hash_suffix}`\n\n\
         C. People mentions: search first; if a page exists, do not put_page over it (the \
            orchestrator handles people enrichment via timeline entries — your job is the \
            reflection/original synthesis, NOT modifying existing person pages).\n\n\
         D. If nothing in this transcript meets the bar (significance filter already passed but \
            the content is still routine), return without writing anything.\n\n\
         TRANSCRIPT ({transcript_header})\n\
         ---\n\
         {chunk_text}\n\
         ---\n\n\
         When done, briefly list the slugs you wrote in your final message so the orchestrator \
         can audit.",
        date_hint = date_hint,
        hash_suffix = hash_suffix,
        base_slug_segment = base_slug_segment,
        chunk_banner = chunk_banner,
        transcript_header = transcript_header,
        chunk_text = chunk_text,
    )
}

/// Significance verdict returned by [`judge_significance`].
struct VerdictResult {
    worth_processing: bool,
    reasons: Vec<String>,
}

/// Judge whether a transcript is worth synthesizing, via the wired chat
/// provider. Port of TS `judgeSignificance` + `makeJudgeClient`.
///
/// The transcript is truncated to 8K chars (head + tail) for cost control. The
/// model is asked to reply as JSON `{worth_processing, reasons}`; unparseable
/// responses default to `worth_processing = false` (cheap fallback), mirroring
/// TS.
async fn judge_significance(
    chat: &dyn ChatProvider,
    t: &DiscoveredTranscript,
    verdict_model: &str,
) -> std::result::Result<VerdictResult, ChatError> {
    let chars: Vec<char> = t.content.chars().collect();
    let trimmed = if chars.len() > 8000 {
        let head: String = chars.iter().take(4000).collect();
        let tail: String = chars
            .iter()
            .skip(chars.len().saturating_sub(4000))
            .collect();
        format!("{head}\n[...truncated...]\n{tail}")
    } else {
        t.content.clone()
    };

    let sys = "You judge whether a conversation transcript is worth synthesizing into a personal knowledge brain.\n\n\
        WORTH PROCESSING (return worth_processing=true):\n\
        - The user articulates a new idea, frame, mental model, or thesis\n\
        - The user reflects on themselves, names patterns, processes emotion\n\
        - The user discusses specific people, companies, or decisions in depth\n\
        - The user makes a strategic call worth remembering\n\n\
        NOT WORTH PROCESSING (return worth_processing=false):\n\
        - Routine ops (\"check my email\", \"schedule X\")\n\
        - Pure code debugging without user reflection\n\
        - Short message exchanges with no original thought\n\
        - Repetitive content the brain already has\n\n\
        Respond as JSON: {\"worth_processing\": <bool>, \"reasons\": [\"<short>\", \"<short>\"]}.\n\
        Two reasons max, one phrase each.";

    let result = chat
        .chat(ChatOpts {
            model: Some(verdict_model.to_string()),
            system: Some(sys.to_string()),
            messages: vec![ChatMessage::text(
                ChatRole::User,
                format!("Transcript {}:\n\n{}", t.basename, trimmed),
            )],
            max_tokens: Some(200),
            cache_system: false,
            ..Default::default()
        })
        .await?;

    // Extract the first balanced JSON object (greedy, from first '{' to last
    // '}') — mirrors TS `\{[\s\S]*\}`. Verdict JSON is flat, so this is safe.
    let text = result.text.trim();
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            let obj = &text[start..=end];
            if let Ok(parsed) = serde_json::from_str::<Value>(obj) {
                let worth = parsed
                    .get("worth_processing")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let reasons = parsed
                    .get("reasons")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .take(4)
                            .collect()
                    })
                    .unwrap_or_default();
                return Ok(VerdictResult {
                    worth_processing: worth,
                    reasons,
                });
            }
        }
    }
    Ok(VerdictResult {
        worth_processing: false,
        reasons: vec!["judge response unparseable".into()],
    })
}

/// Run the synthesize phase. Mirrors TS `runPhaseSynthesize` (fan-out core).
pub async fn run_phase_synthesize(
    engine: &dyn BrainEngine,
    chat: Option<&dyn ChatProvider>,
    opts: &SynthesizePhaseOpts,
) -> Result<SynthesizePhaseResult> {
    let config = load_synth_config(opts);

    if config.corpus_dir.is_none() {
        return Ok(SynthesizePhaseResult::skipped(
            "not_configured",
            "dream.synthesize.session_corpus_dir is unset",
        ));
    }
    if !config.enabled {
        return Ok(SynthesizePhaseResult::skipped(
            "not_configured",
            "dream.synthesize.enabled is explicitly false",
        ));
    }

    // Discover transcripts.
    let corpus_dir = config.corpus_dir.clone().unwrap();
    let transcripts = discover_transcripts(&DiscoverOpts {
        corpus_dir: std::path::PathBuf::from(&corpus_dir),
        meeting_transcripts_dir: config
            .meeting_transcripts_dir
            .as_ref()
            .map(std::path::PathBuf::from),
        min_chars: Some(config.min_chars),
        exclude_patterns: config.exclude_patterns.clone(),
        date: opts.date.clone(),
        from: opts.from.clone(),
        to: opts.to.clone(),
        bypass_guard: opts.bypass_dream_guard,
    });

    if transcripts.is_empty() {
        let mut r = SynthesizePhaseResult::ok("no transcripts to process");
        r.transcripts_discovered = 0;
        return Ok(r);
    }

    // Significance verdicts (cached in dream_verdicts; judge on miss).
    let mut worth_processing: Vec<DiscoveredTranscript> = Vec::new();
    let mut verdicts: Vec<VerdictRecord> = Vec::new();
    for t in &transcripts {
        let fp = t.file_path.to_string_lossy().to_string();
        if let Some(cached) = engine
            .get_dream_verdict(&fp, &t.content_hash)
            .await?
        {
            let DreamVerdict {
                worth_processing: worth,
                reasons,
                ..
            } = cached;
            verdicts.push(VerdictRecord {
                file_path: fp.clone(),
                worth,
                reasons: reasons.clone(),
                cached: true,
            });
            if worth {
                worth_processing.push(t.clone());
            }
            continue;
        }

        // No cached verdict → judge (needs a chat provider).
        let verdict = match chat {
            None => {
                verdicts.push(VerdictRecord {
                    file_path: fp.clone(),
                    worth: false,
                    reasons: vec![format!(
                        "no configured provider for verdict model: {}",
                        config.verdict_model
                    )],
                    cached: false,
                });
                continue;
            }
            Some(c) => match judge_significance(c, t, &config.verdict_model).await {
                Ok(v) => v,
                Err(e) => {
                    verdicts.push(VerdictRecord {
                        file_path: fp.clone(),
                        worth: false,
                        reasons: vec![format!("gateway error: {e}")],
                        cached: false,
                    });
                    continue;
                }
            },
        };

        engine
            .put_dream_verdict(
                &fp,
                &t.content_hash,
                &DreamVerdictInput {
                    worth_processing: verdict.worth_processing,
                    reasons: verdict.reasons.clone(),
                },
            )
            .await?;
        verdicts.push(VerdictRecord {
            file_path: fp.clone(),
            worth: verdict.worth_processing,
            reasons: verdict.reasons.clone(),
            cached: false,
        });
        if verdict.worth_processing {
            worth_processing.push(t.clone());
        }
    }

    // Dry-run stops here: significance filter ran (verdicts cached), but no
    // synthesis. Mirrors TS (Codex finding #8: --dry-run skips Sonnet only).
    if opts.dry_run {
        let mut r = SynthesizePhaseResult::ok(&format!(
            "dry-run: {} of {} transcripts would synthesize",
            worth_processing.len(),
            transcripts.len()
        ));
        r.dry_run = true;
        r.transcripts_discovered = transcripts.len() as u64;
        r.verdicts = verdicts;
        return Ok(r);
    }

    if worth_processing.is_empty() {
        let mut r = SynthesizePhaseResult::ok("all transcripts skipped by significance filter");
        r.transcripts_discovered = transcripts.len() as u64;
        r.verdicts = verdicts;
        return Ok(r);
    }

    // Fan-out: submit one subagent per worth-processing transcript (or one per
    // chunk for chunked transcripts).
    let allowed = allowed_slug_prefixes();
    let queue = MinionQueue::new(engine);
    let max_chars = compute_chunk_char_budget(&config.model, config.max_prompt_tokens);
    let max_chars = if max_chars == 0 {
        // Should not happen (model lookup floors to > 0); guard against panic
        // in split_transcript_by_budget.
        return Ok(SynthesizePhaseResult::skipped(
            "budget_zero",
            "compute_chunk_char_budget returned 0",
        ));
    } else {
        max_chars as usize
    };

    let mut child_ids: Vec<i64> = Vec::new();
    let mut skips: Vec<SkipReport> = Vec::new();

    for t in &worth_processing {
        let fp = t.file_path.to_string_lossy().to_string();
        let hash16 = t.content_hash[..t.content_hash.len().min(16)].to_string();

        let chunks = split_transcript_by_budget(&t.content, &t.content_hash, max_chars);

        // D5 cap hit: log + skip (do NOT cache in dream_verdicts).
        if chunks.len() > config.max_chunks_per_transcript {
            skips.push(SkipReport {
                file_path: fp.clone(),
                reason: format!(
                    "oversize_after_split: {}/{}",
                    chunks.len(),
                    config.max_chunks_per_transcript
                ),
            });
            continue;
        }

        let is_chunked = chunks.len() > 1;
        let subagent_model = normalize_subagent_model(&config.model);
        for (i, chunk) in chunks.iter().enumerate() {
            let prompt = build_synthesis_prompt(t, chunk, i, chunks.len());
            let idempotency_key = if is_chunked {
                format!("dream:synth:{fp}:{hash16}:c{i}of{}", chunks.len())
            } else {
                format!("dream:synth:{fp}:{hash16}")
            };
            let input = MinionJobInput {
                name: "subagent".to_string(),
                data: Some(json!({
                    "prompt": prompt,
                    "model": subagent_model,
                    "max_turns": 30,
                    "allowed_slug_prefixes": allowed,
                })),
                max_stalled: Some(3),
                on_child_fail: Some(ChildFailPolicy::Continue),
                timeout_ms: Some(30 * 60 * 1000),
                idempotency_key: Some(idempotency_key),
                ..Default::default()
            };
            let job = queue.add(&input).await?;
            child_ids.push(job.id);
        }
    }

    // Wait for every child to reach a terminal state.
    let mut child_outcomes: Vec<ChildOutcome> = Vec::new();
    for job_id in &child_ids {
        let status = match wait_for_completion(
            &queue,
            *job_id,
            WaitOpts {
                timeout_ms: opts.wait_timeout_ms.or(Some(35 * 60 * 1000)),
                poll_ms: Some(5_000),
            },
        )
        .await
        {
            Ok(job) => job.status.as_str().to_string(),
            Err(WaitError::Timeout { .. }) => "timeout".to_string(),
            Err(e) => {
                child_outcomes.push(ChildOutcome {
                    job_id: *job_id,
                    status: format!("error: {e}"),
                });
                continue;
            }
        };
        child_outcomes.push(ChildOutcome {
            job_id: *job_id,
            status,
        });
    }

    // Best-effort slug collection (subagent_tool_executions not mirrored — 1-3-4-6).
    let written_refs = collect_child_put_page_slugs(engine, &child_ids).await;

    let submitted = worth_processing.len() - skips.len();
    let mut r = SynthesizePhaseResult::ok(&format!(
        "{} transcript(s) synthesized",
        submitted
    ));
    r.transcripts_discovered = transcripts.len() as u64;
    r.transcripts_processed = submitted as u64;
    r.pages_written = written_refs.len() as u64;
    r.children_submitted = child_ids.len() as u64;
    r.verdicts = verdicts;
    r.child_outcomes = child_outcomes;
    r.skips = skips;
    Ok(r)
}

/// Harvest slugs the child subagents wrote via `brain_put_page`. Fail-soft
/// (see module docs): returns an empty list when the engine lacks
/// `execute_raw` or the `subagent_tool_executions` table. The subagents write
/// pages to the engine DB directly (canonical), so `pages_written` is
/// best-effort 0 in Rust.
async fn collect_child_put_page_slugs(engine: &dyn BrainEngine, job_ids: &[i64]) -> Vec<String> {
    if job_ids.is_empty() {
        return Vec::new();
    }
    let placeholders = (0..job_ids.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT COALESCE(input->>'slug', input->>'slug') AS slug \
         FROM subagent_tool_executions \
         WHERE job_id IN ({placeholders}) AND tool_name = 'brain_put_page' AND status = 'complete' \
         ORDER BY 1"
    );
    let params: Vec<&(dyn erased_serde::Serialize + Sync)> = job_ids
        .iter()
        .map(|id| id as &(dyn erased_serde::Serialize + Sync))
        .collect();
    match engine.execute_raw(&sql, &params).await {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|r| match r {
                Value::Object(map) => map
                    .get("slug")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatError, ChatResult, ChatUsage, StopReason};
    use crate::engine::{BrainEngine, EngineConfig, InMemoryEngine};
    use crate::minions::types::MinionJobStatus;

    async fn setup() -> InMemoryEngine {
        let engine = InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        engine
    }

    /// A transcript body longer than `min_chars` (2000) so discovery keeps it.
    fn long_content() -> String {
        "User: I realized a new mental model for decision-making under uncertainty. \
         It reframes options as bets rather than choices, and forces me to size \
         each position by how wrong I could be. "
            .repeat(40)
    }

    /// A stub chat provider that returns a fixed JSON verdict.
    #[derive(Debug)]
    struct VerdictStub {
        worth: bool,
        reasons: Vec<String>,
    }

    #[async_trait::async_trait]
    impl crate::ai::chat::ChatProvider for VerdictStub {
        async fn chat(&self, _opts: ChatOpts) -> std::result::Result<ChatResult, ChatError> {
            let text = serde_json::to_string(&serde_json::json!({
                "worth_processing": self.worth,
                "reasons": self.reasons,
            }))
            .unwrap();
            Ok(ChatResult {
                text,
                blocks: vec![],
                stop_reason: StopReason::End,
                usage: ChatUsage::default(),
                model: "anthropic:claude-haiku-4-5-20251001".to_string(),
                provider_id: "anthropic".to_string(),
                provider_metadata: None,
            })
        }
    }

    #[tokio::test]
    async fn not_configured_skips_without_corpus_dir() {
        // corpus_dir unset → skip "not_configured" (config wiring is 1-3-4-6).
        let engine = setup().await;
        let r = run_phase_synthesize(
            &engine,
            None,
            &SynthesizePhaseOpts::default(),
        )
        .await
        .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("not_configured"));
    }

    #[tokio::test]
    async fn no_transcripts_ok() {
        let dir = tempfile::tempdir().unwrap(); // empty → no transcripts
        let engine = setup().await;
        let r = run_phase_synthesize(
            &engine,
            None,
            &SynthesizePhaseOpts {
                corpus_dir: Some(dir.path().to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.transcripts_discovered, 0);
        assert_eq!(r.children_submitted, 0);
    }

    #[tokio::test]
    async fn dry_run_judges_but_does_not_synthesize() {
        // A corpus dir with one transcript; verdict stub says worth=true.
        // dry_run → verdicts cached, zero children submitted.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("2026-07-30-session.md");
        std::fs::write(&p, long_content()).unwrap();

        let engine = setup().await;
        let stub = VerdictStub {
            worth: true,
            reasons: vec!["new mental model".into()],
        };
        let r = run_phase_synthesize(
            &engine,
            Some(&stub),
            &SynthesizePhaseOpts {
                corpus_dir: Some(dir.path().to_string_lossy().to_string()),
                dry_run: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert!(r.dry_run);
        assert_eq!(r.transcripts_discovered, 1);
        assert_eq!(r.children_submitted, 0);
        assert_eq!(r.verdicts.len(), 1);
        assert!(r.verdicts[0].worth);
        assert!(!r.verdicts[0].cached); // freshly judged (but persisted) in dry-run

        // Re-run without dry_run: verdict is now a cache hit, no second judge
        // call needed, but still fans out (worth=true).
        let r2 = run_phase_synthesize(
            &engine,
            Some(&stub),
            &SynthesizePhaseOpts {
                corpus_dir: Some(dir.path().to_string_lossy().to_string()),
                wait_timeout_ms: Some(150),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r2.transcripts_discovered, 1);
        assert_eq!(r2.children_submitted, 1); // one chunk → one subagent job
        assert!(r2.verdicts[0].cached); // cache hit on re-run
        assert_eq!(r2.child_outcomes.len(), 1);
        // InMemory has no worker → waits out the short timeout → "timeout".
        assert_eq!(r2.child_outcomes[0].status, "timeout");
    }

    #[tokio::test]
    async fn judge_not_worth_skips_synthesis() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("2026-07-30-ops.md");
        std::fs::write(&p, long_content()).unwrap();

        let engine = setup().await;
        let stub = VerdictStub {
            worth: false,
            reasons: vec!["routine ops".into()],
        };
        let r = run_phase_synthesize(
            &engine,
            Some(&stub),
            &SynthesizePhaseOpts {
                corpus_dir: Some(dir.path().to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.transcripts_processed, 0);
        assert_eq!(r.children_submitted, 0);
        assert!(!r.verdicts[0].worth);
    }

    #[tokio::test]
    async fn no_chat_provider_skips_judging() {
        // A transcript exists but no chat provider → judged worth=false with
        // explicit "no configured provider" reason; phase still completes.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("2026-07-30-session.md");
        std::fs::write(&p, long_content()).unwrap();

        let engine = setup().await;
        let r = run_phase_synthesize(
            &engine,
            None,
            &SynthesizePhaseOpts {
                corpus_dir: Some(dir.path().to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.children_submitted, 0);
        assert_eq!(r.verdicts.len(), 1);
        assert!(!r.verdicts[0].worth);
        assert!(r.verdicts[0]
            .reasons
            .iter()
            .any(|x| x.contains("no configured provider")));
    }

    #[tokio::test]
    async fn chunked_transcript_submits_one_job_per_chunk() {
        // A large transcript exceeds the per-chunk budget → multiple chunks →
        // multiple subagent jobs with chunked idempotency keys.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("2026-07-30-long.md");
        let big = "User: a deep reflection.\n".repeat(2000); // well over budget
        std::fs::write(&p, &big).unwrap();

        let engine = setup().await;
        let stub = VerdictStub {
            worth: true,
            reasons: vec!["high signal".into()],
        };
        let r = run_phase_synthesize(
            &engine,
            Some(&stub),
            &SynthesizePhaseOpts {
                corpus_dir: Some(dir.path().to_string_lossy().to_string()),
                wait_timeout_ms: Some(150),
                // tiny budget to force chunking
                max_prompt_tokens: Some(1000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.transcripts_discovered, 1);
        assert!(r.children_submitted > 1, "expected multiple chunks → multiple jobs");
        // every child outcome is "timeout" (no worker in InMemory)
        assert!(r.child_outcomes.iter().all(|c| c.status == "timeout"));
    }
}
