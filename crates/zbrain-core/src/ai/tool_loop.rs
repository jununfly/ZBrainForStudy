//! Provider-agnostic tool-calling loop.
//!
//! Phase 8 slice 5 (Part6). Ports `toolLoop()` from `src/core/ai/gateway.ts`
//! (`gateway.ts:2573-2764`, the "v0.38 — D11 + D6/D7 gateway-native subagent
//! path" loop) plus its option/result/handler types (`gateway.ts:2469-2555`).
//!
//! ## What this is
//!
//! A stateless loop wrapping [`ChatProvider::chat`] with:
//!   - the assistant -> tool-dispatch -> tool-result cycle,
//!   - zbrain-stable tool-use IDs (D11) claimed at first observation,
//!   - the write-ordering invariant (persist BEFORE the side effect so a
//!     crash mid-execute is reconcilable on the next replay),
//!   - crash-replay reconciliation keyed by `zbrain_tool_use_id`,
//!   - Anthropic-only `cache_control` passthrough (via [`ChatOpts::cache_system`]).
//!
//! ## Design decisions (slice 5 grill)
//!
//! - **Provider is injected** as `&dyn ChatProvider` (not resolved from the
//!   model string inside the loop). The caller resolves once; the loop stays a
//!   pure deep module testable with [`MockChatProvider`]. Provider resolution
//!   (the `provider:model` -> concrete impl factory) is slice 4's job.
//! - **Persistence is a trait**, [`ToolLoopHooks`], not five loose optional
//!   closures (async closures are painful in Rust). Phase 9 (Minions) impls it
//!   with DB writes; slice-5 tests use [`NoopHooks`] or a recording mock. This
//!   preserves the write-ordering invariant because the loop `await`s
//!   [`ToolLoopHooks::on_tool_call_start`] to obtain the [`ZbrainToolUseId`]
//!   before executing the side effect.
//! - **Single cut**: loop control + `ToolHandler` dispatch + crash-replay all
//!   land here. Replay is structurally interleaved with dispatch (the
//!   short-circuit checks live inside the per-call loop, before `execute`), so
//!   splitting it out would double the dispatch code.
//!
//! ## Out of scope
//!
//! DB persistence itself (Phase 9 subagent handler impls [`ToolLoopHooks`]),
//! and the `provider:model` -> provider factory (slice 4).

use std::collections::HashMap;

use async_trait::async_trait;

use crate::ai::chat::{
    ChatBlock, ChatContent, ChatError, ChatMessage, ChatOpts, ChatProvider, ChatResult, ChatRole,
    ChatToolDef, ChatUsage, StopReason,
};

/// Zbrain-owned stable identifier for a single tool execution (D11). Minted by
/// the caller in [`ToolLoopHooks::on_tool_call_start`] (a uuid v7 in Phase 9);
/// the loop keys crash-replay reconciliation on this, NOT the provider-supplied
/// tool-call id. Mirrors the TS `zbrainToolUseId` string.
pub type ZbrainToolUseId = String;

/// Outcome of a prior tool execution carried in from a crashed run. Mirrors the
/// TS `ToolLoopReplayState.priorTools` value shape (`gateway.ts:2484`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorToolStatus {
    /// Persisted pending but never settled (crashed mid-execute).
    Pending,
    /// Completed successfully; `output` is the recorded result.
    Complete { output: serde_json::Value },
    /// Failed; `error` is the recorded message.
    Failed { error: String },
}

/// State carried in from a prior crashed run. The reconciler keys by
/// zbrain-owned [`ZbrainToolUseId`] (D11), NOT provider-supplied IDs.
/// Mirrors the TS `ToolLoopReplayState` (`gateway.ts:2482-2487`).
#[derive(Debug, Clone, Default)]
pub struct ToolLoopReplayState {
    /// Chat history up to the assistant's last turn (empty on a fresh run).
    pub prior_messages: Vec<ChatMessage>,
    /// `zbrain_tool_use_id` -> recorded outcome. The Phase 9 D5 read-time shim
    /// synthesizes ids for legacy rows so this map sees both shapes uniformly.
    pub prior_tools: HashMap<ZbrainToolUseId, PriorToolStatus>,
    /// Turn index to resume from.
    pub next_turn_idx: u32,
    /// Message index to resume from (so the first persisted assistant turn does
    /// not collide with a seed user message at idx 0).
    pub next_message_idx: u32,
}

