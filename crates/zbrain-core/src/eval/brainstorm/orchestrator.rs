//! v0.37.0 — brainstorm + LSD orchestrator.
//!
//! Faithful port of `src/core/orchestrator.ts`. Shared 4-phase pipeline driven
//! by a [`BrainstormProfile`] config object:
//!
//!   Phase 1: retrieve close-set via hybridSearch (K=4 brainstorm, K=2 LSD).
//!   Phase 2: fetch far-set via domain-bank (M=6 brainstorm, M=12 LSD).
//!   Phase 3: cross-generate ideas via the chat provider (one call per close × far).
//!   Phase 4: judge via [`run_judge`] (single-file, two configs).
//!
//! Deviations from TS (documented, Q3-MVP-scoped):
//!   * The TTY grace window + SIGINT handling in `previewCostAndWait` is
//!     dropped — non-interactive callers always proceed; the `--max-cost`
//!     hard ceiling still aborts the run. Keeps suites from hanging.
//!   * `BudgetTracker` runtime telemetry is replaced by a single estimate-vs
//!     ceiling check (the circuit breaker is the load-bearing guardrail).
//!   * Checkpoint resume playback is TODO (skeleton in `checkpoint.rs`); the
//!     run_id is still derived + reported.
//!   * Calibration anti-bias context is injected via
//!     `BrainstormOptions.active_bias_tags` rather than a DB lookup
//!     (cold-start fallback = empty → `None`).

