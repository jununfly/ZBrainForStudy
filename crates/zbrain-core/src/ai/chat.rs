//! Provider-neutral chat completion abstraction.
//!
//! Phase 8 slice 3 (Part6). Ports the chat surface of `src/core/ai/gateway.ts`
//! (`chat()` — `gateway.ts:2284`, the message/tool/result types at
//! `gateway.ts:2151-2206`, the stop-reason mapping at `gateway.ts:2261`, and
//! the error hierarchy in `errors.ts`).
//!
//! ## Why a standalone trait, not an extension of [`crate::llm::LlmClient`]
//!
//! `LlmClient` is a Think-only single-shot seam: `system + user + context`
//! flattened to a plain-`String` answer, no multi-turn message array, no tool
//! calls, no structured blocks. Chat is a fundamentally different shape —
//! multi-turn `messages`, provider-neutral `tool-call`/`tool-result` blocks,
//! and a structured [`ChatResult`] carrying usage/stop-reason/provider
//! metadata. Folding both into one trait would force Think impls to stub chat
//! (or vice versa) and violate the deep-module boundary. So [`ChatProvider`]
//! lives here; `llm.rs` is untouched.
//!
//! ## Trait shape (mirrors `rerank_client.rs`)
//!
//! [`ChatProvider`] is the transport seam: production wires the
//! `reqwest`-backed [`OpenAiChatProvider`] (feature `openai`); tests install
//! [`MockChatProvider`] to exercise call-site logic without the network. This
//! mirrors the [`crate::rerank_client::RerankClient`] precedent (trait + mock +
//! classified error).
//!
//! ## Scope of this slice
//!
//! In: the provider-neutral types, the [`ChatProvider`] trait, a mock, one
//! real OpenAI-over-HTTP implementation (`/chat/completions`, extending the
//! `llm.rs::OpenAiClient` precedent), the native [`AnthropicChatProvider`]
//! over the `/v1/messages` API (slice 1-4-1, feature `anthropic`) with
//! Anthropic prompt-cache `cache_control`, and — since slice 1-4-2 — the
//! native [`GeminiChatProvider`] over the `generateContent` API (feature
//! `google`), which speaks Gemini's `contents`/`parts`/`functionCall` wire
//! format (no prompt cache). Since slice 1-4-3 the recipe→provider factory
//! [`instantiate_chat`] dispatches on `recipe.implementation` (the Rust
//! equivalent of the TS `instantiateChat` switch), turning a resolved recipe
//! into a live boxed [`ChatProvider`] — this is the seam that lets the three
//! native providers actually reach production. Out: budget tracking lives in
//! its own [`crate::budget`] module (the TS `withBudgetTracker` rides an
//! `AsyncLocalStorage`; Rust threads `Option<&BudgetTracker>` explicitly
//! instead). The multi-turn `toolLoop` landed in slice 5. Capability gating
//! reuses [`crate::ai::assert_touchpoint`] from slice 2.

use super::resolver::AiConfigError;
use super::types::{Implementation, Recipe};
use std::collections::VecDeque;
use std::sync::Mutex;

/// A message role in the provider-neutral chat protocol. Mirrors the TS
/// `ChatRole` union (`gateway.ts:2151`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl ChatRole {
    /// Wire string used by the OpenAI-compatible `messages[].role` field.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        }
    }
}

/// A single content part inside a [`ChatMessage`]. Mirrors the TS `ChatBlock`
/// discriminated union (`gateway.ts:2153-2156`): text, an assistant-produced
/// tool call, or a caller-supplied tool result fed back on the next turn.
///
/// `input`/`output` are opaque JSON (`serde_json::Value`) — the SDK-neutral
/// equivalent of the TS `unknown`. There is intentionally no image part; TS
/// multimodal rides a separate `embedMultimodal` path outside the chat
/// contract.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatBlock {
    /// Plain text content.
    Text { text: String },
    /// Assistant asked to call a tool. `input` is the tool's JSON arguments.
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    /// Caller's result for a prior tool call, fed back into the next turn.
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        output: serde_json::Value,
        /// True when the tool execution errored (mirrors TS `isError?`).
        is_error: bool,
    },
}

/// Content of a [`ChatMessage`]: either a flat string or a list of typed
/// blocks. Mirrors the TS `string | ChatBlock[]` (`gateway.ts:2160`).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatContent {
    Text(String),
    Blocks(Vec<ChatBlock>),
}

/// One provider-neutral message. Mirrors the TS `ChatMessage`
/// (`gateway.ts:2158`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatContent,
}

impl ChatMessage {
    /// Convenience constructor for a plain-text message.
    #[must_use]
    pub fn text(role: ChatRole, text: impl Into<String>) -> Self {
        Self { role, content: ChatContent::Text(text.into()) }
    }
}

/// A tool the model may call. Mirrors the TS `ChatToolDef`
/// (`gateway.ts:2163`). `input_schema` is a raw JSON Schema object (the TS
/// `Record<string, unknown>`), not a zod/typed schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for tool input.
    pub input_schema: serde_json::Value,
}

/// Why the model stopped. Provider-neutral mapping of the SDK
/// `finish_reason` / `stop_reason`. Mirrors the TS `ChatResult['stopReason']`
/// union (`gateway.ts:2176`) and the `mapStopReason` logic (`gateway.ts:2261`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Normal completion (`stop`/`end`/`end-turn`).
    End,
    /// Model requested tool calls (`tool-calls`/`tool_calls`).
    ToolCalls,
    /// Hit the output token limit (`length`/`max-tokens`).
    Length,
    /// Model refused (Anthropic `stop_reason: 'refusal'`).
    Refusal,
    /// Provider content filter tripped (`content-filter`/`content_filter`).
    ContentFilter,
    /// Anything unrecognized.
    Other,
}

impl StopReason {
    /// Map a raw SDK `finish_reason` plus an optional Anthropic `stop_reason`
    /// into the provider-neutral variant. Mirrors `mapStopReason`
    /// (`gateway.ts:2261-2274`): Anthropic refusal wins first, then OpenAI
    /// content-filter / tool-calls / length / stop, else `Other`.
    ///
    /// The `anthropic_stop` channel recognizes the full set of native
    /// Anthropic `stop_reason` values, not just `refusal`. Anthropic's raw
    /// wire values use underscores and distinct spellings (`end_turn`,
    /// `tool_use`, `max_tokens`, `stop_sequence`) that the OpenAI
    /// `finish_reason` arm below does not match — the native `AnthropicChatProvider`
    /// calls `from_signals(None, Some(raw_stop_reason))`, so these must be
    /// handled here or every Anthropic turn falls through to `Other` (which
    /// would silently break `tool_loop`'s `ToolCalls`-driven continuation).
    #[must_use]
    pub fn from_signals(finish_reason: Option<&str>, anthropic_stop: Option<&str>) -> Self {
        match anthropic_stop {
            Some("refusal") => return StopReason::Refusal,
            // `tool_use` is Anthropic's signal to run the requested tools and
            // continue the loop — must map to ToolCalls, not Other.
            Some("tool_use") => return StopReason::ToolCalls,
            Some("max_tokens") => return StopReason::Length,
            Some("end_turn" | "stop_sequence") => return StopReason::End,
            _ => {}
        }
        match finish_reason {
            Some("content-filter" | "content_filter") => StopReason::ContentFilter,
            Some("tool-calls" | "tool_calls") => StopReason::ToolCalls,
            Some("length" | "max-tokens") => StopReason::Length,
            Some("stop" | "end" | "end-turn") => StopReason::End,
            _ => StopReason::Other,
        }
    }
}

/// Provider-neutral token usage. Mirrors the TS `ChatResult['usage']`
/// (`gateway.ts:2178-2183`): snake_case `input_tokens`/`output_tokens` plus
/// two Anthropic-only cache counters (0 on providers that don't return them).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// Result of one chat completion turn. Mirrors the TS `ChatResult`
/// (`gateway.ts:2170-2190`) field-for-field: convenience `text`, structured
/// `blocks`, `stop_reason`, `usage`, the answering `model` (`provider:modelId`)
/// and `provider_id`, plus opaque `provider_metadata` for downstream callers
/// that need raw provider signals.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResult {
    /// Final text content, concatenated from all text blocks.
    pub text: String,
    /// Raw assistant response blocks (text + tool-call) for persistence.
    pub blocks: Vec<ChatBlock>,
    pub stop_reason: StopReason,
    pub usage: ChatUsage,
    /// `provider:modelId` of the model that actually answered.
    pub model: String,
    /// Recipe id for the answering provider.
    pub provider_id: String,
    /// Raw provider metadata (Anthropic cache fields, OpenAI finish_reason,
    /// etc.). `None` when the provider returned nothing extra.
    pub provider_metadata: Option<serde_json::Value>,
}

/// Options for one chat completion turn. Mirrors the TS `ChatOpts`
/// (`gateway.ts:2192-2206`). `abortSignal` is intentionally omitted for now
/// (no cancellation plumbing on the Rust query path yet); `cache_system` is
/// carried through but only honored on providers with `supports_prompt_cache`.
#[derive(Debug, Clone, Default)]
pub struct ChatOpts {
    /// `provider:modelId`. `None` lets the caller/provider pick its default.
    pub model: Option<String>,
    /// System prompt (top-level, not part of `messages` — matches TS).
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ChatToolDef>,
    /// Max output tokens. `None` defaults to 4096 (matches TS
    /// `gateway.ts:2288`).
    pub max_tokens: Option<u32>,
    /// Anthropic-only: cache the system prompt. Silently ignored on providers
    /// without `supports_prompt_cache` (matches TS `gateway.ts:2351`).
    pub cache_system: bool,
}

