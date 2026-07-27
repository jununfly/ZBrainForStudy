//! 1-3-3-6 — `gateVoice`: calibration-wave voice gate with a retry loop, an
//! injected judge, and a pure template fallback.
//!
//! Port of `src/core/calibration/voice-gate.ts` (v0.36.1.0, T6 / D24). Five
//! UX surfaces (pattern statement, nudge, forecast blurb, dashboard caption,
//! morning pulse) share ONE gate so voice rubric can't drift between surfaces.
//!
//! Design (locked via grill-me, 2026-07-27):
//! - `VoiceGenerator` / `VoiceJudge` are independent `async` traits, injected
//!   per call as `Arc<dyn Trait>` (never stored in `OperationContext`).
//! - The production judge (`LlmBackedVoiceJudge`) and a reusable generator
//!   (`ChatBackedVoiceGenerator`) wrap `Box<dyn ChatProvider>` obtained from
//!   `instantiate_chat` — matching the TS `defaultJudge` which called the AI
//!   gateway with `claude-haiku-4-5`.
//! - `parse_judge_output` is the faithful Rust port of the TS helper: robust to
//!   fence wrapping + leading prose; on unrecoverable parse it returns
//!   `academic` so the gate falls back to the template rather than silently
//!   passing bad voice.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};

/// UX surface driving the rubric tuning. Mirrors the TS `VoiceGateMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceGateMode {
    PatternStatement,
    Nudge,
    ForecastBlurb,
    DashboardCaption,
    MorningPulse,
}

impl VoiceGateMode {
    /// Stable string key, used for logging / telemetry.
    pub fn as_key(self) -> &'static str {
        match self {
            VoiceGateMode::PatternStatement => "pattern_statement",
            VoiceGateMode::Nudge => "nudge",
            VoiceGateMode::ForecastBlurb => "forecast_blurb",
            VoiceGateMode::DashboardCaption => "dashboard_caption",
            VoiceGateMode::MorningPulse => "morning_pulse",
        }
    }
}

/// Default rubric per surface. One gate, one rubric source — tuning happens
/// here, not in forked per-surface gate code.
pub fn default_rubric(mode: VoiceGateMode) -> &'static str {
    match mode {
        VoiceGateMode::PatternStatement => PATTERN_STATEMENT_RUBRIC,
        VoiceGateMode::Nudge => NUDGE_RUBRIC,
        VoiceGateMode::ForecastBlurb => FORECAST_BLURB_RUBRIC,
        VoiceGateMode::DashboardCaption => DASHBOARD_CAPTION_RUBRIC,
        VoiceGateMode::MorningPulse => MORNING_PULSE_RUBRIC,
    }
}