/// A single tool invocation. `idempotent` lets the loop safely re-execute a
/// pending row on crash-replay; a non-idempotent tool that crashed mid-execute
/// is surfaced as [`ToolLoopStopReason::Unrecoverable`]. Mirrors the TS
/// `ToolHandler` (`gateway.ts:2469-2472`).
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Whether this tool is safe to re-run after a crash-mid-execute.
    fn idempotent(&self) -> bool {
        false
    }
    /// Run the tool. `input` is the model-supplied JSON arguments.
    async fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value, String>;
}

/// Why the loop stopped. Mirrors the TS `ToolLoopStopReason`
/// (`gateway.ts:2546`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolLoopStopReason {
    /// Assistant produced a final answer with no tool calls.
    End,
    /// Hit the `max_turns` cap.
    MaxTurns,
    /// Model refused.
    Refusal,
    /// Provider content filter tripped.
    ContentFilter,
    /// `abort` flag observed between steps.
    Aborted,
    /// Non-idempotent tool pending on resume — cannot safely re-run.
    Unrecoverable,
}

/// Result of a completed loop. Mirrors the TS `ToolLoopResult`
/// (`gateway.ts:2548-2555`).
#[derive(Debug, Clone)]
pub struct ToolLoopResult {
    /// Final assistant text (the answer, or the refusal/filter message).
    pub final_text: String,
    /// Turns executed.
    pub total_turns: u32,
    /// Summed usage across every turn.
    pub total_usage: ChatUsage,
    pub stop_reason: ToolLoopStopReason,
    /// Full message array including every assistant + tool-result turn. The
    /// caller persists it if desired.
    pub messages: Vec<ChatMessage>,
}

/// Options for one [`tool_loop`] run. Mirrors the TS `ToolLoopOpts`
/// (`gateway.ts:2489-2544`), minus the five persistence callbacks (now on
/// [`ToolLoopHooks`]) and `abortSignal` (replaced by the `abort` closure).
pub struct ToolLoopOpts {
    /// `provider:modelId`. `None` lets the provider pick its default.
    pub model: Option<String>,
    /// System prompt (provider-neutral). Cached when supported + `cache_system`.
    pub system: Option<String>,
    /// Initial user message(s). Prepended only when `replay_state` is `None` or
    /// its `prior_messages` is empty. Mirrors TS `initialMessages`.
    pub initial_messages: Vec<ChatMessage>,
    /// Tool definitions (provider-neutral JSON Schema).
    pub tools: Vec<ChatToolDef>,
    /// Hard cap on loop iterations. Default 20 (matches TS).
    pub max_turns: u32,
    /// Per-turn max output tokens. Default 4096 (matches TS).
    pub max_tokens: u32,
    /// Apply Anthropic `cache_control` to system + last tool. Ignored elsewhere.
    pub cache_system: bool,
    /// Crash-replay state. When set, the loop resumes from the recorded position.
    pub replay_state: Option<ToolLoopReplayState>,
}

impl Default for ToolLoopOpts {
    fn default() -> Self {
        Self {
            model: None,
            system: None,
            initial_messages: Vec::new(),
            tools: Vec::new(),
            max_turns: 20,
            max_tokens: 4096,
            cache_system: false,
            replay_state: None,
        }
    }
}

// ---- Persistence hooks ----

