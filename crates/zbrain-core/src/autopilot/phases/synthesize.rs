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
//! 5. best-effort slug collection via
//!    [`BrainEngine::collect_child_put_page_slugs`] over the
//!    `subagent_tool_executions` table (read side wired in 1-3-4-6; the write
//!    side is registered in docs/plans/KNOWN-GAPS.md (G63))
//! 6. disk dual-write: [`reverse_write_refs`] mirrors each synthesized page
//!    back to `brainDir/<slug>.md` (or `brainDir/.sources/<id>/<slug>.md` for
//!    non-default sources), and [`write_summary_page`] writes the
//!    `dream-cycle-summaries/<date>.md` index page to both the engine and disk
//!    (1-3-4-5, faithful port of TS `reverseWriteRefs` + `writeSummaryPage`).
//!
//! ## Rust deviations (documented so the port stays honest)
//!
//! - **Config (1-3-4-6, wired)**: [`load_synth_config`] now reads
//!   `dream.synthesize.*` from the engine config store via
//!   `engine.get_config` (defaulting to TS values when unset). Ad-hoc overrides
//!   in [`SynthesizePhaseOpts`] still win (mirrors TS `inputFile`). The engine
//!   config store (`config` table) is backed by `get_config`/`set_config`/
//!   `unset_config` on `BrainEngine`, so the phase is fully config-driven.
//! - **Disk dual-write**: implemented faithfully (1-3-4-5). When `brain_dir`
//!   is `None` (config not wired yet), the disk mirror is skipped per-page but
//!   the summary `put_page` still runs (engine-canonical, no disk needed).
//!   `pages_written` is harvested via `engine.collect_child_put_page_slugs`
//!   over the `subagent_tool_executions` table (added in 1-3-4-6). The **write
//!   path** (minion `brain_put_page` tool recording executions) is registered
//!   in docs/plans/KNOWN-GAPS.md (G63), so the table is read but rarely
//!   populated until that lands — `reverse_write_refs` typically has no refs
//!   to mirror yet.
//! - **Cooldown (1-3-4-6, wired)**: [`check_cooldown`] reads
//!   `dream.synthesize.last_completion_ts` and skips with `cooldown_active`
//!   when within `cooldown_hours`; the timestamp is written on successful
//!   completion via `engine.set_config`.
//! - **Prior contradictions block**: TS surfaces `loadPriorContradictionsBlock`
//!   into the subagent prompt. Best-effort informational only; omitted here.
//! - **Worker execution**: the subagent itself runs in the minion worker (wired
//!   with a chat provider), so the cycle phase does **not** need chat for the
//!   synthesis step — only for the significance *judge* (which runs in-phase).
//!   Matches the TS design where the phase enqueues rather than calling the
//!   LLM directly for synthesis.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::ai::chat::{ChatError, ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::autopilot::phases::context_budget::{
    compute_chunk_char_budget, split_transcript_by_budget, DEFAULT_MAX_CHUNKS,
};
use crate::autopilot::phases::transcript_discovery::{
    discover_transcripts, DiscoveredTranscript, DiscoverOpts,
};
use crate::engine::{
    BrainEngine, DreamVerdict, DreamVerdictInput, GetPageOpts, Page, PageInput,
};
use crate::minions::queue::MinionQueue;
use crate::minions::types::{ChildFailPolicy, MinionJobInput};
use crate::minions::wait_for_completion::{wait_for_completion, WaitError, WaitOpts};
use crate::Result;

