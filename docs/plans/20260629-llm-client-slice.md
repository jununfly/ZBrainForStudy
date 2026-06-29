# Slice #57-58: LLM Client Interface (June 29, 2026)

## Completed: Issue #57 — LLM Client Mock Interface

- `LlmClient` trait (async-trait) in `crates/zbrain-core/src/llm.rs`
- `LlmRequest` struct: system_prompt, user_prompt, context, max_tokens, temperature
- `LlmResponse` struct: content, tokens_used, model
- `TokenUsage` struct: prompt, completion, total
- `LlmError` enum: AuthError, RateLimited, ModelNotFound, NetworkError, ParseError, InvalidRequest, Timeout
- `MockLlmClient`: thread-safe with `Arc<Mutex>`, supports queue_success() and queue_error()
- 5 tests: default response, context awareness, queued responses, error responses, error Display

## Completed: Issue #58 — Real LLM Client (OpenAI)

- `OpenAiClient` behind `openai` feature flag
- Environment-based config: `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_MODEL`, `OPENAI_ORG_ID`, `OPENAI_PROJECT`
- Builder pattern: `new()`, `with_base_url()`, `with_org_id()`, `with_project_id()`
- Context injection: appends retrieved document snippets to user prompt
- Full error mapping from HTTP status codes to `LlmError`
- Token usage parsing from OpenAI response
- Re-exported at crate root: `LlmClient`, `LlmRequest`, `LlmResponse`, `LlmError`, `MockLlmClient`, `TokenUsage`; `OpenAiClient` conditional on feature

## Design Decisions

1. **Feature gating**: OpenAI client is optional (`openai` feature)
2. **Thread safety**: Mock client uses `Arc<Mutex<...>>` for interior mutability
3. **Provider-compatible interface**: Same trait works for OpenAI, Azure, Groq, local LLMs, etc.
4. **Context handling**: Context snippets are appended to user prompt per simple RAG pattern (v1)
5. **Zero external dependencies in default build**: No `reqwest` when `openai` feature is off

## Next Steps (Future Slices)

- Integrate `LlmClient` into `ThinkOperation::execute()` 
- Wire `OperationContext` to carry a configured LLM client instance
- Add page retrieval (keyword → SQL search → page snippets) before LLM call
- Add source citations to `ThinkOutput`
- Add streaming support (SSE for web/MCP)
- Add retry logic with backoff for rate limits
- Add more providers: Anthropic, Google Gemini, Ollama