/// Write-ordering + D11 persistence seam. The loop fires these BEFORE side
/// effects so a crash mid-execute is reconcilable on the next replay. Mirrors
/// the five optional TS callbacks (`gateway.ts:2515-2543`), collapsed into one
/// trait so Phase 9 (Minions) has a single interface to implement and slice-5
/// tests can use [`NoopHooks`].
///
/// Ordering per turn (matches the TS invariant):
///   1. `on_assistant_turn`   — assistant message persisted (D11 step 1)
///   2. `on_tool_call_start`  — pending row persisted, returns id (D11 step 2)
///   3. `ToolHandler::execute` — side effect
///   4. `on_tool_call_complete` / `on_tool_call_failed` (D11 step 4)
#[async_trait]
pub trait ToolLoopHooks: Send + Sync {
    /// Assistant turn persisted before any tool dispatch (D11 step 1).
    async fn on_assistant_turn(
        &self,
        _turn_idx: u32,
        _message_idx: u32,
        _blocks: &[ChatBlock],
        _usage: &ChatUsage,
        _model: &str,
    ) {
    }

    /// Persist a pending tool execution and return its zbrain-owned id. The
    /// caller assigns ordinal + uuid v7 and returns it so the loop can key
    /// replay by [`ZbrainToolUseId`]. `provider_tool_call_id` is the
    /// provider-supplied id, kept as a debug-only side channel.
    ///
    /// The default impl returns the TS fallback id `inline-{turn}-{ordinal}`
    /// so [`NoopHooks`] keeps replay keying identical without a DB.
    async fn on_tool_call_start(
        &self,
        turn_idx: u32,
        _message_idx: u32,
        ordinal: u32,
        _tool_name: &str,
        _input: &serde_json::Value,
        _provider_tool_call_id: &str,
    ) -> ZbrainToolUseId {
        format!("inline-{turn_idx}-{ordinal}")
    }

    /// Settle a tool execution as complete (D11 step 4).
    async fn on_tool_call_complete(&self, _id: &ZbrainToolUseId, _output: &serde_json::Value) {}

    /// Settle a tool execution as failed (D11 step 4).
    async fn on_tool_call_failed(&self, _id: &ZbrainToolUseId, _error: &str) {}

    /// Optional observability heartbeat. Sync (matches the TS `void` callback).
    fn on_heartbeat(&self, _event: &str, _data: &serde_json::Value) {}
}

/// No-op hooks: every method uses the trait default. Used by slice-5 tests and
/// any caller that does not need persistence.
#[derive(Debug, Default)]
pub struct NoopHooks;

impl ToolLoopHooks for NoopHooks {}

// ---- The loop ----