/// Options for [`run_phase_synthesize`]. Mirrors TS `SynthesizePhaseOpts`.
pub struct SynthesizePhaseOpts {
    /// Brain directory for disk reverse-write (1-3-4-5). When `Some`, the
    /// synthesized pages and the summary index are mirrored to `*.md` files
    /// under this dir; when `None`, disk writes are skipped (the engine DB
    /// remains canonical). Mirrors TS `opts.brainDir`.
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
    pub disk_files_written: u64,
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
/// Load synth config from the engine config store (1-3-4-6).
///
/// Mirrors TS `loadSynthConfig` (synthesize.ts:598): reads `dream.synthesize.*`
/// keys via `engine.get_config`. Ad-hoc overrides in `SynthesizePhaseOpts`
/// still win over the stored value (mirrors TS `inputFile` path). Missing keys
/// fall back to TS defaults.
async fn load_synth_config(
    engine: &dyn BrainEngine,
    opts: &SynthesizePhaseOpts,
) -> Result<SynthConfig> {
    let enabled_raw = engine.get_config("dream.synthesize.enabled").await?;
    let stored_corpus_dir = engine.get_config("dream.synthesize.session_corpus_dir").await?;
    let meeting_transcripts_dir = engine
        .get_config("dream.synthesize.meeting_transcripts_dir")
        .await?;
    let min_chars_raw = engine.get_config("dream.synthesize.min_chars").await?;
    let exclude_raw = engine.get_config("dream.synthesize.exclude_patterns").await?;
    let cooldown_raw = engine.get_config("dream.synthesize.cooldown_hours").await?;
    let max_prompt_raw = engine.get_config("dream.synthesize.max_prompt_tokens").await?;
    let max_chunks_raw = engine
        .get_config("dream.synthesize.max_chunks_per_transcript")
        .await?;

    let enabled = match enabled_raw.as_deref() {
        Some("false") | Some("0") => false,
        _ => true,
    };
    // Ad-hoc override wins (mirrors TS inputFile), else stored corpus_dir.
    let corpus_dir = opts.corpus_dir.clone().or(stored_corpus_dir);
    let min_chars = opts
        .min_chars
        .or_else(|| min_chars_raw.and_then(|s| s.parse::<usize>().ok()))
        .unwrap_or(2000);
    let exclude_patterns = exclude_raw
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();
    let cooldown_hours = cooldown_raw
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(12);
    let max_prompt_tokens = opts
        .max_prompt_tokens
        .or_else(|| max_prompt_raw.and_then(|s| s.parse::<u32>().ok()));
    let max_chunks_per_transcript = opts
        .max_chunks_per_transcript
        .or_else(|| max_chunks_raw.and_then(|s| s.parse::<usize>().ok()))
        .unwrap_or(DEFAULT_MAX_CHUNKS);

    Ok(SynthConfig {
        enabled,
        corpus_dir,
        meeting_transcripts_dir,
        min_chars,
        exclude_patterns,
        model: opts
            .model
            .clone()
            .unwrap_or_else(|| "sonnet".to_string()),
        verdict_model: opts
            .verdict_model
            .clone()
            .unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string()),
        cooldown_hours,
        max_prompt_tokens,
        max_chunks_per_transcript,
    })
}

/// Cooldown gate state (1-3-4-6). Mirrors TS `checkCooldown` (synthesize.ts:664).
struct CooldownState {
    active: bool,
    expires_at: String,
}