/// AI service error hierarchy. Mirrors the three TS classes in `errors.ts`,
/// collapsed into one enum keyed by the caller decision each drives:
///
/// - [`ChatError::Config`] — user fixes: bad key, missing model, dim mismatch.
///   Abort + show the `fix` recovery recipe. (TS `AIConfigError`.)
/// - [`ChatError::Transient`] — retryable: SDK retries exhausted, sustained
///   rate limit, 5xx, timeout, network blip. Propagate so a job queue can
///   retry later. (TS `AITransientError`.)
///
/// The TS base `AIServiceError` has no Rust variant of its own — every
/// concrete error is one of the two leaves, matching how `normalizeAIError`
/// only ever constructs `AIConfigError` or `AITransientError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatError {
    /// Non-retryable, user-fixable. `fix` is a human-readable recovery recipe.
    Config { message: String, fix: Option<String> },
    /// Retryable. The default class for unknown/5xx/network errors so callers
    /// don't permanently abort on a transient blip (matches TS default).
    Transient { message: String },
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Config { message, fix: Some(fix) } => write!(f, "{message} — {fix}"),
            ChatError::Config { message, fix: None } | ChatError::Transient { message } => {
                write!(f, "{message}")
            }
        }
    }
}

impl std::error::Error for ChatError {}

impl ChatError {
    /// True for the retryable class.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, ChatError::Transient { .. })
    }

    /// Classify a raw upstream failure into the hierarchy by HTTP status +
    /// error name. Mirrors `normalizeAIError` (`errors.ts:44-71`):
    ///
    /// - 4xx except 429 → [`ChatError::Config`] (401/403 get an API-key hint,
    ///   other 4xx get a model-id/provider-options hint).
    /// - named `LoadAPIKeyError` / `InvalidArgumentError` → `Config` (no hint).
    /// - everything else (5xx, 429, timeout, network) → [`ChatError::Transient`].
    ///
    /// `context` is prefixed as `[context] ` so failures name their call site
    /// (matches the TS `ctxPrefix`).
    #[must_use]
    pub fn normalize(
        status: Option<u16>,
        name: Option<&str>,
        message: &str,
        context: Option<&str>,
    ) -> Self {
        let prefixed = match context {
            Some(ctx) => format!("[{ctx}] {message}"),
            None => message.to_string(),
        };

        if let Some(s) = status {
            if (400..500).contains(&s) && s != 429 {
                let fix = if s == 401 || s == 403 {
                    "Check your API key is valid and has access to this model."
                } else {
                    "Check your model id + provider options match the provider API."
                };
                return ChatError::Config { message: prefixed, fix: Some(fix.to_string()) };
            }
        }

        if matches!(name, Some("LoadAPIKeyError" | "InvalidArgumentError")) {
            return ChatError::Config { message: prefixed, fix: None };
        }

        ChatError::Transient { message: prefixed }
    }
}

/// Transport seam for a single chat completion turn. Production wires the
/// `reqwest`-backed [`OpenAiChatProvider`] (feature `openai`); tests install
/// [`MockChatProvider`]. Mirrors the [`crate::rerank_client::RerankClient`]
/// trait precedent.
#[async_trait::async_trait]
pub trait ChatProvider: std::fmt::Debug + Send + Sync {
    /// Run one chat completion turn. Returns a structured [`ChatResult`] on
    /// success or a classified [`ChatError`].
    async fn chat(&self, opts: ChatOpts) -> Result<ChatResult, ChatError>;
}

// ---- Mock ----

#[derive(Debug, Default)]
struct MockChatState {
    responses: VecDeque<Result<ChatResult, ChatError>>,
    default_text: String,
}

/// Mock chat provider for testing. Returns queued responses in FIFO order,
/// falling back to a canned text answer. Mirrors [`crate::llm::MockLlmClient`]
/// and the `MockEmbeddingProvider` precedent.
#[derive(Debug)]
pub struct MockChatProvider {
    state: Mutex<MockChatState>,
}

impl MockChatProvider {
    /// Create a mock with a default text answer used once the queue drains.
    #[must_use]
    pub fn new(default_text: impl Into<String>) -> Self {
        Self {
            state: Mutex::new(MockChatState {
                responses: VecDeque::new(),
                default_text: default_text.into(),
            }),
        }
    }

    /// Queue a successful text-only result.
    pub fn queue_text(&self, text: impl Into<String>) {
        let text = text.into();
        self.state.lock().unwrap().responses.push_back(Ok(ChatResult {
            text: text.clone(),
            blocks: vec![ChatBlock::Text { text }],
            stop_reason: StopReason::End,
            usage: ChatUsage { input_tokens: 10, output_tokens: 20, ..Default::default() },
            model: "mock:mock-model".to_string(),
            provider_id: "mock".to_string(),
            provider_metadata: None,
        }));
    }

    /// Queue a full pre-built result (e.g. to assert tool-call blocks).
    pub fn queue_result(&self, result: ChatResult) {
        self.state.lock().unwrap().responses.push_back(Ok(result));
    }

    /// Queue an error.
    pub fn queue_error(&self, error: ChatError) {
        self.state.lock().unwrap().responses.push_back(Err(error));
    }
}

#[async_trait::async_trait]
impl ChatProvider for MockChatProvider {
    async fn chat(&self, _opts: ChatOpts) -> Result<ChatResult, ChatError> {
        let mut state = self.state.lock().unwrap();
        if let Some(response) = state.responses.pop_front() {
            return response;
        }
        let text = state.default_text.clone();
        Ok(ChatResult {
            text: text.clone(),
            blocks: vec![ChatBlock::Text { text }],
            stop_reason: StopReason::End,
            usage: ChatUsage::default(),
            model: "mock:mock-model".to_string(),
            provider_id: "mock".to_string(),
            provider_metadata: None,
        })
    }
}

// ---- OpenAI-over-HTTP implementation ----

/// Serialize provider-neutral [`ChatMessage`]s into the OpenAI
/// `/chat/completions` `messages` array. Pure (no IO) so it can be unit-tested
/// without a network. Mirrors how the AI SDK's OpenAI adapter lays out
/// tool-call/tool-result parts:
///
/// - Text content → `{role, content: "<text>"}`.
/// - Assistant tool-call blocks → `{role:"assistant", tool_calls:[{id, type:"function",
///   function:{name, arguments:"<json>"}}]}`.
/// - Tool-result blocks → one `{role:"tool", tool_call_id, content:"<json>"}`
///   message each (OpenAI models tool output as separate `tool` messages).
#[must_use]
pub fn serialize_messages_openai(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for msg in messages {
        let role = msg.role.as_str();
        match &msg.content {
            ChatContent::Text(text) => {
                out.push(serde_json::json!({ "role": role, "content": text }));
            }
            ChatContent::Blocks(blocks) => {
                // Collect text + tool-calls for a single assistant/user message;
                // emit tool-results as their own `tool` messages.
                let mut text_parts = String::new();
                let mut tool_calls = Vec::new();
                for block in blocks {
                    match block {
                        ChatBlock::Text { text } => text_parts.push_str(text),
                        ChatBlock::ToolCall { tool_call_id, tool_name, input } => {
                            tool_calls.push(serde_json::json!({
                                "id": tool_call_id,
                                "type": "function",
                                "function": {
                                    "name": tool_name,
                                    "arguments": input.to_string(),
                                },
                            }));
                        }
                        ChatBlock::ToolResult { tool_call_id, output, .. } => {
                            out.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tool_call_id,
                                "content": output.to_string(),
                            }));
                        }
                    }
                }
                if !text_parts.is_empty() || !tool_calls.is_empty() {
                    let mut m = serde_json::json!({ "role": role });
                    if !text_parts.is_empty() {
                        m["content"] = serde_json::json!(text_parts);
                    }
                    if !tool_calls.is_empty() {
                        m["tool_calls"] = serde_json::json!(tool_calls);
                    }
                    out.push(m);
                }
            }
        }
    }
    out
}

/// Serialize [`ChatToolDef`]s into the OpenAI `tools` array
/// (`[{type:"function", function:{name, description, parameters}}]`). Pure.
#[must_use]
pub fn serialize_tools_openai(tools: &[ChatToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                },
            })
        })
        .collect()
}

/// Parse an OpenAI `/chat/completions` response body into a [`ChatResult`].
/// Pure (takes the already-decoded JSON) so the normalization is unit-testable
/// against captured fixtures without a live call. `provider_id` and the
/// resolved `model_id` are threaded in to build the `provider:modelId` label.
///
/// Mirrors the block/usage/stop-reason normalization in `chat`
/// (`gateway.ts:2397-2449`): text + tool-call blocks, `input_tokens`/
/// `output_tokens` from `usage`, and `mapStopReason` over `finish_reason`.
pub fn parse_openai_response(
    json: &serde_json::Value,
    provider_id: &str,
    model_id: &str,
) -> Result<ChatResult, ChatError> {
    let choice = json
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| ChatError::Transient {
            message: "OpenAI response missing choices[0]".to_string(),
        })?;
    let message = choice.get("message").ok_or_else(|| ChatError::Transient {
        message: "OpenAI response missing choices[0].message".to_string(),
    })?;

    let mut blocks = Vec::new();
    if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
        if !content.is_empty() {
            blocks.push(ChatBlock::Text { text: content.to_string() });
        }
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tool_calls {
            let tool_call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let func = tc.get("function");
            let tool_name = func
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // OpenAI returns arguments as a JSON *string*; parse back to Value,
            // falling back to the raw string on parse failure.
            let raw_args = func
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input =
                serde_json::from_str(raw_args).unwrap_or_else(|_| serde_json::json!(raw_args));
            blocks.push(ChatBlock::ToolCall { tool_call_id, tool_name, input });
        }
    }

    let usage = json.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());
    let stop_reason = StopReason::from_signals(finish_reason, None);

    let text = blocks
        .iter()
        .filter_map(|b| match b {
            ChatBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();

    Ok(ChatResult {
        text,
        blocks,
        stop_reason,
        usage: ChatUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
        model: format!("{provider_id}:{model_id}"),
        provider_id: provider_id.to_string(),
        provider_metadata: json.get("usage").cloned(),
    })
}

/// Production chat provider hitting an OpenAI-compatible `/chat/completions`
/// endpoint over HTTP. Extends the `llm.rs::OpenAiClient` precedent to the
/// full multi-turn chat contract (messages/tools/blocks). Behind the `openai`
/// feature so a build without it stays network-free.
#[cfg(feature = "openai")]
#[derive(Debug, Clone)]
pub struct OpenAiChatProvider {
    api_key: String,
    base_url: String,
    /// Recipe id used to label results (`provider:modelId`).
    provider_id: String,
}

#[cfg(feature = "openai")]
impl OpenAiChatProvider {
    /// Default API root, matching the AI SDK's built-in OpenAI base URL. The
    /// native recipe carries `base_url_default: None` (the TS SDK supplies this
    /// internally); the provider factory uses this constant for that case.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";

    /// Create a provider with explicit config. `base_url` should be the API
    /// root (e.g. `https://api.openai.com/v1`); `/chat/completions` is
    /// appended per call.
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        provider_id: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_id: provider_id.into(),
        }
    }

    /// Build the request body from opts (pure; exposed for testing). Requires
    /// a resolved `model_id` since `ChatOpts.model` is `provider:model`.
    #[must_use]
    pub fn build_body(&self, opts: &ChatOpts, model_id: &str) -> serde_json::Value {
        let mut messages = Vec::new();
        if let Some(system) = &opts.system {
            messages.push(serde_json::json!({ "role": "system", "content": system }));
        }
        messages.extend(serialize_messages_openai(&opts.messages));

        let mut body = serde_json::json!({
            "model": model_id,
            "messages": messages,
            "max_tokens": opts.max_tokens.unwrap_or(4096),
        });
        if !opts.tools.is_empty() {
            body["tools"] = serde_json::json!(serialize_tools_openai(&opts.tools));
        }
        body
    }
}