use crate::ai::chat::{ChatContent, ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::embedding::EmbeddingClient;
use crate::engine::{domain_bank_prefix, BrainEngine, SearchResult};
use crate::eval::brainstorm::checkpoint::compute_run_id;
use crate::eval::brainstorm::domain_bank::{fetch_far, CloseRef, FetchFarOpts, FarPage};
use crate::eval::brainstorm::error_classify::classify_brainstorm_error;
use crate::eval::brainstorm::judges::{
    run_judge, BRAINSTORM_JUDGE_CONFIG, JudgeConfig, JudgeIdea, JudgeIdeaResult, LSD_JUDGE_CONFIG,
    RunJudgeOptions,
};
use crate::search::hybrid_search;
use crate::search::HybridSearchOpts;
use regex::Regex;
use std::sync::Arc;
use std::sync::LazyLock;

/// Cost pricing fallback (per 1M tokens). Anchored on Sonnet-class pricing;
/// TS looked up `ANTHROPIC_PRICING[model]` and fell back to this same pair.
const DEFAULT_PRICING: (f64, f64) = (3.0, 15.0);

/// One brainstorm vs LSD config object (D1 fold).
#[derive(Debug, Clone, Copy)]
pub struct BrainstormProfile {
    /// Stable label — used in stderr lines, frontmatter, audit.
    pub label: &'static str,
    /// Close-set size from hybridSearch. brainstorm=4, lsd=2.
    pub k_close: usize,
    /// Far-set size from domain-bank. brainstorm=6, lsd=12.
    pub m_far: usize,
    /// Ideas to generate per (close × far) cross.
    pub ideas_per_cross: usize,
    /// Generation "temperature" (informational; recorded, not enforced here).
    pub temperature: f64,
    /// Domain-bank stale-bias toggle. LSD only.
    pub stale_bias: bool,
    /// Judge config (rubric + threshold + LSD inversion rule).
    pub judge_config: &'static JudgeConfig,
    /// Whether to save by default. brainstorm=true, lsd=false.
    pub default_save: bool,
    /// Frontmatter `mode:` value the dream-cycle hook reads (D4).
    pub frontmatter_mode: &'static str,
    /// Generator system-prompt suffix — what's the vibe?
    pub generator_voice: &'static str,
    /// Optional generator-side constraint (LSD: axiomatic inversions required).
    pub generator_constraint: Option<&'static str>,
}

pub const BRAINSTORM_PROFILE: BrainstormProfile = BrainstormProfile {
    label: "brainstorm",
    k_close: 4,
    m_far: 6,
    ideas_per_cross: 3,
    temperature: 0.7,
    stale_bias: false,
    judge_config: &BRAINSTORM_JUDGE_CONFIG,
    default_save: true,
    frontmatter_mode: "brainstorm",
    generator_voice: "Defensible, cite-heavy. An analyst riffing with their own notes.",
    generator_constraint: None,
};

pub const LSD_PROFILE: BrainstormProfile = BrainstormProfile {
    label: "lsd",
    k_close: 2,
    m_far: 12,
    ideas_per_cross: 4,
    temperature: 0.95,
    stale_bias: true,
    judge_config: &LSD_JUDGE_CONFIG,
    default_save: false,
    frontmatter_mode: "lsd",
    generator_voice:
        "Your brain at 3am noticing a connection between things it has no business connecting.",
    generator_constraint: Some(
        "Every idea MUST invert at least one implicit axiom (X is good → X is the problem; \
         everyone does Y → opposite; dominant narrative → hidden cause).",
    ),
};

/// Caller-facing options for [`run_brainstorm`].
#[derive(Debug, Clone, Default)]
pub struct BrainstormOptions {
    pub question: String,
    /// Profile selects brainstorm vs LSD; defaults to [`BRAINSTORM_PROFILE`].
    pub profile: Option<BrainstormProfile>,
    /// Override the default chat model.
    pub model_override: Option<String>,
    /// Source scope.
    pub source_id: Option<String>,
    /// Federated read scope.
    pub source_ids: Option<Vec<String>>,
    /// Maximum projected cost in USD before the run aborts. Default $5.
    pub max_cost_usd: Option<f64>,
    /// Hard cap on the domain-bank far set (cost guardrail). Default 50.
    pub max_far_set: Option<usize>,
    /// Override the model used for the judge phase. Falls back to
    /// `model_override` then the chat provider default.
    pub judge_model: Option<String>,
    /// Max ideas per judge LLM call. Default 100.
    pub max_ideas_per_judge_call: Option<usize>,
    /// Calibration anti-bias tags injected into the judge (cold-start = None).
    pub active_bias_tags: Option<Vec<String>>,
}

/// One idea emitted to the user, with citation transparency (D6).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrainstormIdea {
    /// "01" .. "NN", stable within this run.
    pub id: String,
    /// Free-form idea body (2-4 sentences).
    pub text: String,
    /// Citation: close-set page slug.
    pub close_slug: String,
    /// Citation: far-set page slug.
    pub far_slug: String,
    /// D6 transparency badge — how far this collision actually traveled.
    pub distance_score: f64,
    /// Scoring from the judge. Absent when `judge_failed == true`.
    pub judge: Option<JudgeIdeaResult>,
    /// True iff this idea passed the judge threshold.
    pub passes: bool,
    /// True iff the judge call failed mid-run. When true `judge` is None and
    /// `passes == false`.
    pub judge_failed: bool,
}

/// Top-level orchestrator result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrainstormResult {
    pub profile_label: String,
    pub question: String,
    pub embedding_model: Option<String>,
    pub ideas: Vec<BrainstormIdea>,
    pub close_set: Vec<CloseRefForReport>,
    pub far_set: Vec<FarRefForReport>,
    pub active_bias_tags: Option<Vec<String>>,
    pub short_of_target: bool,
    pub judge_failed: bool,
    pub cost: BrainstormCost,
    /// run_id (A5) — reported for `--resume` ergonomics.
    pub run_id: String,
}

/// Close-set citation for the run header.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CloseRefForReport {
    pub slug: String,
    pub title: Option<String>,
}

/// Far-set citation for the run header.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FarRefForReport {
    pub slug: String,
    pub title: Option<String>,
    pub distance_score: f64,
    pub source: String,
}

/// Cost actuals (codex r2 #10).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BrainstormCost {
    pub estimated_usd: f64,
    pub actual_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Per-profile cost estimate. brainstorm: ~$0.05-0.15. lsd: ~$0.20-0.40.