/// Read `dream.synthesize.last_completion_ts`; if set and within
/// `cooldown_hours`, returns `active = true` with the ISO expiry timestamp.
/// Mirrors TS `checkCooldown`.
async fn check_cooldown(
    engine: &dyn BrainEngine,
    cooldown_hours: u64,
) -> Result<CooldownState> {
    let last_raw = engine
        .get_config("dream.synthesize.last_completion_ts")
        .await?;
    let Some(last) = last_raw else {
        return Ok(CooldownState {
            active: false,
            expires_at: String::new(),
        });
    };
    let last_ts = match chrono::DateTime::parse_from_rfc3339(&last) {
        Ok(t) => t.with_timezone(&chrono::Utc),
        Err(_) => {
            return Ok(CooldownState {
                active: false,
                expires_at: String::new(),
            })
        }
    };
    let now = chrono::Utc::now();
    let elapsed_hours = now.signed_duration_since(last_ts).num_hours();
    if elapsed_hours < cooldown_hours as i64 {
        let expires = last_ts + chrono::Duration::hours(cooldown_hours as i64);
        return Ok(CooldownState {
            active: true,
            expires_at: expires.to_rfc3339(),
        });
    }
    Ok(CooldownState {
        active: false,
        expires_at: String::new(),
    })
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
    let config = load_synth_config(engine, opts).await?;

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

    // Cooldown gate (1-3-4-6). Mirrors TS checkCooldown: if the last
    // completion is within `cooldown_hours`, skip with `cooldown_active`.
    let cooldown = check_cooldown(engine, config.cooldown_hours).await?;
    if cooldown.active {
        return Ok(SynthesizePhaseResult::skipped(
            "cooldown_active",
            &format!(
                "synthesize cooled down until {} ({}h cooldown)",
                cooldown.expires_at, config.cooldown_hours
            ),
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

    // Best-effort slug+source collection (subagent_tool_executions not mirrored
    // — 1-3-4-6). Each entry is `(slug, source_id)`.
    let written_refs = engine.collect_child_put_page_slugs(&child_ids).await?;
    let written_slugs: Vec<String> = written_refs.iter().map(|(s, _)| s.clone()).collect();

    // Disk dual-write (1-3-4-5). Mirrors TS reverseWriteRefs + writeSummaryPage.
    // When brain_dir is unset, the disk mirror is skipped but the summary
    // put_page (engine-canonical) still runs.
    let brain_dir_path = opts.brain_dir.as_deref().map(Path::new);
    let disk_files_written = if let Some(bd) = brain_dir_path {
        reverse_write_refs(engine, bd, &written_refs).await
    } else {
        0
    };
    let summary_date = today();
    let summary_slug = format!("dream-cycle-summaries/{summary_date}");
    write_summary_page(
        engine,
        brain_dir_path,
        &summary_slug,
        &summary_date,
        &written_slugs,
        &child_outcomes,
    )
    .await;

    // Cooldown timestamp is updated only on a successful completion (TS:542).
    let _ = engine
        .set_config(
            "dream.synthesize.last_completion_ts",
            &chrono::Utc::now().to_rfc3339(),
        )
        .await;

    let submitted = worth_processing.len() - skips.len();
    let mut r = SynthesizePhaseResult::ok(&format!(
        "{} transcript(s) synthesized",
        submitted
    ));
    r.transcripts_discovered = transcripts.len() as u64;
    r.transcripts_processed = submitted as u64;
    r.pages_written = written_refs.len() as u64;
    r.children_submitted = child_ids.len() as u64;
    r.disk_files_written = disk_files_written as u64;
    r.verdicts = verdicts;
    r.child_outcomes = child_outcomes;
    r.skips = skips;
    Ok(r)
}

// `collect_child_put_page_slugs` now lives on `BrainEngine` as a trait method
// (1-3-4-6); the synthesize phase calls `engine.collect_child_put_page_slugs`.

// ── Disk dual-write (1-3-4-5) ──────────────────────────────────────────────
//
// Faithful port of TS `reverseWriteRefs` + `writeSummaryPage` from
// src/core/cycle/synthesize.ts. These mirror the engine-canonical pages back
// to `brainDir/*.md` so the on-disk vault stays in sync. All per-item failures
// are non-fatal (stderr, continue) — mirroring TS.

/// Current date as `YYYY-MM-DD` (TS `today()`).
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Canonical source_id regex (port of TS `SOURCE_ID_RE` in source-id.ts).
/// 1-32 lowercase alnum, optional interior hyphens, no edge hyphens.
fn source_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$").unwrap())
}

/// Validate a source_id is filesystem-safe. Port of TS `assertValidSourceId`.
fn validate_source_id(s: &str) -> std::result::Result<(), String> {
    if source_id_re().is_match(s) {
        Ok(())
    } else {
        Err(format!(
            "Invalid source_id: {s:?}. Must be 1-32 lowercase alnum chars with optional \
             interior hyphens (matches ^[a-z0-9](?:[a-z0-9-]{{0,30}}[a-z0-9])?$)"
        ))
    }
}

/// Compute the on-disk path for a `(brain_dir, slug, source_id)` tuple per the
/// v0.32.8 multi-source filing layout. Port of TS `resolvePageFilePath`.
fn resolve_page_file_path(brain_dir: &Path, slug: &str, source_id: &str) -> PathBuf {
    if source_id == "default" {
        brain_dir.join(format!("{slug}.md"))
    } else {
        brain_dir
            .join(".sources")
            .join(source_id)
            .join(format!("{slug}.md"))
    }
}

/// Render a `Page` row to its canonical on-disk markdown form. Port of TS
/// `serializePageToMarkdown` (which delegates to `serializeMarkdown`). The
/// dream-output identity stamp is applied via `overrides` so every reverse-write
/// path carries the same marker that `transcript_discovery::is_dream_output`
/// checks.
fn render_page_to_markdown(page: &Page, tags: &[String], overrides: &Value) -> String {
    let mut fm: Map<String, Value> = Map::new();
    fm.insert("type".into(), Value::String(page.page_type.clone()));
    fm.insert("title".into(), Value::String(page.title.clone()));
    if let Value::Object(base) = &page.frontmatter {
        for (k, v) in base {
            fm.insert(k.clone(), v.clone());
        }
    }
    if let Value::Object(ov) = overrides {
        for (k, v) in ov {
            fm.insert(k.clone(), v.clone());
        }
    }
    if !tags.is_empty() {
        fm.insert(
            "tags".into(),
            Value::Array(tags.iter().map(|t| Value::String(t.clone())).collect()),
        );
    }
    crate::markdown::serialize_markdown(&Value::Object(fm), &page.compiled_truth, &page.timeline, tags)
}

/// Mirror each synthesized `(slug, source_id)` page back to disk. Port of TS
/// `reverseWriteRefs`. Returns the number of files written. Per-ref failures
/// are non-fatal.
async fn reverse_write_refs(
    engine: &dyn BrainEngine,
    brain_dir: &Path,
    refs: &[(String, String)],
) -> usize {
    let mut count = 0;
    for (slug, source_id) in refs {
        if let Err(e) = validate_source_id(source_id) {
            eprintln!("[dream] reverse-write {slug}@{source_id} skipped: {e}");
            continue;
        }
        let page = match engine
            .get_page(slug, &GetPageOpts {
                source_id: Some(source_id.clone()),
                include_deleted: false,
            })
            .await
        {
            Ok(Some(p)) => p,
            Ok(None) => continue, // page gone → skip (non-fatal)
            Err(e) => {
                eprintln!("[dream] reverse-write {slug}@{source_id} get_page failed: {e}");
                continue;
            }
        };
        let tags = engine
            .get_tags(slug, Some(source_id))
            .await
            .unwrap_or_default();
        let md = render_page_to_markdown(&page, &tags, &json!({
            "dream_generated": true,
            "dream_cycle_date": today(),
        }));
        let path = resolve_page_file_path(brain_dir, slug, source_id);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("[dream] reverse-write mkdir {parent:?} failed: {e}");
                continue;
            }
        }
        match std::fs::write(&path, md) {
            Ok(()) => count += 1,
            Err(e) => eprintln!("[dream] reverse-write {slug}@{source_id} write failed: {e}"),
        }
    }
    count
}