/// Extract the `(tool_call_id, tool_name, input)` tuples from an assistant
/// turn's blocks, in order. Pure helper mirroring the TS
/// `chatResult.blocks.filter(b => b.type === 'tool-call')` (`gateway.ts:2645`).
fn tool_calls_of(blocks: &[ChatBlock]) -> Vec<(String, String, serde_json::Value)> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ChatBlock::ToolCall { tool_call_id, tool_name, input } => {
                Some((tool_call_id.clone(), tool_name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Provider-agnostic tool-calling loop. See the module docs for the design.
///
/// `provider` is injected (slice-5 decision A); the loop never resolves it from
/// a model string. `handlers` maps tool name -> implementation. `hooks` carries
/// persistence + observability; pass [`NoopHooks`] when none is needed. An
/// `abort` closure is polled between steps (Rust stand-in for the TS
/// `AbortSignal`); return `true` to stop with [`ToolLoopStopReason::Aborted`].
///
/// # Errors
///
/// Propagates the first [`ChatError`] from [`ChatProvider::chat`] (the TS loop
/// rethrows LLM-call failures). A non-idempotent tool found pending on resume
/// returns `Ok` with [`ToolLoopStopReason::Unrecoverable`] — it is a loop
/// outcome, not a transport error.
#[allow(clippy::too_many_lines)] // Faithful port of one cohesive TS loop; extracting helpers would split the write-ordering invariant (persist->execute->settle) across functions and obscure it.
pub async fn tool_loop(
    provider: &dyn ChatProvider,
    opts: ToolLoopOpts,
    handlers: &HashMap<String, Box<dyn ToolHandler>>,
    hooks: &dyn ToolLoopHooks,
    abort: &(dyn Fn() -> bool + Send + Sync),
) -> Result<ToolLoopResult, ChatError> {
    let mut total_usage = ChatUsage::default();

    // Seed messages: prior history (replay) or initial. Mirrors gateway.ts:2585.
    let (mut messages, mut turn_idx, mut message_idx) = match &opts.replay_state {
        Some(rs) => {
            let mut msgs = rs.prior_messages.clone();
            if msgs.is_empty() {
                msgs.extend(opts.initial_messages.iter().cloned());
            }
            (msgs, rs.next_turn_idx, rs.next_message_idx)
        }
        None => (opts.initial_messages.clone(), 0, 0),
    };

    let mut final_text = String::new();
    let mut stop_reason = ToolLoopStopReason::End;

    while turn_idx < opts.max_turns {
        if abort() {
            stop_reason = ToolLoopStopReason::Aborted;
            break;
        }

        hooks.on_heartbeat("turn_start", &serde_json::json!({ "turn_idx": turn_idx }));

        // One chat completion turn. Failures propagate (TS rethrows).
        let result: ChatResult = match provider
            .chat(ChatOpts {
                model: opts.model.clone(),
                system: opts.system.clone(),
                messages: messages.clone(),
                tools: opts.tools.clone(),
                max_tokens: Some(opts.max_tokens),
                cache_system: opts.cache_system,
            })
            .await
        {
            Ok(r) => r,
            Err(err) => {
                hooks.on_heartbeat(
                    "llm_call_failed",
                    &serde_json::json!({ "turn_idx": turn_idx, "error": err.to_string() }),
                );
                return Err(err);
            }
        };

        total_usage.input_tokens += result.usage.input_tokens;
        total_usage.output_tokens += result.usage.output_tokens;
        total_usage.cache_read_tokens += result.usage.cache_read_tokens;
        total_usage.cache_creation_tokens += result.usage.cache_creation_tokens;

        // D11 step 1: persist assistant turn BEFORE any tool dispatch.
        let assistant_message_idx = message_idx;
        message_idx += 1;
        hooks
            .on_assistant_turn(
                turn_idx,
                assistant_message_idx,
                &result.blocks,
                &result.usage,
                &result.model,
            )
            .await;
        messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: ChatContent::Blocks(result.blocks.clone()),
        });

        // Stop-reason check BEFORE tool dispatch; only tool_calls continue.
        match result.stop_reason {
            StopReason::Refusal => {
                stop_reason = ToolLoopStopReason::Refusal;
                final_text = result.text;
                break;
            }
            StopReason::ContentFilter => {
                stop_reason = ToolLoopStopReason::ContentFilter;
                final_text = result.text;
                break;
            }
            _ => {}
        }

        let calls = tool_calls_of(&result.blocks);
        if calls.is_empty() {
            stop_reason = ToolLoopStopReason::End;
            final_text = result.text;
            break;
        }

        // D11 + write-ordering: persist pending -> execute -> settle.
        let mut tool_result_blocks: Vec<ChatBlock> = Vec::new();
        let mut aborted_mid_dispatch = false;
        let mut unrecoverable = false;

        for (ordinal, (tool_call_id, tool_name, input)) in calls.into_iter().enumerate() {
            let ordinal = u32::try_from(ordinal).unwrap_or(u32::MAX);
            if abort() {
                aborted_mid_dispatch = true;
                break;
            }

            let Some(handler) = handlers.get(&tool_name) else {
                // Tool not registered: synthesize an error result, don't persist.
                tool_result_blocks.push(ChatBlock::ToolResult {
                    tool_call_id,
                    tool_name: tool_name.clone(),
                    output: serde_json::Value::String(format!(
                        "tool \"{tool_name}\" is not in the registry for this subagent"
                    )),
                    is_error: true,
                });
                hooks.on_heartbeat(
                    "tool_failed",
                    &serde_json::json!({ "turn_idx": turn_idx, "tool_name": tool_name, "error": "not_registered" }),
                );
                continue;
            };

            // Step 2: persist pending + claim zbrain_tool_use_id.
            let zid = hooks
                .on_tool_call_start(
                    turn_idx,
                    assistant_message_idx,
                    ordinal,
                    &tool_name,
                    &input,
                    &tool_call_id,
                )
                .await;

            // Replay short-circuit: prior outcome wins; idempotent re-exec ok.
            if let Some(prior) = opts.replay_state.as_ref().and_then(|rs| rs.prior_tools.get(&zid)) {
                match prior {
                    PriorToolStatus::Complete { output } => {
                        tool_result_blocks.push(ChatBlock::ToolResult {
                            tool_call_id,
                            tool_name: tool_name.clone(),
                            output: output.clone(),
                            is_error: false,
                        });
                        hooks.on_heartbeat(
                            "tool_replay_complete",
                            &serde_json::json!({ "turn_idx": turn_idx, "tool_name": tool_name }),
                        );
                        continue;
                    }
                    PriorToolStatus::Failed { error } => {
                        tool_result_blocks.push(ChatBlock::ToolResult {
                            tool_call_id,
                            tool_name: tool_name.clone(),
                            output: serde_json::Value::String(error.clone()),
                            is_error: true,
                        });
                        hooks.on_heartbeat(
                            "tool_replay_failed",
                            &serde_json::json!({ "turn_idx": turn_idx, "tool_name": tool_name }),
                        );
                        continue;
                    }
                    PriorToolStatus::Pending if !handler.idempotent() => {
                        // Non-idempotent crash-mid-execute: unrecoverable.
                        unrecoverable = true;
                        break;
                    }
                    PriorToolStatus::Pending => { /* idempotent: fall through to re-execute */ }
                }
            }

            // Step 3: execute (side effect).
            hooks.on_heartbeat(
                "tool_called",
                &serde_json::json!({ "turn_idx": turn_idx, "tool_name": tool_name }),
            );
            match handler.execute(input).await {
                Ok(output) => {
                    // Step 4: settle complete.
                    hooks.on_tool_call_complete(&zid, &output).await;
                    tool_result_blocks.push(ChatBlock::ToolResult {
                        tool_call_id,
                        tool_name: tool_name.clone(),
                        output,
                        is_error: false,
                    });
                    hooks.on_heartbeat(
                        "tool_result",
                        &serde_json::json!({ "turn_idx": turn_idx, "tool_name": tool_name }),
                    );
                }
                Err(err_msg) => {
                    hooks.on_tool_call_failed(&zid, &err_msg).await;
                    tool_result_blocks.push(ChatBlock::ToolResult {
                        tool_call_id,
                        tool_name: tool_name.clone(),
                        output: serde_json::Value::String(err_msg.clone()),
                        is_error: true,
                    });
                    hooks.on_heartbeat(
                        "tool_failed",
                        &serde_json::json!({ "turn_idx": turn_idx, "tool_name": tool_name, "error": err_msg }),
                    );
                }
            }
        }

        if unrecoverable {
            // Surface as a typed loop outcome (the TS loop throws here instead).
            stop_reason = ToolLoopStopReason::Unrecoverable;
            break;
        }
        if aborted_mid_dispatch {
            stop_reason = ToolLoopStopReason::Aborted;
            break;
        }

        // Feed all tool results back as a single user message.
        message_idx += 1;
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Blocks(tool_result_blocks),
        });

        turn_idx += 1;
    }

    if turn_idx >= opts.max_turns && stop_reason == ToolLoopStopReason::End {
        stop_reason = ToolLoopStopReason::MaxTurns;
    }

    Ok(ToolLoopResult {
        final_text,
        total_turns: turn_idx,
        total_usage,
        stop_reason,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatBlock, ChatResult, ChatUsage, MockChatProvider, StopReason};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn no_abort() -> Box<dyn Fn() -> bool + Send + Sync> {
        Box::new(|| false)
    }

    fn tool_call_result(
        tool_call_id: &str,
        tool_name: &str,
        input: serde_json::Value,
    ) -> ChatResult {
        ChatResult {
            text: String::new(),
            blocks: vec![ChatBlock::ToolCall {
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                input,
            }],
            stop_reason: StopReason::ToolCalls,
            usage: ChatUsage { input_tokens: 5, output_tokens: 7, ..Default::default() },
            model: "mock:mock-model".to_string(),
            provider_id: "mock".to_string(),
            provider_metadata: None,
        }
    }

    /// A handler that records its call count and returns a canned output.
    struct RecordingHandler {
        idempotent: bool,
        calls: Arc<AtomicUsize>,
        output: serde_json::Value,
        fail: bool,
    }

    #[async_trait]
    impl ToolHandler for RecordingHandler {
        fn idempotent(&self) -> bool {
            self.idempotent
        }
        async fn execute(&self, _input: serde_json::Value) -> Result<serde_json::Value, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err("boom".to_string())
            } else {
                Ok(self.output.clone())
            }
        }
    }

    fn handlers_with(name: &str, h: RecordingHandler) -> HashMap<String, Box<dyn ToolHandler>> {
        let mut m: HashMap<String, Box<dyn ToolHandler>> = HashMap::new();
        m.insert(name.to_string(), Box::new(h));
        m
    }

    // ---- Loop control ----

    #[tokio::test]
    async fn no_tool_calls_ends_immediately() {
        let provider = MockChatProvider::new("final answer");
        let handlers: HashMap<String, Box<dyn ToolHandler>> = HashMap::new();
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "hi")],
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::End);
        assert_eq!(r.final_text, "final answer");
        assert_eq!(r.total_turns, 0);
        // 1 initial user + 1 assistant turn persisted into messages.
        assert_eq!(r.messages.len(), 2);
    }

    #[tokio::test]
    async fn one_tool_call_then_final() {
        let provider = MockChatProvider::new("done");
        provider.queue_result(tool_call_result("tc1", "echo", serde_json::json!({"x": 1})));
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler {
                idempotent: false,
                calls: calls.clone(),
                output: serde_json::json!({"ok": true}),
                fail: false,
            },
        );
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::End);
        assert_eq!(r.final_text, "done");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "tool executed exactly once");
        assert_eq!(r.total_turns, 1, "one tool-dispatch turn then final");
        // usage summed: turn0 tool_call(5/7) + turn1 default(0/0).
        assert_eq!(r.total_usage.input_tokens, 5);
        assert_eq!(r.total_usage.output_tokens, 7);
    }

    #[tokio::test]
    async fn max_turns_cap() {
        let provider = MockChatProvider::new("never reached");
        for _ in 0..5 {
            provider.queue_result(tool_call_result("tc", "echo", serde_json::json!({})));
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler { idempotent: true, calls, output: serde_json::json!(1), fail: false },
        );
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            max_turns: 3,
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::MaxTurns);
        assert_eq!(r.total_turns, 3);
    }

    #[tokio::test]
    async fn refusal_stops_before_dispatch() {
        let provider = MockChatProvider::new("x");
        provider.queue_result(ChatResult {
            text: "no".to_string(),
            blocks: vec![ChatBlock::Text { text: "no".to_string() }],
            stop_reason: StopReason::Refusal,
            usage: ChatUsage::default(),
            model: "mock:m".to_string(),
            provider_id: "mock".to_string(),
            provider_metadata: None,
        });
        let handlers: HashMap<String, Box<dyn ToolHandler>> = HashMap::new();
        let r = tool_loop(&provider, ToolLoopOpts::default(), &handlers, &NoopHooks, &no_abort())
            .await
            .unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::Refusal);
        assert_eq!(r.final_text, "no");
    }

    #[tokio::test]
    async fn content_filter_stops_before_dispatch() {
        let provider = MockChatProvider::new("x");
        provider.queue_result(ChatResult {
            text: "blocked".to_string(),
            blocks: vec![ChatBlock::Text { text: "blocked".to_string() }],
            stop_reason: StopReason::ContentFilter,
            usage: ChatUsage::default(),
            model: "mock:m".to_string(),
            provider_id: "mock".to_string(),
            provider_metadata: None,
        });
        let handlers: HashMap<String, Box<dyn ToolHandler>> = HashMap::new();
        let r = tool_loop(&provider, ToolLoopOpts::default(), &handlers, &NoopHooks, &no_abort())
            .await
            .unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::ContentFilter);
        assert_eq!(r.final_text, "blocked");
    }

    #[tokio::test]
    async fn chat_error_propagates() {
        let provider = MockChatProvider::new("x");
        provider.queue_error(ChatError::Transient { message: "429".to_string() });
        let handlers: HashMap<String, Box<dyn ToolHandler>> = HashMap::new();
        let err = tool_loop(&provider, ToolLoopOpts::default(), &handlers, &NoopHooks, &no_abort())
            .await
            .unwrap_err();
        assert!(err.is_transient());
    }

    // ---- Tool dispatch ----

    #[tokio::test]
    async fn unregistered_tool_yields_error_result() {
        let provider = MockChatProvider::new("done");
        provider.queue_result(tool_call_result("tc", "missing_tool", serde_json::json!({})));
        let handlers: HashMap<String, Box<dyn ToolHandler>> = HashMap::new();
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::End);
        let last_user = r.messages.iter().rev().find(|m| m.role == ChatRole::User).unwrap();
        match &last_user.content {
            ChatContent::Blocks(blocks) => match &blocks[0] {
                ChatBlock::ToolResult { is_error, tool_name, .. } => {
                    assert!(is_error);
                    assert_eq!(tool_name, "missing_tool");
                }
                other => panic!("expected tool-result, got {other:?}"),
            },
            other => panic!("expected blocks, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_failure_yields_error_result_but_continues() {
        let provider = MockChatProvider::new("recovered");
        provider.queue_result(tool_call_result("tc", "echo", serde_json::json!({})));
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler {
                idempotent: false,
                calls: calls.clone(),
                output: serde_json::json!(null),
                fail: true,
            },
        );
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(r.stop_reason, ToolLoopStopReason::End);
        assert_eq!(r.final_text, "recovered");
    }

    // ---- Abort ----

    #[tokio::test]
    async fn abort_before_first_turn() {
        let provider = MockChatProvider::new("unused");
        let handlers: HashMap<String, Box<dyn ToolHandler>> = HashMap::new();
        let abort: Box<dyn Fn() -> bool + Send + Sync> = Box::new(|| true);
        let r = tool_loop(&provider, ToolLoopOpts::default(), &handlers, &NoopHooks, &abort)
            .await
            .unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::Aborted);
        assert_eq!(r.total_turns, 0);
    }

    #[tokio::test]
    async fn abort_mid_dispatch_stops_before_next_turn() {
        // First turn returns a tool call; abort flips true after the first
        // provider call so dispatch sees it and bails with Aborted.
        let provider = MockChatProvider::new("unused");
        provider.queue_result(tool_call_result("tc", "echo", serde_json::json!({})));
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler {
                idempotent: false,
                calls: calls.clone(),
                output: serde_json::json!(1),
                fail: false,
            },
        );
        // abort returns false on the loop-top check (turn 0), then true once the
        // per-call dispatch check runs. Use a counter: first call false, rest true.
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        let abort: Box<dyn Fn() -> bool + Send + Sync> =
            Box::new(move || seen2.fetch_add(1, Ordering::SeqCst) >= 1);
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &abort).await.unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::Aborted);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "tool never executed after abort");
    }

    // ---- Crash-replay reconciliation ----
    //
    // With NoopHooks, on_tool_call_start yields the fallback id
    // `inline-{turn}-{ordinal}`. A fresh run starts at turn 0, so the first
    // tool call is keyed `inline-0-0`. We seed replay_state.prior_tools with
    // that key to exercise each short-circuit branch.

    fn replay_state_with(id: &str, status: PriorToolStatus) -> ToolLoopReplayState {
        let mut prior_tools = HashMap::new();
        prior_tools.insert(id.to_string(), status);
        ToolLoopReplayState {
            prior_messages: Vec::new(),
            prior_tools,
            next_turn_idx: 0,
            next_message_idx: 0,
        }
    }

    #[tokio::test]
    async fn replay_complete_short_circuits_without_reexecute() {
        let provider = MockChatProvider::new("done");
        provider.queue_result(tool_call_result("tc", "echo", serde_json::json!({})));
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler {
                idempotent: false,
                calls: calls.clone(),
                output: serde_json::json!("fresh"),
                fail: false,
            },
        );
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            replay_state: Some(replay_state_with(
                "inline-0-0",
                PriorToolStatus::Complete { output: serde_json::json!("prior") },
            )),
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "prior complete: no re-execute");
        assert_eq!(r.stop_reason, ToolLoopStopReason::End);
        // The recorded prior output is fed back, not a fresh execution.
        let user_msg = r.messages.iter().find(|m| m.role == ChatRole::User
            && matches!(&m.content, ChatContent::Blocks(b) if matches!(b.first(), Some(ChatBlock::ToolResult{..}))));
        let user_msg = user_msg.expect("tool-result user turn present");
        if let ChatContent::Blocks(blocks) = &user_msg.content {
            if let ChatBlock::ToolResult { output, is_error, .. } = &blocks[0] {
                assert_eq!(output, &serde_json::json!("prior"));
                assert!(!is_error);
            } else {
                panic!("expected tool-result");
            }
        }
    }

    #[tokio::test]
    async fn replay_failed_short_circuits_as_error() {
        let provider = MockChatProvider::new("done");
        provider.queue_result(tool_call_result("tc", "echo", serde_json::json!({})));
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler {
                idempotent: false,
                calls: calls.clone(),
                output: serde_json::json!("fresh"),
                fail: false,
            },
        );
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            replay_state: Some(replay_state_with(
                "inline-0-0",
                PriorToolStatus::Failed { error: "prior boom".to_string() },
            )),
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "prior failed: no re-execute");
        assert_eq!(r.stop_reason, ToolLoopStopReason::End);
    }

    #[tokio::test]
    async fn replay_pending_nonidempotent_is_unrecoverable() {
        let provider = MockChatProvider::new("unused");
        provider.queue_result(tool_call_result("tc", "echo", serde_json::json!({})));
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler {
                idempotent: false,
                calls: calls.clone(),
                output: serde_json::json!(1),
                fail: false,
            },
        );
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            replay_state: Some(replay_state_with("inline-0-0", PriorToolStatus::Pending)),
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(r.stop_reason, ToolLoopStopReason::Unrecoverable);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "non-idempotent pending: not re-run");
    }

    #[tokio::test]
    async fn replay_pending_idempotent_reexecutes() {
        let provider = MockChatProvider::new("done");
        provider.queue_result(tool_call_result("tc", "echo", serde_json::json!({})));
        let calls = Arc::new(AtomicUsize::new(0));
        let handlers = handlers_with(
            "echo",
            RecordingHandler {
                idempotent: true,
                calls: calls.clone(),
                output: serde_json::json!("re-run"),
                fail: false,
            },
        );
        let opts = ToolLoopOpts {
            initial_messages: vec![ChatMessage::text(ChatRole::User, "go")],
            replay_state: Some(replay_state_with("inline-0-0", PriorToolStatus::Pending)),
            ..Default::default()
        };
        let r = tool_loop(&provider, opts, &handlers, &NoopHooks, &no_abort()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "idempotent pending: safely re-run");
        assert_eq!(r.stop_reason, ToolLoopStopReason::End);
    }
}