const PATTERN_STATEMENT_RUBRIC: &str = "Voice for a calibration pattern statement:
- Sounds like a smart friend recapping your record, not a doctor or HR.
- Uses second person (\"your\", \"you\").
- Names numbers grounded in actual takes (\"2 of 3 missed\"), not abstract
  metrics like \"Brier 0.31\" or \"conviction-bucket 0.8-0.9\".
- No preachy/clinical phrasing (\"our analysis indicates\", \"the data shows\").
- Short — under 25 words.
- NEVER mentions internal field names like 'Brier' or 'conviction-bucket'
  without translation.";

const NUDGE_RUBRIC: &str = "Voice for a real-time nudge fired during sync after a take is committed:
- Sounds like a friend tapping you on the shoulder, not an alert system.
- Second person, contractions allowed, casual.
- Grounded in 1-2 concrete past data points the user can verify.
- Always closes with a concrete next step (a CLI command or a question).
- Under 30 words.
- NEVER preachy. NEVER \"we recommend.\" NEVER \"according to your data\".";

const FORECAST_BLURB_RUBRIC: &str = "Voice for an inline forecast blurb on a new take:
- One short factual line, ~12-20 words.
- Names the past data in concrete terms (\"2 of 3 missed\" beats \"Brier 0.31\").
- Acknowledges uncertainty when n is small.
- No \"predicted Brier\" jargon without translation.
- NEVER condescending.";

const DASHBOARD_CAPTION_RUBRIC: &str = "Voice for a chart caption on the admin dashboard:
- Single short sentence per caption.
- Names ONE concrete fact.
- No marketing copy, no \"powerful insights\", no \"leverage\".
- Plain language, no jargon.";

const MORNING_PULSE_RUBRIC: &str = "Voice for a daily morning-pulse line:
- One sentence, sounds like a friend giving you a quick status check.
- Names the trend in plain words (\"improving\" beats \"trending positive\").
- Mentions ONE pattern when relevant; skip when no clear pattern.
- Under 25 words.
- NEVER clinical, NEVER preachy, NEVER hedged corporate language.";

/// Verdict the judge returns for a candidate string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceVerdict {
    Conversational,
    Academic,
}

/// Judge verdict: pass-through `Conversational`; reject with a short reason
/// for `Academic`.
#[derive(Debug, Clone)]
pub struct VoiceGateVerdict {
    pub verdict: VoiceVerdict,
    pub reason: String,
}

/// Error type for the injected voice traits. The gate never propagates these —
/// a trait error is treated as a failed attempt and the gate falls back to the
/// template (mirrors the TS gate, which never suppresses a surface silently).
#[derive(Debug, Clone)]
pub struct VoiceGateError(pub String);

impl std::fmt::Display for VoiceGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "voice_gate_error: {}", self.0)
    }
}

impl std::error::Error for VoiceGateError {}

/// Haiku judge prompt (ported from TS `HAIKU_GATE_PROMPT`). `{RUBRIC}` and
/// `{CANDIDATE}` are substituted before the call.
const HAIKU_GATE_PROMPT: &str = "You are the voice gate for a personal AI brain. A surface wants to show
this candidate text to the user. Decide whether it sounds conversational
(friend talking to friend) or academic (clinical / corporate).

Output ONLY a JSON object: {\"verdict\":\"conversational\"|\"academic\",\"reason\":\"<<=80 chars>\"}.

RUBRIC for this surface:
{RUBRIC}

CANDIDATE:
{CANDIDATE}";

/// Parse the judge's JSON output. Robust to fence wrapping + leading prose.
/// On unrecoverable parse failure, treat as `Academic` with
/// `reason='parse_failed'` so the gate falls back to the template rather than
/// silently passing bad voice. Faithful port of TS `parseJudgeOutput`.
pub fn parse_judge_output(raw: &str) -> VoiceGateVerdict {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return VoiceGateVerdict {
            verdict: VoiceVerdict::Academic,
            reason: "empty_judge_output".to_string(),
        };
    }
    // Strip a leading/trailing ``` fence if present.
    let body = if let Some(stripped) = trimmed
        .strip_prefix("```")
        .and_then(|s| s.strip_suffix("```"))
    {
        stripped.trim()
    } else {
        trimmed
    };
    let first_brace = match body.find('{') {
        Some(i) => i,
        None => {
            return VoiceGateVerdict {
                verdict: VoiceVerdict::Academic,
                reason: "parse_failed".to_string(),
            }
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&body[first_brace..]) {
        Ok(v) => v,
        Err(_) => {
            return VoiceGateVerdict {
                verdict: VoiceVerdict::Academic,
                reason: "parse_failed".to_string(),
            }
        }
    };
    let verdict = match parsed.get("verdict").and_then(|v| v.as_str()) {
        Some("conversational") => VoiceVerdict::Conversational,
        _ => VoiceVerdict::Academic,
    };
    let reason = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("no_reason")
        .chars()
        .take(80)
        .collect::<String>();
    VoiceGateVerdict { verdict, reason }
}

/// Produces ONE candidate string per call. The gate decides whether to accept
/// or regenerate; `feedback` carries the last judge reason to nudge away from
/// the rejected failure mode.
#[async_trait]
pub trait VoiceGenerator: Send + Sync {
    async fn generate(&self, attempt: u32, feedback: Option<String>) -> Result<String, VoiceGateError>;
}

/// Decides whether a candidate sounds conversational vs academic. `rubric` is
/// the surface-tuned rubric supplied by the gate.
#[async_trait]
pub trait VoiceJudge: Send + Sync {
    async fn judge(
        &self,
        candidate: &str,
        mode: VoiceGateMode,
        rubric: &str,
    ) -> Result<VoiceGateVerdict, VoiceGateError>;
}

/// Pure fallback producer. Receives the caller's slots and returns the final
/// string. Never iterates.
pub type VoiceGateTemplate<S> = Box<dyn Fn(&S) -> String + Send + Sync>;

/// Options for [`gate_voice`].
pub struct VoiceGateOpts<S> {
    /// UX surface — drives the rubric tuning.
    pub mode: VoiceGateMode,
    /// Producer of an LLM candidate per attempt.
    pub generator: Arc<dyn VoiceGenerator>,
    /// Judge. Required (production wiring passes `LlmBackedVoiceJudge`; tests
    /// pass a stub). Mirrors TS `opts.judge ?? defaultJudge` where `defaultJudge`
    /// is precisely the production-supplied judge.
    pub judge: Arc<dyn VoiceJudge>,
    /// Template fallback used when all generation attempts fail.
    pub template_fallback: (VoiceGateTemplate<S>, S),
    /// Max generation attempts before falling back. Default 2 (D11).
    pub max_attempts: u32,
    /// Override the rubric per mode (rarely needed).
    pub rubric: Option<String>,
}

/// Result of gating a single piece of voice.
pub struct VoiceGateResult<S> {
    /// The final text — the LLM output if a generation passed, or the template.
    pub text: String,
    /// Did a generation attempt pass the rubric?
    pub passed: bool,
    /// How many generation attempts ran before falling back. 0 means template-only.
    pub attempts: u32,
    /// Reason from the LAST judge call (the one that decided pass vs final reject).
    pub last_reason: Option<String>,
    /// Template slots used when `passed=false` (kept for audit).
    pub template_slots: Option<S>,
}

/// Gate a single piece of LLM-generated voice. Returns the final text + audit
/// info (pass/fail + attempts). Faithful port of TS `gateVoice`.
pub async fn gate_voice<S: Send + Sync>(opts: VoiceGateOpts<S>) -> VoiceGateResult<S> {
    let rubric = opts
        .rubric
        .clone()
        .unwrap_or_else(|| default_rubric(opts.mode).to_string());
    let max_attempts = opts.max_attempts.max(1);

    let mut last_reason: Option<String> = None;
    for attempt in 1..=max_attempts {
        let candidate = match opts.generator.generate(attempt, last_reason.clone()).await {
            Ok(c) if !c.trim().is_empty() => c,
            // Empty generation counts as a failed attempt; retry with feedback.
            Ok(_) => {
                last_reason = Some("empty_generation".to_string());
                continue;
            }
            // Generator threw — treat as a failed attempt but continue. If all
            // attempts throw we fall through to the template (D11 fallback).
            Err(e) => {
                last_reason = Some(format!("generator_error: {}", e));
                continue;
            }
        };

        match opts.judge.judge(&candidate, opts.mode, &rubric).await {
            Ok(v) if v.verdict == VoiceVerdict::Conversational => {
                return VoiceGateResult {
                    text: candidate,
                    passed: true,
                    attempts: attempt,
                    last_reason: Some(v.reason),
                    template_slots: None,
                };
            }
            Ok(v) => {
                last_reason = Some(v.reason);
            }
            Err(e) => {
                last_reason = Some(format!("judge_error: {}", e));
            }
        }
    }

    // Both attempts failed (or threw) — template fallback.
    let (produce, slots) = opts.template_fallback;
    let text = produce(&slots);
    VoiceGateResult {
        text,
        passed: false,
        attempts: max_attempts,
        last_reason,
        template_slots: Some(slots),
    }
}

/// Production judge backed by a `ChatProvider` (obtained from `instantiate_chat`).
/// Uses the Haiku gate prompt + `parse_judge_output`. If the chat call errors
/// or returns unparseable text, it returns `Academic` (reuse parse semantics)
/// so the gate falls back to the template rather than silently passing.
pub struct LlmBackedVoiceJudge {
    provider: Arc<dyn ChatProvider>,
    model_id: String,
}

impl LlmBackedVoiceJudge {
    /// Wrap an already-instantiated chat provider (e.g. from `instantiate_chat`)
    /// and the model id to use for the judge call.
    pub fn new(provider: Arc<dyn ChatProvider>, model_id: impl Into<String>) -> Self {
        Self {
            provider,
            model_id: model_id.into(),
        }
    }
}

#[async_trait]
impl VoiceJudge for LlmBackedVoiceJudge {
    async fn judge(
        &self,
        candidate: &str,
        mode: VoiceGateMode,
        rubric: &str,
    ) -> Result<VoiceGateVerdict, VoiceGateError> {
        let prompt = HAIKU_GATE_PROMPT
            .replace("{RUBRIC}", rubric)
            .replace("{CANDIDATE}", candidate);
        let result = self
            .provider
            .chat(ChatOpts {
                model: Some(self.model_id.clone()),
                system: None,
                messages: vec![ChatMessage::text(ChatRole::User, prompt)],
                tools: vec![],
                max_tokens: Some(100),
                cache_system: false,
            })
            .await;
        match result {
            Ok(r) => Ok(parse_judge_output(&r.text)),
            Err(e) => Ok(VoiceGateVerdict {
                verdict: VoiceVerdict::Academic,
                reason: format!("chat_error: {}", e),
            }),
        }
    }
}

/// Reusable production generator backed by a `ChatProvider` (from
/// `instantiate_chat`). The caller supplies a `build_prompt` closure mapping
/// `(attempt, feedback)` to `(system, user)` so per-surface prompts stay with
/// the surface, not in this core helper.
pub struct ChatBackedVoiceGenerator {
    provider: Arc<dyn ChatProvider>,
    model_id: String,
    build_prompt: Box<dyn Fn(u32, Option<&str>) -> (Option<String>, String) + Send + Sync>,
}

impl ChatBackedVoiceGenerator {
    pub fn new<F>(provider: Arc<dyn ChatProvider>, model_id: impl Into<String>, build_prompt: F) -> Self
    where
        F: Fn(u32, Option<&str>) -> (Option<String>, String) + Send + Sync + 'static,
    {
        Self {
            provider,
            model_id: model_id.into(),
            build_prompt: Box::new(build_prompt),
        }
    }
}

#[async_trait]
impl VoiceGenerator for ChatBackedVoiceGenerator {
    async fn generate(&self, attempt: u32, feedback: Option<String>) -> Result<String, VoiceGateError> {
        let (system, user) = (self.build_prompt)(attempt, feedback.as_deref());
        let result = self
            .provider
            .chat(ChatOpts {
                model: Some(self.model_id.clone()),
                system,
                messages: vec![ChatMessage::text(ChatRole::User, user)],
                tools: vec![],
                max_tokens: Some(400),
                cache_system: false,
            })
            .await
            .map_err(|e| VoiceGateError(format!("chat_error: {}", e)))?;
        Ok(result.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubJudge {
        verdict: VoiceVerdict,
    }

    #[async_trait]
    impl VoiceJudge for StubJudge {
        async fn judge(
            &self,
            _candidate: &str,
            _mode: VoiceGateMode,
            _rubric: &str,
        ) -> Result<VoiceGateVerdict, VoiceGateError> {
            Ok(VoiceGateVerdict {
                verdict: self.verdict.clone(),
                reason: "stub".to_string(),
            })
        }
    }

    /// Generator returning `candidates[i]` on attempt i+1, then erroring.
    struct SeqGenerator {
        candidates: Vec<String>,
    }

    #[async_trait]
    impl VoiceGenerator for SeqGenerator {
        async fn generate(
            &self,
            attempt: u32,
            _feedback: Option<String>,
        ) -> Result<String, VoiceGateError> {
            let idx = (attempt as usize).saturating_sub(1);
            if idx < self.candidates.len() {
                Ok(self.candidates[idx].clone())
            } else {
                Err(VoiceGateError("exhausted".to_string()))
            }
        }
    }

    struct ErrGenerator;

    #[async_trait]
    impl VoiceGenerator for ErrGenerator {
        async fn generate(
            &self,
            _attempt: u32,
            _feedback: Option<String>,
        ) -> Result<String, VoiceGateError> {
            Err(VoiceGateError("boom".to_string()))
        }
    }

    fn template_fallback() -> (VoiceGateTemplate<()>, ()) {
        (Box::new(|_| "TEMPLATE FALLBACK".to_string()), ())
    }

    #[tokio::test]
    async fn passes_on_first_attempt() {
        let opts = VoiceGateOpts {
            mode: VoiceGateMode::Nudge,
            generator: Arc::new(SeqGenerator {
                candidates: vec!["hey you missed two of three".to_string()],
            }),
            judge: Arc::new(StubJudge {
                verdict: VoiceVerdict::Conversational,
            }),
            template_fallback: template_fallback(),
            max_attempts: 2,
            rubric: None,
        };
        let r = gate_voice(opts).await;
        assert!(r.passed);
        assert_eq!(r.attempts, 1);
        assert_eq!(r.text, "hey you missed two of three");
        assert!(r.template_slots.is_none());
    }

    #[tokio::test]
    async fn falls_back_to_template_after_rejections() {
        let opts = VoiceGateOpts {
            mode: VoiceGateMode::Nudge,
            generator: Arc::new(SeqGenerator {
                candidates: vec!["clinical analysis indicates".to_string(), "academic again".to_string()],
            }),
            judge: Arc::new(StubJudge {
                verdict: VoiceVerdict::Academic,
            }),
            template_fallback: template_fallback(),
            max_attempts: 2,
            rubric: None,
        };
        let r = gate_voice(opts).await;
        assert!(!r.passed);
        assert_eq!(r.attempts, 2);
        assert_eq!(r.text, "TEMPLATE FALLBACK");
        assert!(r.template_slots.is_some());
    }

    #[tokio::test]
    async fn single_attempt_then_fallback() {
        let opts = VoiceGateOpts {
            mode: VoiceGateMode::Nudge,
            generator: Arc::new(SeqGenerator {
                candidates: vec!["academic".to_string()],
            }),
            judge: Arc::new(StubJudge {
                verdict: VoiceVerdict::Academic,
            }),
            template_fallback: template_fallback(),
            max_attempts: 1,
            rubric: None,
        };
        let r = gate_voice(opts).await;
        assert!(!r.passed);
        assert_eq!(r.attempts, 1);
        assert_eq!(r.text, "TEMPLATE FALLBACK");
    }

    #[tokio::test]
    async fn empty_generation_counts_as_failed_attempt() {
        let opts = VoiceGateOpts {
            mode: VoiceGateMode::Nudge,
            generator: Arc::new(SeqGenerator {
                candidates: vec!["".to_string(), "ok friend".to_string()],
            }),
            judge: Arc::new(StubJudge {
                verdict: VoiceVerdict::Conversational,
            }),
            template_fallback: template_fallback(),
            max_attempts: 2,
            rubric: None,
        };
        let r = gate_voice(opts).await;
        // Empty first attempt is skipped; second (non-empty) passes.
        assert!(r.passed);
        assert_eq!(r.attempts, 2);
    }

    #[tokio::test]
    async fn generator_error_falls_back() {
        let opts = VoiceGateOpts {
            mode: VoiceGateMode::Nudge,
            generator: Arc::new(ErrGenerator),
            judge: Arc::new(StubJudge {
                verdict: VoiceVerdict::Conversational,
            }),
            template_fallback: template_fallback(),
            max_attempts: 2,
            rubric: None,
        };
        let r = gate_voice(opts).await;
        assert!(!r.passed);
        assert_eq!(r.attempts, 2);
        assert_eq!(r.text, "TEMPLATE FALLBACK");
    }

    #[test]
    fn parse_judge_output_variants() {
        assert_eq!(
            parse_judge_output("{\"verdict\":\"conversational\",\"reason\":\"sounds like a friend\"}").verdict,
            VoiceVerdict::Conversational
        );
        // Fenced.
        assert_eq!(
            parse_judge_output("```json\n{\"verdict\":\"academic\",\"reason\":\"clinical\"}\n```").verdict,
            VoiceVerdict::Academic
        );
        // Leading prose before the JSON object.
        assert_eq!(
            parse_judge_output("here:\n{\"verdict\":\"conversational\"}").verdict,
            VoiceVerdict::Conversational
        );
        // Empty + garbage -> academic (never silently pass).
        assert_eq!(parse_judge_output("").verdict, VoiceVerdict::Academic);
        assert_eq!(parse_judge_output("not json at all").verdict, VoiceVerdict::Academic);
        // Unknown verdict value -> academic.
        assert_eq!(
            parse_judge_output("{\"verdict\":\"weird\"}").verdict,
            VoiceVerdict::Academic
        );
    }
}