#[cfg(feature = "openai")]
#[async_trait::async_trait]
impl ChatProvider for OpenAiChatProvider {
    async fn chat(&self, opts: ChatOpts) -> Result<ChatResult, ChatError> {
        // `ChatOpts.model` is `provider:modelId`; strip the provider prefix for
        // the wire `model` field. Fall back to the whole string if unprefixed.
        let model_spec = opts.model.clone().unwrap_or_default();
        let model_id = model_spec
            .split_once(':')
            .map_or_else(|| model_spec.clone(), |(_, m)| m.to_string());

        let ctx = format!("chat({}:{})", self.provider_id, model_id);
        let body = self.build_body(&opts, &model_id);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ChatError::normalize(None, None, &e.to_string(), Some(&ctx)))?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response
                .text()
                .await
                .unwrap_or_else(|_| "No error details".to_string());
            return Err(ChatError::normalize(
                Some(status),
                None,
                &format!("HTTP {status}: {body_text}"),
                Some(&ctx),
            ));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ChatError::normalize(None, None, &e.to_string(), Some(&ctx)))?;

        parse_openai_response(&json, &self.provider_id, &model_id)
    }
}

// ============================================================================
// Native Anthropic provider (Part6 slice 1-4-1).
//
// Ports the hand-written Anthropic Messages API wire format from the legacy
// subagent direct path (`src/core/minions/handlers/subagent.ts:478-666`) — the
// only place TS builds the Anthropic body by hand (the `gateway.chat` path
// delegates to `@ai-sdk/anthropic`, an opaque SDK with no wire sample). Mirrors
// the `OpenAiChatProvider` three-part shape: pure `build_body` /
// `serialize_messages` / `serialize_tools` / `parse_*` functions plus a thin
// `reqwest`-backed `ChatProvider` impl. Behind the `anthropic` feature.
//
// Key wire differences vs OpenAI (see decisions on roadmap node 1-4-1):
//   - `system` is a TOP-LEVEL field, not a message. When prompt-cache is on it
//     becomes an array block `[{type:text, text, cache_control:{ephemeral}}]`;
//     otherwise a plain string.
//   - Assistant `tool_use` and user `tool_result` are CONTENT BLOCKS inside the
//     messages array, not OpenAI's separate `tool_calls` field / `tool` role.
//   - `tool_use.input` is a JSON object (NOT stringified like OpenAI arguments).
//   - `tool_result.content` is stringified when the output is not already a
//     string (mirrors TS `asStringIfNotObject`); `is_error` is emitted only
//     when true.
//   - tools are `[{name, description, input_schema}]` (no `{type:function}`
//     wrapper); the LAST tool def carries `cache_control` when caching.
//   - Requires the `anthropic-version` header and a mandatory `max_tokens`.
// ============================================================================

/// Anthropic wire `role` for a neutral [`ChatRole`]. Anthropic has no `system`
/// or `tool` role: system rides the top-level field, and tool results ride a
/// `user` message's content blocks (see [`serialize_messages_anthropic`]).
#[must_use]
fn anthropic_role(role: ChatRole) -> &'static str {
    match role {
        ChatRole::Assistant => "assistant",
        // System never reaches here (stripped into the top-level `system`
        // field by build_body); Tool results are folded into `user` messages.
        ChatRole::System | ChatRole::User | ChatRole::Tool => "user",
    }
}

/// Stringify a tool-result payload the way TS `asStringIfNotObject` does: leave
/// JSON strings as their inner text, serialize everything else to compact JSON.
#[must_use]
fn anthropic_tool_result_content(output: &serde_json::Value) -> String {
    match output {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Serialize neutral [`ChatMessage`]s into the Anthropic `messages` array.
/// Pure (no I/O) so it is unit-testable against fixtures.
///
/// Each neutral message maps to exactly one wire message — no cross-message
/// merging, because the upstream `tool_loop` already groups a turn's tool
/// results into a single `{role:User, Blocks([ToolResult...])}` message
/// (`tool_loop.rs:486-491`), matching subagent.ts's N-results→1-user-message
/// shape. Text content becomes a plain string; block content becomes an array
/// of Anthropic content blocks.
#[must_use]
pub fn serialize_messages_anthropic(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for msg in messages {
        // System messages, if any slipped into the array, are not valid here;
        // they belong in the top-level `system` field. Skip defensively.
        if msg.role == ChatRole::System {
            continue;
        }
        let role = anthropic_role(msg.role);
        match &msg.content {
            ChatContent::Text(text) => {
                out.push(serde_json::json!({ "role": role, "content": text }));
            }
            ChatContent::Blocks(blocks) => {
                let mut content = Vec::new();
                for block in blocks {
                    match block {
                        ChatBlock::Text { text } => {
                            content.push(serde_json::json!({ "type": "text", "text": text }));
                        }
                        ChatBlock::ToolCall { tool_call_id, tool_name, input } => {
                            // Anthropic tool_use: input is the raw JSON object.
                            content.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tool_call_id,
                                "name": tool_name,
                                "input": input,
                            }));
                        }
                        ChatBlock::ToolResult { tool_call_id, output, is_error, .. } => {
                            let mut tr = serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": tool_call_id,
                                "content": anthropic_tool_result_content(output),
                            });
                            // Only emit is_error when true (matches subagent.ts).
                            if *is_error {
                                tr["is_error"] = serde_json::json!(true);
                            }
                            content.push(tr);
                        }
                    }
                }
                out.push(serde_json::json!({ "role": role, "content": content }));
            }
        }
    }
    out
}

/// Serialize [`ChatToolDef`]s into the Anthropic `tools` array
/// (`[{name, description, input_schema}]` — no `{type:function}` wrapper). When
/// `cache_last` is true the final tool def gets `cache_control:{ephemeral}`;
/// Anthropic treats `cache_control` as "cache everything up to and including
/// this block", so caching only the last def caches the whole tool preamble
/// (mirrors subagent.ts:496-498). Pure.
#[must_use]
pub fn serialize_tools_anthropic(tools: &[ChatToolDef], cache_last: bool) -> Vec<serde_json::Value> {
    let last = tools.len().saturating_sub(1);
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut def = serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            });
            if cache_last && i == last {
                def["cache_control"] = serde_json::json!({ "type": "ephemeral" });
            }
            def
        })
        .collect()
}