/// Real numbers depend on the configured model; we anchor on the default
/// pricing pair (Sonnet-class). The estimate is informational — operators
/// see actuals printed at run-end.
#[must_use]
pub fn estimate_cost(profile: &BrainstormProfile, _model: &str) -> f64 {
    let crosses = profile.k_close * profile.m_far;
    let ideas = crosses * profile.ideas_per_cross;
    let in_tokens = crosses * 3000;
    let out_tokens = ideas * 250;
    let judge_in = ideas * 350;
    let judge_out = ideas * 200;
    let (input_price, output_price) = DEFAULT_PRICING;
    let in_cost = ((in_tokens + judge_in) as f64 / 1_000_000.0) * input_price;
    let out_cost = ((out_tokens + judge_out) as f64 / 1_000_000.0) * output_price;
    in_cost + out_cost
}

fn fmt_usd(n: f64) -> String {
    format!("${:.2}", n)
}

// ---------------------------------------------------------------------------
// Idea generation prompts + response parsing
// ---------------------------------------------------------------------------

/// Strip lone/orphaned UTF-16 surrogates that would crash downstream encoding.
fn sanitize_unicode(s: &str) -> String {
    // Replace lone high surrogates (D800-DBFF) not followed by a low surrogate,
    // and lone low surrogates (DC00-DFFF) not preceded by a high surrogate.
    let re_high =
        LazyLock::new(|| Regex::new(r"[\uD800-\uDBFF](?![\uDC00-\uDFFF])").unwrap());
    let re_low =
        LazyLock::new(|| Regex::new(r"(^|[^\uD800-\uDBFF])[\uDC00-\uDFFF]").unwrap());
    let s = re_high.replace_all(s, "\u{FFFD}");
    re_low
        .replace_all(&s, "$1\u{FFFD}")
        .into_owned()
}

/// Lightweight close-page view (the cross prompt only needs slug/title/body).
struct ClosePage {
    slug: String,
    title: Option<String>,
    compiled_truth: String,
}

/// Build a single (close × far) cross-generation prompt.
fn build_cross_prompt(
    profile: &BrainstormProfile,
    question: &str,
    close: &ClosePage,
    far: &FarPage,
) -> (String, String) {
    let system = format!(
        "You are an idea generator using bisociation (Arthur Koestler, 1964). You surface \
         non-trivial ideas by colliding two pages from a user's own knowledge brain.\n\n\
         Voice: {}\n\n\
         Style rules:\n\
         - Short, assertive sentences. Zero hedging.\n\
         - Each idea starts from a principle, not anecdote.\n\
         - Cite BOTH the close and far slug verbatim — these are the user's own notes.\n\
         - Never fabricate facts, figures, or quotes. Stay grounded in the cited pages.{}\n",
        profile.generator_voice,
        profile
            .generator_constraint
            .map(|c| format!("\n- {c}"))
            .unwrap_or_default(),
    );

    let close_content = sanitize_unicode(&close.compiled_truth);
    let far_content = sanitize_unicode(&far.content);
    let close_title = sanitize_unicode(close.title.as_deref().unwrap_or("(untitled)"));
    let far_title = sanitize_unicode(far.title.as_deref().unwrap_or("(untitled)"));
    let question = sanitize_unicode(question);

    let user = format!(
        "QUESTION:\n{question}\n\n\
         CLOSE PAGE (related to the question — context anchor):\n\
         [{close_slug}] {close_title}\n\
         {close_content_head}\n\n\
         FAR PAGE (from a distant region of the user's brain — the collision partner):\n\
         [{far_slug}] {far_title}\n\
         {far_content}\n\n\
         Generate exactly {n} ideas from cross-pollinating these pages.\n\n\
         Output format (one idea per ## block, no JSON):\n\
         ## Idea 1\n\
         [2-4 sentences. Reference [{close_slug}] and [{far_slug}].]\n\n\
         ## Idea 2\n\
         [2-4 sentences. Reference [{close_slug}] and [{far_slug}].]\n\n\
         (Continue for all {n} ideas.)",
        question = question,
        close_slug = close.slug,
        close_title = close_title,
        close_content_head = &close_content.chars().take(1500).collect::<String>(),
        far_slug = far.slug,
        far_title = far_title,
        far_content = far_content,
        n = profile.ideas_per_cross,
    );

    (system, user)
}

static HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?mi)^#{2,4}\s*(?:idea\s+)?\d+[.:\s\-]*").unwrap()
});
static NUMBERED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*\d+\.\s+").unwrap());