/// Write the `dream-cycle-summaries/<date>` index page. Port of TS
/// `writeSummaryPage`: builds the markdown summary, `put_page`s it to the
/// engine (canonical), and (when `brain_dir` is set) also mirrors it to disk.
/// Disk write failure is non-fatal.
async fn write_summary_page(
    engine: &dyn BrainEngine,
    brain_dir: Option<&Path>,
    summary_slug: &str,
    summary_date: &str,
    written_slugs: &[String],
    child_outcomes: &[ChildOutcome],
) {
    let completed = child_outcomes
        .iter()
        .filter(|c| c.status == "completed")
        .count();
    let failed = child_outcomes.len() - completed;

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# Dream cycle {summary_date}"));
    lines.push(String::new());
    lines.push(format!("**Children:** {completed} completed, {failed} failed/timeout."));
    lines.push(format!("**Pages written:** {}.", written_slugs.len()));
    lines.push(String::new());
    if !written_slugs.is_empty() {
        lines.push("## Pages".into());
        lines.push(String::new());
        for s in written_slugs {
            lines.push(format!("- [[{s}]]"));
        }
        lines.push(String::new());
    }
    let body = lines.join("\n");

    // Engine write (canonical) — direct orchestrator put_page, no allow-list.
    let input = PageInput {
        page_type: "note".to_string(),
        title: format!("Dream cycle {summary_date}"),
        compiled_truth: body.clone(),
        timeline: None,
        frontmatter: Some(json!({
            "dream_generated": true,
            "dream_cycle_date": summary_date,
            "tags": ["dream-cycle"],
        })),
        ..Default::default()
    };
    if let Err(e) = engine.put_page(summary_slug, Some("default"), &input).await {
        eprintln!("[dream] summary put_page failed: {e}");
    }

    // Disk mirror (orchestrator dual-write).
    if let Some(bd) = brain_dir {
        let md = crate::markdown::serialize_markdown(
            &json!({
                "dream_generated": true,
                "dream_cycle_date": summary_date,
            }),
            &body,
            "",
            &["dream-cycle".to_string()],
        );
        let path = bd.join(format!("{summary_slug}.md"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, md) {
            eprintln!("[dream] summary file-write failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatError, ChatResult, ChatUsage, StopReason};
    use crate::engine::{EngineConfig, InMemoryEngine};
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
    async fn config_session_corpus_dir_drives_run_when_override_unset() {
        // load_synth_config now reads dream.synthesize.session_corpus_dir from
        // the engine config store (1-3-4-6); opts.corpus_dir stays None.
        let dir = tempfile::tempdir().unwrap();
        let engine = setup().await;
        engine
            .set_config(
                "dream.synthesize.session_corpus_dir",
                dir.path().to_string_lossy().as_ref(),
            )
            .await
            .unwrap();
        let r = run_phase_synthesize(&engine, None, &SynthesizePhaseOpts::default())
            .await
            .unwrap();
        // corpus_dir resolved from config (not not_configured); empty dir → ok.
        assert_eq!(r.status, "ok");
        assert_eq!(r.summary, "no transcripts to process");
        assert_eq!(r.reason, None);
    }

    #[tokio::test]
    async fn cooldown_active_skips_run() {
        let dir = tempfile::tempdir().unwrap();
        let engine = setup().await;
        engine
            .set_config(
                "dream.synthesize.session_corpus_dir",
                dir.path().to_string_lossy().as_ref(),
            )
            .await
            .unwrap();
        // recently completed → within default 12h cooldown
        engine
            .set_config(
                "dream.synthesize.last_completion_ts",
                &chrono::Utc::now().to_rfc3339(),
            )
            .await
            .unwrap();
        let r = run_phase_synthesize(&engine, None, &SynthesizePhaseOpts::default())
            .await
            .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("cooldown_active"));
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

    // ── 1-3-4-5 disk dual-write ──────────────────────────────────────────

    /// Build a minimal PageInput for seeding the engine.
    fn page_input(title: &str, body: &str) -> PageInput {
        PageInput {
            page_type: "note".to_string(),
            title: title.to_string(),
            compiled_truth: body.to_string(),
            timeline: None,
            frontmatter: Some(json!({ "x": 1 })),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn reverse_write_refs_mirrors_page_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let engine = setup().await;
        engine
            .put_page("wiki/foo", Some("default"), &page_input("Foo", "body text"))
            .await
            .unwrap();

        let n = reverse_write_refs(
            &engine,
            dir.path(),
            &[("wiki/foo".into(), "default".into())],
        )
        .await;
        assert_eq!(n, 1);
        let p = dir.path().join("wiki/foo.md");
        assert!(p.exists(), "default-source page should write to brainDir/<slug>.md");
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("dream_generated: true"));
        assert!(content.contains("body text"));
    }

    #[tokio::test]
    async fn reverse_write_refs_uses_sources_subdir_for_nondefault() {
        let dir = tempfile::tempdir().unwrap();
        let engine = setup().await;
        engine
            .put_page("wiki/bar", Some("mysrc"), &page_input("Bar", "body"))
            .await
            .unwrap();

        let n = reverse_write_refs(
            &engine,
            dir.path(),
            &[("wiki/bar".into(), "mysrc".into())],
        )
        .await;
        assert_eq!(n, 1);
        let p = dir.path().join(".sources").join("mysrc").join("wiki/bar.md");
        assert!(
            p.exists(),
            "non-default source should file under brainDir/.sources/<id>/<slug>.md"
        );
    }

    #[tokio::test]
    async fn reverse_write_refs_skips_missing_page() {
        let dir = tempfile::tempdir().unwrap();
        let engine = setup().await;
        let n = reverse_write_refs(
            &engine,
            dir.path(),
            &[("wiki/missing".into(), "default".into())],
        )
        .await;
        assert_eq!(n, 0);
        assert!(!dir.path().join("wiki/missing.md").exists());
    }

    #[tokio::test]
    async fn write_summary_page_puts_engine_and_mirrors_disk() {
        let dir = tempfile::tempdir().unwrap();
        let engine = setup().await;
        let outcomes = vec![ChildOutcome {
            job_id: 1,
            status: "completed".into(),
        }];
        write_summary_page(
            &engine,
            Some(dir.path()),
            "dream-cycle-summaries/2026-07-30",
            "2026-07-30",
            &["wiki/foo".into()],
            &outcomes,
        )
        .await;

        // Engine write (canonical).
        let page = engine
            .get_page(
                "dream-cycle-summaries/2026-07-30",
                &GetPageOpts {
                    source_id: Some("default".into()),
                    include_deleted: false,
                },
            )
            .await
            .unwrap();
        assert!(page.is_some());

        // Disk mirror.
        let p = dir.path().join("dream-cycle-summaries/2026-07-30.md");
        assert!(p.exists());
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("Dream cycle 2026-07-30"));
        assert!(content.contains("- [[wiki/foo]]"));
    }

    #[tokio::test]
    async fn run_phase_mirrors_summary_index_to_disk() {
        // brain_dir set → summary index mirrored to disk even with no
        // harvested slugs (reverse_write_refs has nothing to mirror, but the
        // summary page dual-write still runs).
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
                brain_dir: Some(dir.path().to_string_lossy().to_string()),
                wait_timeout_ms: Some(150),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.children_submitted, 1);
        assert_eq!(r.disk_files_written, 0); // no harvested slugs yet

        // Summary index .md mirrored to disk.
        let summary_dir = dir.path().join("dream-cycle-summaries");
        let mut found = false;
        if let Ok(entries) = std::fs::read_dir(&summary_dir) {
            for e in entries.flatten() {
                if e.path()
                    .extension()
                    .map_or(false, |x| x == "md")
                {
                    found = true;
                }
            }
        }
        assert!(found, "summary index .md should be mirrored to disk");
    }
}