/// Parse an Anthropic `/v1/messages` response body into a [`ChatResult`].
/// Pure (takes decoded JSON) so normalization is unit-testable against
/// fixtures. Mirrors subagent.ts:527-582: text + tool_use blocks, usage token
/// counts (including Anthropic-only cache tokens), and the native `stop_reason`
/// routed through [`StopReason::from_signals`]'s Anthropic channel.
pub fn parse_anthropic_response(
    json: &serde_json::Value,
    provider_id: &str,
    model_id: &str,
) -> Result<ChatResult, ChatError> {
    let raw_blocks = json
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| ChatError::Transient {
            message: "Anthropic response missing content array".to_string(),
        })?;

    let mut blocks = Vec::new();
    for part in raw_blocks {
        match part.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    blocks.push(ChatBlock::Text { text: text.to_string() });
                }
            }
            Some("tool_use") => {
                let tool_call_id =
                    part.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let tool_name =
                    part.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let input = part.get("input").cloned().unwrap_or_else(|| serde_json::json!({}));
                blocks.push(ChatBlock::ToolCall { tool_call_id, tool_name, input });
            }
            _ => {}
        }
    }

    let usage = json.get("usage");
    let read = |key: &str| -> u64 {
        usage.and_then(|u| u.get(key)).and_then(serde_json::Value::as_u64).unwrap_or(0)
    };
    let input_tokens = read("input_tokens");
    let output_tokens = read("output_tokens");
    let cache_read_tokens = read("cache_read_input_tokens");
    let cache_creation_tokens = read("cache_creation_input_tokens");

    let raw_stop = json.get("stop_reason").and_then(|v| v.as_str());
    let stop_reason = StopReason::from_signals(None, raw_stop);

    let text = blocks
        .iter()
        .filter_map(|b| match b {
            ChatBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();

    Ok(ChatResult {
        text,
        blocks,
        stop_reason,
        usage: ChatUsage { input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens },
        model: format!("{provider_id}:{model_id}"),
        provider_id: provider_id.to_string(),
        provider_metadata: usage.cloned(),
    })
}

/// Production chat provider hitting the native Anthropic `/v1/messages` API
/// over HTTP. Mirrors [`OpenAiChatProvider`] but speaks the Anthropic wire
/// format. Behind the `anthropic` feature so a build without it stays
/// network-free.
#[cfg(feature = "anthropic")]
#[derive(Debug, Clone)]
pub struct AnthropicChatProvider {
    api_key: String,
    base_url: String,
    /// Recipe id used to label results (`provider:modelId`).
    provider_id: String,
    /// `anthropic-version` header value.
    api_version: String,
}

#[cfg(feature = "anthropic")]
impl AnthropicChatProvider {
    /// Default `anthropic-version` pinned for the Messages API.
    pub const DEFAULT_API_VERSION: &'static str = "2023-06-01";

    /// Default API root, matching the AI SDK's built-in Anthropic base URL. The
    /// native recipe carries `base_url_default: None`; the provider factory
    /// uses this constant for that case.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.anthropic.com";

    /// Create a provider. `base_url` should be the API root (e.g.
    /// `https://api.anthropic.com`); `/v1/messages` is appended per call.
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        provider_id: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_id: provider_id.into(),
            api_version: Self::DEFAULT_API_VERSION.to_string(),
        }
    }

    /// Build the request body from opts (pure; exposed for testing). Requires a
    /// resolved `model_id` since `ChatOpts.model` is `provider:model`. When
    /// `opts.cache_system` is set, the system prompt is emitted as    /// an array block carrying `cache_control:{ephemeral}` and the last tool
    /// def is likewise cached; `AnthropicChatProvider` always supports
    /// prompt-cache, so the effective condition is simply `opts.cache_system`.
    #[must_use]
    pub fn build_body(&self, opts: &ChatOpts, model_id: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": model_id,
            "max_tokens": opts.max_tokens.unwrap_or(4096),
            "messages": serialize_messages_anthropic(&opts.messages),
        });

        if let Some(system) = &opts.system {
            if opts.cache_system {
                // Array form is required to attach cache_control (subagent.ts:484).
                body["system"] = serde_json::json!([{
                    "type": "text",
                    "text": system,
                    "cache_control": { "type": "ephemeral" },
                }]);
            } else {
                body["system"] = serde_json::json!(system);
            }
        }

        if !opts.tools.is_empty() {
            body["tools"] =
                serde_json::json!(serialize_tools_anthropic(&opts.tools, opts.cache_system));
        }
        body
    }
}

#[cfg(feature = "anthropic")]
#[async_trait::async_trait]
impl ChatProvider for AnthropicChatProvider {
    async fn chat(&self, opts: ChatOpts) -> Result<ChatResult, ChatError> {
        // `ChatOpts.model` is `provider:modelId`; strip the provider prefix for
        // the wire `model` field. Fall back to the whole string if unprefixed.
        let model_spec = opts.model.clone().unwrap_or_default();
        let model_id = model_spec
            .split_once(':')
            .map_or_else(|| model_spec.clone(), |(_, m)| m.to_string());

        let ctx = format!("chat({}:{})", self.provider_id, model_id);
        let body = self.build_body(&opts, &model_id);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.api_version)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ChatError::normalize(None, None, &e.to_string(), Some(&ctx)))?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response
                .text()
                .await
                .unwrap_or_else(|_| "No error details".to_string());
            return Err(ChatError::normalize(
                Some(status),
                None,
                &format!("HTTP {status}: {body_text}"),
                Some(&ctx),
            ));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ChatError::normalize(None, None, &e.to_string(), Some(&ctx)))?;

        parse_anthropic_response(&json, &self.provider_id, &model_id)
    }
}

// ---------------------------------------------------------------------------
// Native Google Gemini provider (slice 1-4-2)
// ---------------------------------------------------------------------------
//
// Gemini's `generateContent` REST wire format differs structurally from both
// OpenAI and Anthropic, so it gets its own serializer/parser rather than
// reusing either. Key differences (see the slice 1-4-2 decisions):
//
// - Roles are `user` / `model` (no `system`/`assistant`/`tool` literals). The
//   system prompt rides a top-level `systemInstruction`, not a message.
// - A tool call is a `functionCall` part `{name, args}` with NO call id; a
//   tool result is a `functionResponse` part `{name, response}` correlated by
//   name. We synthesize a deterministic `{name}-{index}` id on parse so the
//   neutral `ChatBlock`/`tool_loop`/replay layers keep working.
// - `functionResponse.response` MUST be a JSON object, so non-object tool
//   outputs are wrapped as `{"result": output}` (the mirror image of
//   Anthropic's `asStringIfNotObject`, which flattens to a string).
// - There is no tool stop signal: a response carrying `functionCall` parts
//   still reports `finishReason: "STOP"`. So `parse_gemini_response` maps the
//   finish reason on its own and overrides to `ToolCalls` when any
//   functionCall block is present — it never touches `StopReason::from_signals`
//   (whose OpenAI/Anthropic value sets don't overlap Gemini's).
// - Gemini has no prompt cache (`supports_prompt_cache: false`), so
//   `opts.cache_system` is intentionally ignored here.

/// Gemini wire `role` for a neutral [`ChatRole`]. Gemini only has `user` and
/// `model`; system rides the top-level `systemInstruction` (stripped in
/// [`build_body_gemini`]) and tool results are folded into `user` turns.
#[must_use]
fn gemini_role(role: ChatRole) -> &'static str {
    match role {
        ChatRole::Assistant => "model",
        // System never reaches here (lifted into systemInstruction); User and
        // Tool results both ride `user` turns.
        ChatRole::System | ChatRole::User | ChatRole::Tool => "user",
    }
}

/// Wrap a tool-result payload for Gemini's `functionResponse.response`, which
/// must be a JSON object. Objects pass through untouched; every other JSON
/// value (string/number/array/bool/null) is boxed as `{"result": <value>}`.
/// This is the mirror image of Anthropic's `asStringIfNotObject`, which
/// flattens non-strings to a string.
#[must_use]
fn gemini_function_response(output: &serde_json::Value) -> serde_json::Value {
    if output.is_object() {
        output.clone()
    } else {
        serde_json::json!({ "result": output })
    }
}

/// Serialize neutral [`ChatMessage`]s into Gemini's `contents` array. Pure (no
/// I/O) so it is unit-testable against fixtures.
///
/// Each neutral message maps to exactly one wire `content` — no cross-message
/// merging, for the same reason as the Anthropic path: `tool_loop` already
/// groups a turn's tool results into a single `{role:User, Blocks([...])}`
/// message (`tool_loop.rs:486-491`). System messages that slip into the array
/// are skipped (they belong in `systemInstruction`). Text content becomes a
/// single `{text}` part; block content becomes an array of Gemini parts:
/// `Text -> {text}`, `ToolCall -> {functionCall}`, `ToolResult ->
/// {functionResponse}` (correlated by tool name).
#[must_use]
pub fn serialize_messages_gemini(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for msg in messages {
        if msg.role == ChatRole::System {
            continue;
        }
        let role = gemini_role(msg.role);
        match &msg.content {
            ChatContent::Text(text) => {
                out.push(serde_json::json!({
                    "role": role,
                    "parts": [{ "text": text }],
                }));
            }
            ChatContent::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    match block {
                        ChatBlock::Text { text } => {
                            parts.push(serde_json::json!({ "text": text }));
                        }
                        ChatBlock::ToolCall { tool_name, input, .. } => {
                            // Gemini functionCall: no id on the wire; args is
                            // the raw JSON object.
                            parts.push(serde_json::json!({
                                "functionCall": {
                                    "name": tool_name,
                                    "args": input,
                                },
                            }));
                        }
                        ChatBlock::ToolResult { tool_name, output, .. } => {
                            // Gemini correlates results by name (no id); the
                            // response must be a JSON object. `is_error` has no
                            // wire slot on Gemini, so it is not emitted.
                            parts.push(serde_json::json!({
                                "functionResponse": {
                                    "name": tool_name,
                                    "response": gemini_function_response(output),
                                },
                            }));
                        }
                    }
                }
                out.push(serde_json::json!({ "role": role, "parts": parts }));
            }
        }
    }
    out
}

/// Serialize [`ChatToolDef`]s into Gemini's `tools` array. Gemini wraps all
/// declarations in a single `{functionDeclarations: [...]}` entry (unlike
/// OpenAI's per-tool `{type:function}` wrapper or Anthropic's flat array).
/// `input_schema` maps to `parameters`. Returns an empty vec for no tools so
/// the caller can omit the `tools` field entirely. Pure.
#[must_use]
pub fn serialize_tools_gemini(tools: &[ChatToolDef]) -> Vec<serde_json::Value> {
    if tools.is_empty() {
        return Vec::new();
    }
    let declarations: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            })
        })
        .collect();
    vec![serde_json::json!({ "functionDeclarations": declarations })]
}

/// Map a Gemini `finishReason` string to a neutral [`StopReason`]. Gemini's
/// value set (`STOP`/`MAX_TOKENS`/`SAFETY`/`RECITATION`/`OTHER`/...) does not
/// overlap the OpenAI/Anthropic sets, so this is standalone and never routes
/// through [`StopReason::from_signals`]. Note this does NOT return `ToolCalls`:
/// Gemini reports `STOP` even when the turn carries functionCall parts, so the
/// tool-call override happens in [`parse_gemini_response`] based on the parsed
/// blocks, not here.
#[must_use]
fn gemini_finish_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("MAX_TOKENS") => StopReason::Length,
        // SAFETY (blocked for safety) and RECITATION (blocked for reciting
        // training data) are both content-policy stops.
        Some("SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST") => {
            StopReason::ContentFilter
        }
        Some("STOP") => StopReason::End,
        _ => StopReason::Other,
    }
}