/// Parse the generator's idea output. Tolerant: matches `## Idea N`,
/// `### Idea N`, or `## N.` headings; falls back to numbered lists or blank
/// lines. Port of TS `parseIdeaResponse`.
#[must_use]
pub fn parse_idea_response(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return vec![];
    }
    let parts: Vec<String> = HEADER_RE
        .split(text)
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() >= 2 {
        return parts;
    }
    let numbered: Vec<String> = NUMBERED_RE
        .split(text)
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if numbered.len() >= 2 {
        return numbered;
    }
    text.split("\n\n")
        .map(|p| p.trim().to_string())
        .filter(|p| p.chars().count() > 30)
        .collect()
}

/// Extract the 2-segment top-level prefix from a slug (mirrors the SQL
/// `substring(slug from '^[^/]+/[^/]+')` so the orchestrator and engine agree).
#[must_use]
pub fn extract_prefix(slug: &str) -> Option<String> {
    domain_bank_prefix(slug)
}

/// Public entry point. Wraps the impl in a single fallible scope that
/// classifies Postgres SQLSTATE 57014 (query_canceled) into a
/// `brainstorm_timeout` error; non-57014 errors pass through unchanged.
pub async fn run_brainstorm(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    embedding_client: Option<Arc<EmbeddingClient>>,
    opts: &BrainstormOptions,
) -> crate::Result<BrainstormResult> {
    match run_brainstorm_impl(engine, chat, embedding_client, opts).await {
        Ok(r) => Ok(r),
        Err(e) => {
            if let Some(se) = classify_brainstorm_error(&e) {
                Err(se)
            } else {
                Err(e)
            }
        }
    }
}