/// Parse a Gemini `generateContent` response body into a [`ChatResult`]. Pure
/// (takes decoded JSON) so normalization is unit-testable against fixtures.
///
/// Walks `candidates[0].content.parts`, mapping `{text}` -> [`ChatBlock::Text`]
/// and `{functionCall}` -> [`ChatBlock::ToolCall`] with a synthesized
/// `{name}-{index}` id (Gemini has no call id). The stop reason comes from
/// [`gemini_finish_reason`], overridden to [`StopReason::ToolCalls`] when any
/// functionCall block is present (Gemini reports `STOP` in that case). Usage
/// reads `usageMetadata.{promptTokenCount, candidatesTokenCount,
/// cachedContentTokenCount}`; Gemini has no cache-creation counter.
pub fn parse_gemini_response(
    json: &serde_json::Value,
    provider_id: &str,
    model_id: &str,
) -> Result<ChatResult, ChatError> {
    let candidate =
        json.get("candidates").and_then(|c| c.as_array()).and_then(|a| a.first()).ok_or_else(
            || ChatError::Transient {
                message: "Gemini response missing candidates array".to_string(),
            },
        )?;

    let raw_parts = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .ok_or_else(|| ChatError::Transient {
            message: "Gemini candidate missing content.parts array".to_string(),
        })?;

    let mut blocks = Vec::new();
    let mut has_tool_call = false;
    for (index, part) in raw_parts.iter().enumerate() {
        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
            blocks.push(ChatBlock::Text { text: text.to_string() });
        } else if let Some(fc) = part.get("functionCall") {
            has_tool_call = true;
            let tool_name =
                fc.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let input = fc.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
            // Gemini has no call id; synthesize a deterministic {name}-{index}
            // so tool_loop/replay have a stable handle. `index` disambiguates
            // same-name parallel calls within one turn.
            let tool_call_id = format!("{tool_name}-{index}");
            blocks.push(ChatBlock::ToolCall { tool_call_id, tool_name, input });
        }
    }

    let raw_finish = candidate.get("finishReason").and_then(|v| v.as_str());
    let stop_reason = if has_tool_call {
        // Gemini reports STOP even when requesting tools; the neutral loop
        // keys on ToolCalls, so override.
        StopReason::ToolCalls
    } else {
        gemini_finish_reason(raw_finish)
    };

    let usage = json.get("usageMetadata");
    let read = |key: &str| -> u64 {
        usage.and_then(|u| u.get(key)).and_then(serde_json::Value::as_u64).unwrap_or(0)
    };
    let input_tokens = read("promptTokenCount");
    let output_tokens = read("candidatesTokenCount");
    // Gemini reports cached prompt tokens but has no cache-creation counter.
    let cache_read_tokens = read("cachedContentTokenCount");

    let text = blocks
        .iter()
        .filter_map(|b| match b {
            ChatBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();

    Ok(ChatResult {
        text,
        blocks,
        stop_reason,
        usage: ChatUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens: 0,
        },
        model: format!("{provider_id}:{model_id}"),
        provider_id: provider_id.to_string(),
        provider_metadata: usage.cloned(),
    })
}

/// Production chat provider hitting the native Google Gemini
/// `generateContent` API over HTTP. Mirrors [`OpenAiChatProvider`] /
/// [`AnthropicChatProvider`] but speaks the Gemini wire format. Behind the
/// `google` feature so a build without it stays network-free.
#[cfg(feature = "google")]
#[derive(Debug, Clone)]
pub struct GeminiChatProvider {
    api_key: String,
    base_url: String,
    /// Recipe id used to label results (`provider:modelId`).
    provider_id: String,
}

#[cfg(feature = "google")]
impl GeminiChatProvider {
    /// Default API root for Google's Generative Language API.
    pub const DEFAULT_BASE_URL: &'static str = "https://generativelanguage.googleapis.com";

    /// Create a provider. `base_url` should be the API root (e.g.
    /// [`Self::DEFAULT_BASE_URL`]); the model + `:generateContent` are appended
    /// per call.
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        provider_id: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_id: provider_id.into(),
        }
    }

    /// Build the request body from opts (pure; exposed for testing). Requires a
    /// resolved `model_id` since `ChatOpts.model` is `provider:model`.
    /// `opts.cache_system` is intentionally ignored: Gemini has no prompt
    /// cache (`supports_prompt_cache: false`).
    #[must_use]
    pub fn build_body(&self, opts: &ChatOpts, _model_id: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "contents": serialize_messages_gemini(&opts.messages),
            "generationConfig": {
                "maxOutputTokens": opts.max_tokens.unwrap_or(4096),
            },
        });

        if let Some(system) = &opts.system {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{ "text": system }],
            });
        }

        let tools = serialize_tools_gemini(&opts.tools);
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
        }
        body
    }
}

#[cfg(feature = "google")]
#[async_trait::async_trait]
impl ChatProvider for GeminiChatProvider {
    async fn chat(&self, opts: ChatOpts) -> Result<ChatResult, ChatError> {
        // `ChatOpts.model` is `provider:modelId`; strip the provider prefix for
        // the wire model (Gemini embeds it in the URL path).
        let model_spec = opts.model.clone().unwrap_or_default();
        let model_id = model_spec
            .split_once(':')
            .map_or_else(|| model_spec.clone(), |(_, m)| m.to_string());

        let ctx = format!("chat({}:{})", self.provider_id, model_id);
        let body = self.build_body(&opts, &model_id);
        // Gemini embeds the model in the path: /v1beta/models/{model}:generateContent
        let url = format!(
            "{}/v1beta/models/{model_id}:generateContent",
            self.base_url.trim_end_matches('/'),
        );

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            // Key rides a header, never the URL query, to keep it out of logs.
            .header("x-goog-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ChatError::normalize(None, None, &e.to_string(), Some(&ctx)))?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text =
                response.text().await.unwrap_or_else(|_| "No error details".to_string());
            return Err(ChatError::normalize(
                Some(status),
                None,
                &format!("HTTP {status}: {body_text}"),
                Some(&ctx),
            ));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ChatError::normalize(None, None, &e.to_string(), Some(&ctx)))?;

        parse_gemini_response(&json, &self.provider_id, &model_id)
    }
}

// ---- provider factory (mirrors the TS `instantiateChat` switch) ----

/// Fetch the recipe's primary API key via the injected `env_lookup`, or return
/// an `AiConfigError` carrying the recipe's setup hint. Recipes with no
/// `auth_env` (local/no-auth providers like ollama) yield an empty key, which
/// the downstream HTTP call simply sends as no `Authorization`.
#[cfg(any(feature = "openai", feature = "anthropic", feature = "google"))]
fn require_api_key(
    recipe: &Recipe,
    env_lookup: &impl Fn(&str) -> Option<String>,
) -> Result<String, AiConfigError> {
    // No auth block, or an empty required list, means a no-auth local provider.
    let Some(auth) = recipe.auth_env else {
        return Ok(String::new());
    };
    let Some(&primary) = auth.required.first() else {
        return Ok(String::new());
    };
    env_lookup(primary).ok_or_else(|| AiConfigError {
        message: format!(
            "Missing API key for {}: env var {primary} is not set.",
            recipe.name
        ),
        fix: recipe
            .setup_hint
            .map(std::string::ToString::to_string)
            .or_else(|| Some(format!("Set {primary} in your environment."))),
    })
}

/// Construct a boxed [`ChatProvider`] for a resolved recipe, dispatching on
/// `recipe.implementation` — the Rust equivalent of the TS `instantiateChat`
/// switch. This is the seam that lets the three native providers (OpenAI /
/// Anthropic / Gemini) actually reach production: the resolver validates the
/// `provider:model` string, then this factory turns the recipe into a live
/// transport.
///
/// API keys are read via the injected `env_lookup` closure (from
/// `recipe.auth_env.required[0]`) rather than `std::env` directly, keeping the
/// factory pure/testable and mirroring how the TS side threads `cfg.env`.
///
/// Native recipes carry `base_url_default: None` (the TS SDK supplies the URL
/// internally); this factory substitutes each provider's `DEFAULT_BASE_URL`.
/// `openai-compatible` recipes must carry a `base_url_default` (they have no
/// SDK default) and are served by [`OpenAiChatProvider`] over the same wire.
///
/// # Errors
/// Returns [`AiConfigError`] when the API key env var is unset, when an
/// `openai-compatible` recipe lacks a base URL, or when the provider's Cargo
/// feature (`openai` / `anthropic` / `google`) is not enabled in this build.
#[allow(unused_variables)] // `env_lookup` is unused when no provider feature is on.
pub fn instantiate_chat(
    recipe: &Recipe,
    model_id: &str,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<Box<dyn ChatProvider>, AiConfigError> {
    match recipe.implementation {
        Implementation::NativeOpenai => {
            #[cfg(feature = "openai")]
            {
                let api_key = require_api_key(recipe, &env_lookup)?;
                let base_url = recipe
                    .base_url_default
                    .unwrap_or(OpenAiChatProvider::DEFAULT_BASE_URL);
                Ok(Box::new(OpenAiChatProvider::new(api_key, base_url, recipe.id)))
            }
            #[cfg(not(feature = "openai"))]
            {
                Err(feature_disabled("openai", recipe.id))
            }
        }
        Implementation::NativeAnthropic => {
            #[cfg(feature = "anthropic")]
            {
                let api_key = require_api_key(recipe, &env_lookup)?;
                let base_url = recipe
                    .base_url_default
                    .unwrap_or(AnthropicChatProvider::DEFAULT_BASE_URL);
                Ok(Box::new(AnthropicChatProvider::new(api_key, base_url, recipe.id)))
            }
            #[cfg(not(feature = "anthropic"))]
            {
                Err(feature_disabled("anthropic", recipe.id))
            }
        }
        Implementation::NativeGoogle => {
            #[cfg(feature = "google")]
            {
                let api_key = require_api_key(recipe, &env_lookup)?;
                let base_url = recipe
                    .base_url_default
                    .unwrap_or(GeminiChatProvider::DEFAULT_BASE_URL);
                Ok(Box::new(GeminiChatProvider::new(api_key, base_url, recipe.id)))
            }
            #[cfg(not(feature = "google"))]
            {
                Err(feature_disabled("google", recipe.id))
            }
        }
        Implementation::OpenaiCompatible => {
            #[cfg(feature = "openai")]
            {
                let api_key = require_api_key(recipe, &env_lookup)?;
                let Some(base_url) = recipe.base_url_default else {
                    return Err(AiConfigError {
                        message: format!(
                            "openai-compatible recipe \"{}\" has no base URL.",
                            recipe.id
                        ),
                        fix: Some(
                            "Set base_url_default on the recipe, or configure the provider's endpoint env var."
                                .to_string(),
                        ),
                    });
                };
                Ok(Box::new(OpenAiChatProvider::new(api_key, base_url, recipe.id)))
            }
            #[cfg(not(feature = "openai"))]
            {
                Err(feature_disabled("openai", recipe.id))
            }
        }
    }
}

/// Build the `AiConfigError` returned when a provider's Cargo feature is not
/// compiled into this build.
#[allow(dead_code)] // used only in the `not(feature=...)` factory arms.
fn feature_disabled(feature: &str, recipe_id: &str) -> AiConfigError {
    AiConfigError {
        message: format!(
            "Chat provider for \"{recipe_id}\" requires the `{feature}` feature, which is not enabled in this build."
        ),
        fix: Some(format!(
            "Rebuild zbrain-core with `--features {feature}` to enable this provider."
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call_block() -> ChatBlock {
        ChatBlock::ToolCall {
            tool_call_id: "call_1".to_string(),
            tool_name: "search".to_string(),
            input: serde_json::json!({"query": "rust"}),
        }
    }

    // ---- StopReason mapping (mirrors mapStopReason gateway.ts:2261) ----

    #[test]
    fn stop_reason_anthropic_refusal_wins() {
        assert_eq!(
            StopReason::from_signals(Some("stop"), Some("refusal")),
            StopReason::Refusal
        );
    }

    #[test]
    fn stop_reason_content_filter_both_spellings() {
        assert_eq!(StopReason::from_signals(Some("content-filter"), None), StopReason::ContentFilter);
        assert_eq!(StopReason::from_signals(Some("content_filter"), None), StopReason::ContentFilter);
    }

    #[test]
    fn stop_reason_tool_calls_both_spellings() {
        assert_eq!(StopReason::from_signals(Some("tool-calls"), None), StopReason::ToolCalls);
        assert_eq!(StopReason::from_signals(Some("tool_calls"), None), StopReason::ToolCalls);
    }

    #[test]
    fn stop_reason_length_and_end_and_other() {
        assert_eq!(StopReason::from_signals(Some("length"), None), StopReason::Length);
        assert_eq!(StopReason::from_signals(Some("max-tokens"), None), StopReason::Length);
        assert_eq!(StopReason::from_signals(Some("stop"), None), StopReason::End);
        assert_eq!(StopReason::from_signals(Some("end-turn"), None), StopReason::End);
        assert_eq!(StopReason::from_signals(Some("weird"), None), StopReason::Other);
        assert_eq!(StopReason::from_signals(None, None), StopReason::Other);
    }

    // ---- ChatError::normalize (mirrors normalizeAIError errors.ts:44) ----

    #[test]
    fn normalize_401_403_gives_api_key_hint_config() {
        let e = ChatError::normalize(Some(401), None, "unauthorized", Some("chat(openai:gpt)"));
        match e {
            ChatError::Config { message, fix } => {
                assert!(message.starts_with("[chat(openai:gpt)] "));
                assert_eq!(fix.as_deref(), Some("Check your API key is valid and has access to this model."));
            }
            _ => panic!("expected Config"),
        }
        assert!(!ChatError::normalize(Some(403), None, "x", None).is_transient());
    }

    #[test]
    fn normalize_other_4xx_gives_model_hint_config() {
        let e = ChatError::normalize(Some(404), None, "no such model", None);
        match e {
            ChatError::Config { fix, .. } => {
                assert_eq!(fix.as_deref(), Some("Check your model id + provider options match the provider API."));
            }
            _ => panic!("expected Config"),
        }
    }

    #[test]
    fn normalize_429_is_transient_not_config() {
        assert!(ChatError::normalize(Some(429), None, "rate limited", None).is_transient());
    }

    #[test]
    fn normalize_5xx_and_network_are_transient() {
        assert!(ChatError::normalize(Some(500), None, "server error", None).is_transient());
        assert!(ChatError::normalize(None, None, "connection reset", None).is_transient());
    }

    #[test]
    fn normalize_named_config_errors() {
        assert!(matches!(
            ChatError::normalize(None, Some("LoadAPIKeyError"), "missing key", None),
            ChatError::Config { fix: None, .. }
        ));
        assert!(matches!(
            ChatError::normalize(None, Some("InvalidArgumentError"), "bad arg", None),
            ChatError::Config { fix: None, .. }
        ));
    }

    #[test]
    fn chat_error_display_with_and_without_fix() {
        assert_eq!(
            ChatError::Config { message: "bad".to_string(), fix: Some("do x".to_string()) }.to_string(),
            "bad \u{2014} do x"
        );
        assert_eq!(
            ChatError::Transient { message: "blip".to_string() }.to_string(),
            "blip"
        );
    }

    // ---- MockChatProvider ----

    #[tokio::test]
    async fn mock_returns_queued_then_default() {
        let m = MockChatProvider::new("fallback answer");
        m.queue_text("first");
        assert_eq!(m.chat(ChatOpts::default()).await.unwrap().text, "first");
        assert_eq!(m.chat(ChatOpts::default()).await.unwrap().text, "fallback answer");
    }

    #[tokio::test]
    async fn mock_queue_error() {
        let m = MockChatProvider::new("x");
        m.queue_error(ChatError::Transient { message: "boom".to_string() });
        assert!(m.chat(ChatOpts::default()).await.unwrap_err().is_transient());
    }

    #[tokio::test]
    async fn mock_queue_result_with_tool_call() {
        let m = MockChatProvider::new("x");
        m.queue_result(ChatResult {
            text: String::new(),
            blocks: vec![tool_call_block()],
            stop_reason: StopReason::ToolCalls,
            usage: ChatUsage::default(),
            model: "mock:m".to_string(),
            provider_id: "mock".to_string(),
            provider_metadata: None,
        });
        let r = m.chat(ChatOpts::default()).await.unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolCalls);
        assert_eq!(r.blocks.len(), 1);
    }

    // ---- serialize_messages_openai ----

    #[test]
    fn serialize_plain_text_messages() {
        let msgs = vec![
            ChatMessage::text(ChatRole::System, "be helpful"),
            ChatMessage::text(ChatRole::User, "hi"),
        ];
        let out = serialize_messages_openai(&msgs);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["content"], "be helpful");
        assert_eq!(out[1]["role"], "user");
    }

    #[test]
    fn serialize_assistant_tool_call_block() {
        let msgs = vec![ChatMessage {
            role: ChatRole::Assistant,
            content: ChatContent::Blocks(vec![tool_call_block()]),
        }];
        let out = serialize_messages_openai(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "search");
        assert!(out[0]["tool_calls"][0]["function"]["arguments"].is_string());
    }

    #[test]
    fn serialize_tool_result_becomes_tool_message() {
        let msgs = vec![ChatMessage {
            role: ChatRole::Tool,
            content: ChatContent::Blocks(vec![ChatBlock::ToolResult {
                tool_call_id: "call_1".to_string(),
                tool_name: "search".to_string(),
                output: serde_json::json!({"result": "ok"}),
                is_error: false,
            }]),
        }];
        let out = serialize_messages_openai(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "call_1");
        assert!(out[0]["content"].is_string());
    }

    // ---- serialize_tools_openai ----

    #[test]
    fn serialize_tools_shape() {
        let tools = vec![ChatToolDef {
            name: "search".to_string(),
            description: "search the web".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let out = serialize_tools_openai(&tools);
        assert_eq!(out[0]["type"], "function");
        assert_eq!(out[0]["function"]["name"], "search");
        assert_eq!(out[0]["function"]["parameters"]["type"], "object");
    }

    // ---- parse_openai_response ----

    #[test]
    fn parse_text_response() {
        let json = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "hello there"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17}
        });
        let r = parse_openai_response(&json, "openai", "gpt-4o-mini").unwrap();
        assert_eq!(r.text, "hello there");
        assert_eq!(r.blocks.len(), 1);
        assert_eq!(r.stop_reason, StopReason::End);
        assert_eq!(r.usage.input_tokens, 12);
        assert_eq!(r.usage.output_tokens, 5);
        assert_eq!(r.model, "openai:gpt-4o-mini");
        assert_eq!(r.provider_id, "openai");
    }

    #[test]
    fn parse_tool_call_response() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "search", "arguments": "{\"query\":\"rust\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 8}
        });
        let r = parse_openai_response(&json, "openai", "gpt-4o").unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolCalls);
        assert_eq!(r.blocks.len(), 1);
        match &r.blocks[0] {
            ChatBlock::ToolCall { tool_call_id, tool_name, input } => {
                assert_eq!(tool_call_id, "call_abc");
                assert_eq!(tool_name, "search");
                assert_eq!(input["query"], "rust");
            }
            _ => panic!("expected tool-call block"),
        }
    }

    #[test]
    fn parse_missing_choices_is_transient() {
        let json = serde_json::json!({"usage": {}});
        assert!(parse_openai_response(&json, "openai", "gpt").unwrap_err().is_transient());
    }

    // ---- Anthropic stop_reason full-value mapping (Part6 1-4-1) ----

    #[test]
    fn stop_reason_anthropic_tool_use_maps_to_tool_calls() {
        // Critical: tool_use must drive tool_loop continuation, not fall to Other.
        assert_eq!(StopReason::from_signals(None, Some("tool_use")), StopReason::ToolCalls);
    }

    #[test]
    fn stop_reason_anthropic_end_turn_and_stop_sequence_map_to_end() {
        assert_eq!(StopReason::from_signals(None, Some("end_turn")), StopReason::End);
        assert_eq!(StopReason::from_signals(None, Some("stop_sequence")), StopReason::End);
    }

    #[test]
    fn stop_reason_anthropic_max_tokens_maps_to_length() {
        assert_eq!(StopReason::from_signals(None, Some("max_tokens")), StopReason::Length);
    }

    #[test]
    fn stop_reason_anthropic_refusal_still_wins() {
        assert_eq!(StopReason::from_signals(Some("stop"), Some("refusal")), StopReason::Refusal);
    }

    // ---- serialize_messages_anthropic ----

    #[test]
    fn anthropic_serializes_text_message_as_string() {
        let msgs = vec![ChatMessage::text(ChatRole::User, "hello")];
        let out = serialize_messages_anthropic(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], "hello");
    }

    #[test]
    fn anthropic_serializes_assistant_text_and_tool_use_as_blocks() {
        let msgs = vec![ChatMessage {
            role: ChatRole::Assistant,
            content: ChatContent::Blocks(vec![
                ChatBlock::Text { text: "let me search".to_string() },
                ChatBlock::ToolCall {
                    tool_call_id: "toolu_1".to_string(),
                    tool_name: "search".to_string(),
                    input: serde_json::json!({"query": "rust"}),
                },
            ]),
        }];
        let out = serialize_messages_anthropic(&msgs);
        assert_eq!(out[0]["role"], "assistant");
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "let me search");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_1");
        assert_eq!(content[1]["name"], "search");
        // input stays a JSON object (not stringified like OpenAI).
        assert_eq!(content[1]["input"]["query"], "rust");
    }

    #[test]
    fn anthropic_serializes_tool_result_in_user_message() {
        let msgs = vec![ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Blocks(vec![ChatBlock::ToolResult {
                tool_call_id: "toolu_1".to_string(),
                tool_name: "search".to_string(),
                output: serde_json::json!("found 3 results"),
                is_error: false,
            }]),
        }];
        let out = serialize_messages_anthropic(&msgs);
        assert_eq!(out[0]["role"], "user");
        let tr = &out[0]["content"][0];
        assert_eq!(tr["type"], "tool_result");
        assert_eq!(tr["tool_use_id"], "toolu_1");
        // String output stays as its inner text.
        assert_eq!(tr["content"], "found 3 results");
        // is_error omitted when false.
        assert!(tr.get("is_error").is_none());
    }

    #[test]
    fn anthropic_tool_result_error_sets_is_error_and_stringifies_object() {
        let msgs = vec![ChatMessage {
            role: ChatRole::Tool,
            content: ChatContent::Blocks(vec![ChatBlock::ToolResult {
                tool_call_id: "toolu_2".to_string(),
                tool_name: "run".to_string(),
                output: serde_json::json!({"code": 1, "msg": "boom"}),
                is_error: true,
            }]),
        }];
        let out = serialize_messages_anthropic(&msgs);
        // Tool role maps to wire "user".
        assert_eq!(out[0]["role"], "user");
        let tr = &out[0]["content"][0];
        assert_eq!(tr["is_error"], true);
        // Object output is stringified to compact JSON.
        assert_eq!(tr["content"], r#"{"code":1,"msg":"boom"}"#);
    }

    #[test]
    fn anthropic_skips_stray_system_message() {
        let msgs = vec![
            ChatMessage::text(ChatRole::System, "sys"),
            ChatMessage::text(ChatRole::User, "hi"),
        ];
        let out = serialize_messages_anthropic(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["content"], "hi");
    }

    // ---- serialize_tools_anthropic ----

    #[test]
    fn anthropic_tools_no_function_wrapper() {
        let tools = vec![ChatToolDef {
            name: "search".to_string(),
            description: "search docs".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let out = serialize_tools_anthropic(&tools, false);
        assert_eq!(out[0]["name"], "search");
        assert_eq!(out[0]["description"], "search docs");
        assert_eq!(out[0]["input_schema"]["type"], "object");
        // No {type:function} wrapper, no cache_control when cache_last=false.
        assert!(out[0].get("type").is_none());
        assert!(out[0].get("cache_control").is_none());
    }

    #[test]
    fn anthropic_tools_cache_control_on_last_only() {
        let tools = vec![
            ChatToolDef {
                name: "a".to_string(),
                description: "first".to_string(),
                input_schema: serde_json::json!({}),
            },
            ChatToolDef {
                name: "b".to_string(),
                description: "last".to_string(),
                input_schema: serde_json::json!({}),
            },
        ];
        let out = serialize_tools_anthropic(&tools, true);
        assert!(out[0].get("cache_control").is_none());
        assert_eq!(out[1]["cache_control"]["type"], "ephemeral");
    }

    // ---- parse_anthropic_response ----

    #[test]
    fn parse_anthropic_text_and_usage_with_cache() {
        let json = serde_json::json!({
            "content": [{"type": "text", "text": "hello world"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_input_tokens": 100,
                "cache_creation_input_tokens": 20
            }
        });
        let r = parse_anthropic_response(&json, "anthropic", "claude-sonnet-4-6").unwrap();
        assert_eq!(r.text, "hello world");
        assert_eq!(r.stop_reason, StopReason::End);
        assert_eq!(r.usage.input_tokens, 10);
        assert_eq!(r.usage.output_tokens, 5);
        assert_eq!(r.usage.cache_read_tokens, 100);
        assert_eq!(r.usage.cache_creation_tokens, 20);
        assert_eq!(r.model, "anthropic:claude-sonnet-4-6");
    }

    #[test]
    fn parse_anthropic_tool_use_block() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "searching"},
                {"type": "tool_use", "id": "toolu_9", "name": "search",
                 "input": {"query": "rust"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 8, "output_tokens": 4}
        });
        let r = parse_anthropic_response(&json, "anthropic", "claude-sonnet-4-6").unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolCalls);
        assert_eq!(r.blocks.len(), 2);
        match &r.blocks[1] {
            ChatBlock::ToolCall { tool_call_id, tool_name, input } => {
                assert_eq!(tool_call_id, "toolu_9");
                assert_eq!(tool_name, "search");
                assert_eq!(input["query"], "rust");
            }
            _ => panic!("expected tool-call block"),
        }
    }

    #[test]
    fn parse_anthropic_missing_content_is_transient() {
        let json = serde_json::json!({"usage": {}});
        assert!(parse_anthropic_response(&json, "anthropic", "c").unwrap_err().is_transient());
    }

    // ---- build_body (feature-gated: needs AnthropicChatProvider) ----

    #[cfg(feature = "anthropic")]
    #[test]
    fn anthropic_build_body_plain_system_without_cache() {
        let provider = AnthropicChatProvider::new("k", "https://api.anthropic.com", "anthropic");
        let opts = ChatOpts {
            model: Some("anthropic:claude-sonnet-4-6".to_string()),
            system: Some("you are helpful".to_string()),
            messages: vec![ChatMessage::text(ChatRole::User, "hi")],
            tools: vec![],
            max_tokens: None,
            cache_system: false,
        };
        let body = provider.build_body(&opts, "claude-sonnet-4-6");
        // Plain string system, mandatory max_tokens defaulted to 4096.
        assert_eq!(body["system"], "you are helpful");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["model"], "claude-sonnet-4-6");
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn anthropic_build_body_cache_system_uses_array_block() {
        let provider = AnthropicChatProvider::new("k", "https://api.anthropic.com", "anthropic");
        let opts = ChatOpts {
            model: Some("anthropic:claude-sonnet-4-6".to_string()),
            system: Some("cached prompt".to_string()),
            messages: vec![ChatMessage::text(ChatRole::User, "hi")],
            tools: vec![ChatToolDef {
                name: "t".to_string(),
                description: "d".to_string(),
                input_schema: serde_json::json!({}),
            }],
            max_tokens: Some(1024),
            cache_system: true,
        };
        let body = provider.build_body(&opts, "claude-sonnet-4-6");
        // Array-form system with cache_control.
        assert_eq!(body["system"][0]["type"], "text");
        assert_eq!(body["system"][0]["text"], "cached prompt");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["max_tokens"], 1024);
        // Last (only) tool def cached.
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    }

    // ---- serialize_messages_gemini (slice 1-4-2) ----

    #[test]
    fn gemini_serialize_text_message_single_part() {
        let msgs = vec![ChatMessage::text(ChatRole::User, "hello")];
        let out = serialize_messages_gemini(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["parts"][0]["text"], "hello");
    }

    #[test]
    fn gemini_serialize_assistant_maps_to_model_role() {
        let msgs = vec![ChatMessage {
            role: ChatRole::Assistant,
            content: ChatContent::Blocks(vec![
                ChatBlock::Text { text: "thinking".to_string() },
                tool_call_block(),
            ]),
        }];
        let out = serialize_messages_gemini(&msgs);
        assert_eq!(out[0]["role"], "model");
        assert_eq!(out[0]["parts"][0]["text"], "thinking");
        // functionCall carries name + args, no id on the wire.
        assert_eq!(out[0]["parts"][1]["functionCall"]["name"], "search");
        assert_eq!(out[0]["parts"][1]["functionCall"]["args"]["query"], "rust");
        assert!(out[0]["parts"][1]["functionCall"].get("id").is_none());
    }

    #[test]
    fn gemini_serialize_tool_result_is_function_response_user_role() {
        let msgs = vec![ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Blocks(vec![ChatBlock::ToolResult {
                tool_call_id: "search-0".to_string(),
                tool_name: "search".to_string(),
                output: serde_json::json!({"hits": 3}),
                is_error: false,
            }]),
        }];
        let out = serialize_messages_gemini(&msgs);
        assert_eq!(out[0]["role"], "user");
        let fr = &out[0]["parts"][0]["functionResponse"];
        // Correlated by name (no id); object response passes through untouched.
        assert_eq!(fr["name"], "search");
        assert_eq!(fr["response"]["hits"], 3);
    }

    #[test]
    fn gemini_tool_result_non_object_output_wrapped_in_result() {
        let msgs = vec![ChatMessage {
            role: ChatRole::Tool,
            content: ChatContent::Blocks(vec![ChatBlock::ToolResult {
                tool_call_id: "calc-0".to_string(),
                tool_name: "calc".to_string(),
                // A bare string is not a JSON object -> must be boxed.
                output: serde_json::json!("42"),
                is_error: true,
            }]),
        }];
        let out = serialize_messages_gemini(&msgs);
        let fr = &out[0]["parts"][0]["functionResponse"];
        assert_eq!(fr["response"]["result"], "42");
        // Gemini has no is_error slot; it must not leak into the part.
        assert!(out[0]["parts"][0].get("is_error").is_none());
    }

    #[test]
    fn gemini_serialize_skips_system_message() {
        let msgs = vec![
            ChatMessage::text(ChatRole::System, "you are helpful"),
            ChatMessage::text(ChatRole::User, "hi"),
        ];
        let out = serialize_messages_gemini(&msgs);
        // System is lifted into systemInstruction, never a content entry.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
    }

    // ---- serialize_tools_gemini ----

    #[test]
    fn gemini_tools_wrapped_in_function_declarations() {
        let tools = vec![
            ChatToolDef {
                name: "a".to_string(),
                description: "first".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            ChatToolDef {
                name: "b".to_string(),
                description: "second".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
        ];
        let out = serialize_tools_gemini(&tools);
        // Single wrapper entry holding all declarations (no per-tool wrapper).
        assert_eq!(out.len(), 1);
        let decls = out[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0]["name"], "a");
        // input_schema maps to `parameters`.
        assert_eq!(decls[0]["parameters"]["type"], "object");
        assert_eq!(decls[1]["name"], "b");
    }

    #[test]
    fn gemini_tools_empty_is_empty_vec() {
        assert!(serialize_tools_gemini(&[]).is_empty());
    }

    // ---- parse_gemini_response ----

    #[test]
    fn parse_gemini_text_and_usage() {
        let json = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "hi there" }] },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 12,
                "candidatesTokenCount": 5,
                "cachedContentTokenCount": 4
            }
        });
        let r = parse_gemini_response(&json, "google", "gemini-2.0-flash").unwrap();
        assert_eq!(r.text, "hi there");
        assert_eq!(r.stop_reason, StopReason::End);
        assert_eq!(r.usage.input_tokens, 12);
        assert_eq!(r.usage.output_tokens, 5);
        assert_eq!(r.usage.cache_read_tokens, 4);
        assert_eq!(r.usage.cache_creation_tokens, 0);
        assert_eq!(r.model, "google:gemini-2.0-flash");
    }

    #[test]
    fn parse_gemini_function_call_overrides_stop_to_tool_calls() {
        // Gemini reports STOP even when requesting a tool; parse must override.
        let json = serde_json::json!({
            "candidates": [{
                "content": { "parts": [
                    { "functionCall": { "name": "search", "args": { "q": "x" } } }
                ]},
                "finishReason": "STOP"
            }]
        });
        let r = parse_gemini_response(&json, "google", "gemini-2.0-flash").unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolCalls);
        assert_eq!(r.blocks.len(), 1);
        match &r.blocks[0] {
            ChatBlock::ToolCall { tool_call_id, tool_name, input } => {
                // Synthesized {name}-{index} id (index 0).
                assert_eq!(tool_call_id, "search-0");
                assert_eq!(tool_name, "search");
                assert_eq!(input["q"], "x");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_gemini_max_tokens_and_safety_finish_reasons() {
        let mk = |reason: &str| {
            serde_json::json!({
                "candidates": [{
                    "content": { "parts": [{ "text": "x" }] },
                    "finishReason": reason
                }]
            })
        };
        assert_eq!(
            parse_gemini_response(&mk("MAX_TOKENS"), "google", "m").unwrap().stop_reason,
            StopReason::Length
        );
        assert_eq!(
            parse_gemini_response(&mk("SAFETY"), "google", "m").unwrap().stop_reason,
            StopReason::ContentFilter
        );
        assert_eq!(
            parse_gemini_response(&mk("RECITATION"), "google", "m").unwrap().stop_reason,
            StopReason::ContentFilter
        );
        assert_eq!(
            parse_gemini_response(&mk("WEIRD"), "google", "m").unwrap().stop_reason,
            StopReason::Other
        );
    }

    #[test]
    fn parse_gemini_missing_candidates_is_transient() {
        let json = serde_json::json!({ "usageMetadata": {} });
        assert!(parse_gemini_response(&json, "google", "m").unwrap_err().is_transient());
    }

    #[test]
    fn parse_gemini_missing_parts_is_transient() {
        let json = serde_json::json!({ "candidates": [{ "finishReason": "STOP" }] });
        assert!(parse_gemini_response(&json, "google", "m").unwrap_err().is_transient());
    }

    #[cfg(feature = "google")]
    #[test]
    fn gemini_build_body_maps_system_and_tools() {
        let provider = GeminiChatProvider::new("k", "https://example.test", "google");
        let opts = ChatOpts {
            model: Some("google:gemini-2.0-flash".to_string()),
            system: Some("be terse".to_string()),
            messages: vec![ChatMessage::text(ChatRole::User, "hi")],
            tools: vec![ChatToolDef {
                name: "t".to_string(),
                description: "d".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            max_tokens: Some(2048),
            // Gemini has no prompt cache; this flag must be ignored (no panic,
            // no cache_control in the body).
            cache_system: true,
        };
        let body = provider.build_body(&opts, "gemini-2.0-flash");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be terse");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 2048);
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "t");
        // No cache_control anywhere despite cache_system=true.
        assert!(body.to_string().find("cache_control").is_none());
    }

    #[cfg(feature = "google")]
    #[test]
    fn gemini_build_body_omits_optional_fields_when_empty() {
        let provider = GeminiChatProvider::new("k", GeminiChatProvider::DEFAULT_BASE_URL, "google");
        let opts = ChatOpts {
            model: Some("google:gemini-2.0-flash".to_string()),
            messages: vec![ChatMessage::text(ChatRole::User, "hi")],
            ..Default::default()
        };
        let body = provider.build_body(&opts, "gemini-2.0-flash");
        assert!(body.get("systemInstruction").is_none());
        assert!(body.get("tools").is_none());
        // Default max tokens still present.
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 4096);
    }

    // ---- instantiate_chat factory ----

    fn openai_recipe() -> &'static Recipe {
        crate::ai::resolve_recipe("openai").unwrap()
    }

    fn always_key(_: &str) -> Option<String> {
        Some("test-key".to_string())
    }

    fn no_key(_: &str) -> Option<String> {
        None
    }

    #[cfg(feature = "openai")]
    #[test]
    fn factory_builds_openai_native() {
        let recipe = openai_recipe();
        let p = instantiate_chat(recipe, "gpt-5.2", always_key).expect("openai provider built");
        // Debug output identifies the concrete provider type.
        assert!(format!("{p:?}").contains("OpenAiChatProvider"));
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn factory_builds_anthropic_native() {
        let recipe = crate::ai::resolve_recipe("anthropic").unwrap();
        let p = instantiate_chat(recipe, "claude-haiku-4-5-20251001", always_key)
            .expect("anthropic provider built");
        assert!(format!("{p:?}").contains("AnthropicChatProvider"));
    }

    #[cfg(feature = "google")]
    #[test]
    fn factory_builds_google_native() {
        let recipe = crate::ai::resolve_recipe("google").unwrap();
        let p = instantiate_chat(recipe, "gemini-2.0-flash", always_key)
            .expect("google provider built");
        assert!(format!("{p:?}").contains("GeminiChatProvider"));
    }

    #[cfg(feature = "openai")]
    #[test]
    fn factory_builds_openai_compatible_via_openai_provider() {
        // deepseek is an openai-compatible recipe with a base_url_default.
        let recipe = crate::ai::resolve_recipe("deepseek").unwrap();
        let p = instantiate_chat(recipe, "deepseek-chat", always_key)
            .expect("openai-compat provider built");
        assert!(format!("{p:?}").contains("OpenAiChatProvider"));
    }

    #[cfg(feature = "openai")]
    #[test]
    fn factory_missing_key_errors_with_setup_hint() {
        let recipe = openai_recipe();
        let e = instantiate_chat(recipe, "gpt-5.2", no_key).unwrap_err();
        assert!(e.message.contains("Missing API key"));
        assert!(e.fix.is_some());
    }

    #[cfg(not(feature = "openai"))]
    #[test]
    fn factory_openai_feature_disabled_errors() {
        let recipe = openai_recipe();
        let e = instantiate_chat(recipe, "gpt-5.2", always_key).unwrap_err();
        assert!(e.message.contains("requires the `openai` feature"));
        assert!(e.fix.unwrap().contains("--features openai"));
    }

    #[cfg(not(feature = "anthropic"))]
    #[test]
    fn factory_anthropic_feature_disabled_errors() {
        let recipe = crate::ai::resolve_recipe("anthropic").unwrap();
        let e = instantiate_chat(recipe, "claude-haiku-4-5-20251001", always_key).unwrap_err();
        assert!(e.message.contains("requires the `anthropic` feature"));
    }

    #[cfg(not(feature = "google"))]
    #[test]
    fn factory_google_feature_disabled_errors() {
        let recipe = crate::ai::resolve_recipe("google").unwrap();
        let e = instantiate_chat(recipe, "gemini-2.0-flash", always_key).unwrap_err();
        assert!(e.message.contains("requires the `google` feature"));
    }
}