async fn run_brainstorm_impl(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    embedding_client: Option<Arc<EmbeddingClient>>,
    opts: &BrainstormOptions,
) -> crate::Result<BrainstormResult> {
    let profile = opts.profile.unwrap_or(BRAINSTORM_PROFILE);
    let model_str = opts.model_override.clone().unwrap_or_else(|| "anthropic:claude-sonnet-4-6".to_string());

    // ---- Phase 0: cost estimate + hard ceiling (circuit breaker) ----
    let estimate = estimate_cost(&profile, &model_str);
    let max_cost_usd = opts.max_cost_usd.unwrap_or(5.0);
    if estimate > max_cost_usd {
        return Err(crate::Error::engine(format!(
            "{}: estimated cost {} exceeds --max-cost {} (lower --limit, raise --max-cost, or pass --max-far-set <n>)",
            profile.label,
            fmt_usd(estimate),
            fmt_usd(max_cost_usd),
        )));
    }

    // ---- Phase 1: question embedding + close-set retrieval ----
    let question_embedding: Option<Vec<f32>> = match &embedding_client {
        Some(c) => match c.embed_query(&opts.question).await {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!(
                    "[{}] WARN: question embedding failed ({}); distance scores will be neutral.",
                    profile.label, e
                );
                None
            }
        },
        None => None,
    };

    let close_results: Vec<SearchResult> = hybrid_search(
        engine,
        &opts.question,
        &HybridSearchOpts {
            limit: Some(profile.k_close),
            source_id: opts.source_id.clone(),
            embedding_client: embedding_client.clone(),
            ..Default::default()
        },
    )
    .await?;

    if close_results.is_empty() {
        eprintln!(
            "[{}] WARN: no close-set pages matched the question; proceeding with empty anchor.",
            profile.label
        );
    }
    let close_set: Vec<CloseRef> = close_results
        .iter()
        .map(|r| CloseRef {
            slug: r.page.slug.clone(),
            prefix: extract_prefix(&r.page.slug),
            representative_chunk_id: None,
        })
        .collect();

    // ---- Phase 2: domain-bank fetch ----
    let far_result = fetch_far(
        engine,
        FetchFarOpts {
            m: profile.m_far,
            close_set,
            question_embedding,
            stale_bias: profile.stale_bias,
            stale_threshold_days: 90,
            source_id: opts.source_id.clone(),
            source_ids: opts.source_ids.clone(),
            prefix_list_override: None,
            embedding_column: None,
            max_far_set: opts.max_far_set,
            prefix_shuffle_seed: None,
            prefix_cache_ttl_ms: None,
        },
    )
    .await?;

    if far_result.short_of_target {
        eprintln!(
            "[{}] WARN: Only {} distinct prefixes available, expected {} — ideas will be drawn from a narrower domain bank than usual.",
            profile.label, far_result.available_prefixes, profile.m_far
        );
    }
    if far_result.pages.is_empty() {
        return Err(crate::Error::engine(format!(
            "{}: brain has no usable far pages. Try `zbrain import <dir>` to seed cross-domain \
             content, or check the prefix cache via `zbrain doctor`.",
            profile.label
        )));
    }

    // ---- Phase 3: calibration context (cold-start fallback) ----
    let active_bias_tags = opts
        .active_bias_tags
        .as_ref()
        .filter(|t| !t.is_empty())
        .cloned();
    if active_bias_tags.is_none() {
        eprintln!(
            "[{}] calibration cold-start, judging without bias context.",
            profile.label
        );
    }

    // ---- Phase 3.5: generate ideas across (close × far) crosses ----
    // When close-set is empty, fabricate a single anchor-less close entry so
    // the cross still happens (LSD K=0 path).
    let closes_for_cross: Vec<ClosePage> = if !close_results.is_empty() {
        close_results
            .iter()
            .map(|r| ClosePage {
                slug: r.page.slug.clone(),
                title: Some(r.page.title.clone()),
                compiled_truth: r.page.compiled_truth.clone(),
            })
            .collect()
    } else {
        vec![ClosePage {
            slug: "(no anchor)".to_string(),
            title: Some("question only".to_string()),
            compiled_truth: opts.question.clone(),
        }]
    };

    struct Cross<'a> {
        close: &'a ClosePage,
        far: &'a FarPage,
    }
    let mut crosses: Vec<Cross<'_>> = Vec::new();
    for c in &closes_for_cross {
        for f in &far_result.pages {
            crosses.push(Cross { close: c, far: f });
        }
    }

    let mut total_usage = crate::ai::chat::ChatUsage::default();
    let mut cross_model = model_str.clone();
    let mut raw_ideas: Vec<RawIdea> = Vec::new();

    for cross in &crosses {
        let (system, user) = build_cross_prompt(&profile, &opts.question, cross.close, cross.far);
        let req = ChatOpts {
            model: opts.model_override.clone(),
            system: Some(system),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text(user),
            }],
            max_tokens: Some(1500),
            ..Default::default()
        };
        match chat.chat(req).await {
            Ok(result) => {
                total_usage.input_tokens += result.usage.input_tokens;
                total_usage.output_tokens += result.usage.output_tokens;
                cross_model = result.model.clone();
                let parsed = parse_idea_response(&result.text);
                let sliced: Vec<String> =
                    parsed.into_iter().take(profile.ideas_per_cross).collect();
                for text in sliced {
                    raw_ideas.push(RawIdea {
                        id: String::new(),
                        text,
                        close_slug: cross.close.slug.clone(),
                        far_slug: cross.far.slug.clone(),
                        distance_score: cross.far.distance_score,
                    });
                }
            }
            Err(e) => {
                eprintln!(
                    "[{}] WARN: cross [{}] × [{}] failed: {}",
                    profile.label, cross.close.slug, cross.far.slug, e
                );
                // Per-cross errors are warned + swallowed so one bad cross
                // doesn't void the rest of the run.
            }
        }
    }

    if raw_ideas.is_empty() {
        return Err(crate::Error::engine(format!(
            "{}: no ideas generated across {} crosses. Check API keys via `zbrain models doctor`.",
            profile.label,
            crosses.len()
        )));
    }

    // Assign stable ids ("01".."NN") before judging.
    assign_idea_ids(&mut raw_ideas);

    // ---- Phase 4: judge ----
    let mut judge_failed = false;
    let mut judged_by_id: std::collections::HashMap<String, JudgeIdeaResult> =
        std::collections::HashMap::new();
    let mut judge_usage = crate::ai::chat::ChatUsage::default();
    {
        let judge_input: Vec<JudgeIdea> = raw_ideas
            .iter()
            .map(|i| JudgeIdea {
                id: i.id.clone(),
                text: i.text.clone(),
                close_slug: i.close_slug.clone(),
                far_slug: i.far_slug.clone(),
            })
            .collect();
        let judge_opts = RunJudgeOptions {
            model_override: opts.judge_model.clone().or_else(|| opts.model_override.clone()),
            active_bias_tags: active_bias_tags.clone().unwrap_or_default(),
            max_ideas_per_call: opts.max_ideas_per_judge_call,
        };
        match run_judge(profile.judge_config, &judge_input, chat, &judge_opts).await {
            Ok(judge_result) => {
                for idea in &judge_result.ideas {
                    judged_by_id.insert(idea.id.clone(), idea.clone());
                }
                judge_usage = judge_result.usage;
            }
            Err(e) => {
                judge_failed = true;
                eprintln!(
                    "[{}] WARN: judge phase failed ({}); saving ideas unscored.",
                    profile.label, e
                );
            }
        }
    }

    // ---- Phase 5: assemble BrainstormResult ----
    let ideas: Vec<BrainstormIdea> = raw_ideas
        .into_iter()
        .map(|raw| {
            let j = judged_by_id.get(&raw.id).cloned();
            let passes = j.as_ref().map(|x| x.passes).unwrap_or(false);
            BrainstormIdea {
                id: raw.id,
                text: raw.text,
                close_slug: raw.close_slug,
                far_slug: raw.far_slug,
                distance_score: raw.distance_score,
                judge: j,
                passes,
                judge_failed,
            }
        })
        .collect();

    let total_in = total_usage.input_tokens + judge_usage.input_tokens;
    let total_out = total_usage.output_tokens + judge_usage.output_tokens;
    let actual = (total_in as f64 / 1_000_000.0) * DEFAULT_PRICING.0
        + (total_out as f64 / 1_000_000.0) * DEFAULT_PRICING.1;
    eprintln!(
        "[{}] actual cost: {} (estimated {}) — in={} out={} tokens",
        profile.label,
        fmt_usd(actual),
        fmt_usd(estimate),
        total_in,
        total_out
    );

    let close_slugs_all: Vec<String> = closes_for_cross.iter().map(|c| c.slug.clone()).collect();
    let far_slugs_all: Vec<String> = far_result.pages.iter().map(|p| p.slug.clone()).collect();
    let run_id = compute_run_id(&opts.question, profile.label, &close_slugs_all, &far_slugs_all);

    Ok(BrainstormResult {
        profile_label: profile.label.to_string(),
        question: opts.question.clone(),
        embedding_model: embedding_client.as_ref().map(|_| "embedding".to_string()),
        ideas,
        close_set: closes_for_cross
            .iter()
            .map(|c| CloseRefForReport {
                slug: c.slug.clone(),
                title: c.title.clone(),
            })
            .collect(),
        far_set: far_result
            .pages
            .iter()
            .map(|f| FarRefForReport {
                slug: f.slug.clone(),
                title: f.title.clone(),
                distance_score: f.distance_score,
                source: f.source.to_string(),
            })
            .collect(),
        active_bias_tags,
        short_of_target: far_result.short_of_target,
        judge_failed,
        cost: BrainstormCost {
            estimated_usd: estimate,
            actual_usd: actual,
            input_tokens: total_in,
            output_tokens: total_out,
        },
        run_id,
    })
}

struct RawIdea {
    id: String,
    text: String,
    close_slug: String,
    far_slug: String,
    distance_score: f64,
}

// ---------------------------------------------------------------------------
// Output formatter (D6 citation badges, D1 fold)
// ---------------------------------------------------------------------------

/// Render [`BrainstormResult`] as user-facing markdown. When `only_passed`
/// (default), filter to `passes == true` ideas.
#[must_use]
pub fn format_brainstorm_markdown(
    result: &BrainstormResult,
    opts: &FormatOpts,
) -> String {
    let only_passed = opts.only_passed;
    let include_meta = opts.include_meta;
    let ideas_to_show: Vec<&BrainstormIdea> = if only_passed {
        result.ideas.iter().filter(|i| i.passes).collect()
    } else {
        result.ideas.iter().collect()
    };

    let mut lines: Vec<String> = Vec::new();

    if include_meta {
        let heading = if result.profile_label == "lsd" {
            "LSD"
        } else {
            "Brainstorm"
        };
        lines.push(format!("# {}: {}", heading, result.question));
        lines.push(String::new());
        if result.judge_failed {
            lines.push(
                "> **Judge phase failed mid-run** — ideas below are unscored. Re-run to score."
                    .to_string(),
            );
            lines.push(String::new());
        }
        if result.short_of_target {
            lines.push("> _Note: domain bank was narrower than usual — see stderr warning._".to_string());
            lines.push(String::new());
        }
        if result.active_bias_tags.is_none() {
            lines.push(
                "> _Note: calibration cold-start — ideas were judged without anti-bias context._"
                    .to_string(),
            );
            lines.push(String::new());
        }
        lines.push(format!("**Close set** ({}):", result.close_set.len()));
        for c in &result.close_set {
            lines.push(format!(
                "- `{}`{}",
                c.slug,
                c.title.as_ref().map(|t| format!(" — {t}")).unwrap_or_default()
            ));
        }
        lines.push(String::new());
        let corpus_fallback = result
            .far_set
            .iter()
            .filter(|f| f.source == "corpus-sample")
            .count();
        lines.push(format!(
            "**Far set** ({} total, {} via corpus-sample fallback):",
            result.far_set.len(),
            corpus_fallback
        ));
        for f in &result.far_set {
            lines.push(format!(
                "- `{}` — distance {:.2}{}",
                f.slug,
                f.distance_score,
                f.title.as_ref().map(|t| format!(" — {t}")).unwrap_or_default()
            ));
        }
        lines.push(String::new());
    }

    let count_note = if only_passed && result.ideas.len() != ideas_to_show.len() {
        format!(" of {}", result.ideas.len())
    } else {
        String::new()
    };
    lines.push(format!(
        "## Ideas ({}{})",
        ideas_to_show.len(),
        count_note
    ));
    lines.push(String::new());
    for idea in &ideas_to_show {
        let score_suffix = match &idea.judge {
            Some(j) => format!(" _(score {:.2})_", j.weighted_score),
            None if idea.judge_failed => " _(unscored — judge failed)_".to_string(),
            None => String::new(),
        };
        lines.push(format!("### Idea {}{}", idea.id, score_suffix));
        lines.push(String::new());
        lines.push(idea.text.clone());
        lines.push(String::new());
        lines.push(format!(
            "_Citation: `{}` × `{}` — distance {:.2}_",
            idea.close_slug, idea.far_slug, idea.distance_score
        ));
        if let Some(j) = &idea.judge {
            if !j.note.is_empty() {
                lines.push(format!("_Judge note: {}_", j.note));
            }
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Formatting options for [`format_brainstorm_markdown`].
#[derive(Debug, Clone)]
pub struct FormatOpts {
    /// When true (default), filter to `passes == true` ideas.
    pub only_passed: bool,
    /// When true (default), include the close/far set header block.
    pub include_meta: bool,
}

impl Default for FormatOpts {
    fn default() -> Self {
        Self {
            only_passed: true,
            include_meta: true,
        }
    }
}

/// Frontmatter for a saved brainstorm/lsd page.
#[must_use]
pub fn build_brainstorm_frontmatter(result: &BrainstormResult, slug: &str) -> String {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let judge_failed = if result.judge_failed { "\njudge_failed: true" } else { "" };
    let unscored = if result.judge_failed { "\nunscored: true" } else { "" };
    let cold_start = result.active_bias_tags.is_none();
    let title_prefix = if result.profile_label == "lsd" { "LSD" } else { "Brainstorm" };
    let title = result.question.replace('"', "\\\"").chars().take(100).collect::<String>();
    let question = result.question.replace('"', "\\\"").chars().take(200).collect::<String>();
    let close_slugs = result
        .close_set
        .iter()
        .map(|c| format!("\"{}\"", c.slug))
        .collect::<Vec<_>>()
        .join(", ");
    let far_slugs = result
        .far_set
        .iter()
        .map(|f| format!("\"{}\"", f.slug))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "---\ntitle: \"{title_prefix}: {title}\"\nmode: {mode}\ngenerated_at: {now}\ndate: {date}\nquestion: \"{question}\"\nclose_slugs: [{close_slugs}]\nfar_slugs: [{far_slugs}]\nshort_of_target: {short}\ncalibration_cold_start: {cold}{judge_failed}{unscored}\ncost_usd: {cost:.4}\n---\n\n",
        title_prefix = title_prefix,
        title = title,
        mode = result.profile_label,
        now = now,
        date = date,
        question = question,
        close_slugs = close_slugs,
        far_slugs = far_slugs,
        short = result.short_of_target,
        cold = cold_start,
        judge_failed = judge_failed,
        unscored = unscored,
        cost = result.cost.actual_usd,
    )
}

// Assign stable ids after generation (called by the impl before judging).
pub(crate) fn assign_idea_ids(items: &mut [RawIdea]) {
    for (i, item) in items.iter_mut().enumerate() {
        item.id = format!("{:02}", i + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_cost_positive_and_scales() {
        let b = estimate_cost(&BRAINSTORM_PROFILE, "m");
        let l = estimate_cost(&LSD_PROFILE, "m");
        assert!(b > 0.0 && l > 0.0);
        assert!(l > b); // more crosses + ideas → higher cost.
    }

    #[test]
    fn parse_idea_response_header_split() {
        // Faithful to TS: header-split keeps the leading preamble chunk (if
        // non-empty) plus one entry per `## Idea N` header. The model usually
        // opens directly on `## Idea 1`, but when it emits a preamble we keep
        // it verbatim — matching the TS `keep all non-empty` contract.
        let text = "preamble\n## Idea 1\nfirst idea\n## Idea 2\nsecond idea";
        let parts = parse_idea_response(text);
        assert_eq!(parts.len(), 3);
        assert!(parts[1].contains("first"));
        assert!(parts[2].contains("second"));

        // When the model opens directly on the first header, no preamble is
        // captured — 2 ideas, no leading junk.
        let direct = "## Idea 1\nfirst idea\n## Idea 2\nsecond idea";
        let parts2 = parse_idea_response(direct);
        assert_eq!(parts2.len(), 2);
        assert!(parts2[0].contains("first"));
    }

    #[test]
    fn parse_idea_response_empty() {
        assert!(parse_idea_response("   ").is_empty());
    }

    #[test]
    fn extract_prefix_matches_ts_regex() {
        assert_eq!(extract_prefix("wiki/vc/intro"), Some("wiki/vc".to_string()));
        assert_eq!(extract_prefix("people/maria"), Some("people/maria".to_string()));
        assert_eq!(extract_prefix("alice"), None);
    }

    #[test]
    fn format_markdown_renders_passed_only() {
        let result = BrainstormResult {
            profile_label: "brainstorm".to_string(),
            question: "q".into(),
            embedding_model: None,
            ideas: vec![
                BrainstormIdea {
                    id: "01".into(),
                    text: "idea one".into(),
                    close_slug: "a".into(),
                    far_slug: "b".into(),
                    distance_score: 0.7,
                    judge: None,
                    passes: true,
                    judge_failed: false,
                },
                BrainstormIdea {
                    id: "02".into(),
                    text: "idea two".into(),
                    close_slug: "c".into(),
                    far_slug: "d".into(),
                    distance_score: 0.3,
                    judge: None,
                    passes: false,
                    judge_failed: false,
                },
            ],
            close_set: vec![CloseRefForReport { slug: "a".into(), title: None }],
            far_set: vec![FarRefForReport {
                slug: "b".into(),
                title: None,
                distance_score: 0.7,
                source: "prefix-stratified".to_string(),
            }],
            active_bias_tags: None,
            short_of_target: false,
            judge_failed: false,
            cost: BrainstormCost::default(),
            run_id: "deadbeef".into(),
        };
        let md = format_brainstorm_markdown(&result, &FormatOpts::default());
        assert!(md.contains("idea one"));
        assert!(!md.contains("idea two")); // filtered out (not passing)
    }
}
