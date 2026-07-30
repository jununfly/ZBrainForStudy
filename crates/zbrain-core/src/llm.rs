//! LLM client interface for Think operation integration.
//!
//! Provides a unified interface for interacting with language models,
//! with mock implementations for testing and development.

use std::collections::VecDeque;
use std::sync::Mutex;

#[cfg(feature = "openai")]
use std::env;

/// LLM request structure.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// System prompt (optional)
    pub system_prompt: Option<String>,
    /// User prompt / query
    pub user_prompt: String,
    /// Context documents (e.g., retrieved page snippets)
    pub context: Vec<String>,
    /// Maximum tokens in response
    pub max_tokens: Option<u32>,
    /// Sampling temperature
    pub temperature: Option<f32>,
}

/// LLM response structure.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// Generated content
    pub content: String,
    /// Token usage statistics
    pub tokens_used: TokenUsage,
    /// Model name that generated the response
    pub model: String,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Prompt tokens (input)
    pub prompt: u32,
    /// Completion tokens (output)
    pub completion: u32,
    /// Total tokens used
    pub total: u32,
}

/// LLM error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmError {
    /// Network or connectivity issue
    NetworkError(String),
    /// Authentication failure (invalid API key)
    AuthError(String),
    /// Rate limit exceeded
    RateLimited { retry_after_seconds: Option<u32> },
    /// Model not found or unavailable
    ModelNotFound(String),
    /// Invalid request parameters
    InvalidRequest(String),
    /// Response parsing failed
    ParseError(String),
    /// Timeout
    Timeout,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::NetworkError(msg) => write!(f, "Network error: {}", msg),
            LlmError::AuthError(msg) => write!(f, "Auth error: {}", msg),
            LlmError::RateLimited { retry_after_seconds } => {
                write!(f, "Rate limited")?;
                if let Some(seconds) = retry_after_seconds {
                    write!(f, " (retry after {}s)", seconds)?;
                }
                Ok(())
            }
            LlmError::ModelNotFound(model) => write!(f, "Model not found: {}", model),
            LlmError::InvalidRequest(msg) => write!(f, "Invalid request: {}", msg),
            LlmError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            LlmError::Timeout => write!(f, "Request timeout"),
        }
    }
}

impl std::error::Error for LlmError {}

/// LLM client trait.
#[async_trait::async_trait]
pub trait LlmClient: std::fmt::Debug + Send + Sync {
    /// Generate a completion from the LLM.
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError>;
}

/// Think operation structured output.
///
/// Parsed from LLM JSON response containing answer and citations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThinkOutput {
    /// Generated answer
    pub answer: String,
    /// Warning messages (e.g., model fallback, truncated context)
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Number of evidence snippets used
    #[serde(default)]
    pub evidence_used: u32,
    /// Source page slugs that contributed to the answer
    #[serde(default)]
    pub sources: Vec<String>,
    /// Structured citations (TS `ThinkResponse.citations`). Empty when the
    /// model omitted the field — callers should run [`resolve_citations`]
    /// to fall back to inline-marker scanning.
    #[serde(default)]
    pub citations: Vec<Citation>,
}

/// Prompt builder for Think operation.
///
/// Constructs system + user prompts with context injection and JSON
/// output schema instructions.
#[derive(Debug, Clone)]
pub struct ThinkPromptBuilder {
    question: String,
    context: Vec<String>,
    sources: Vec<String>,
}

impl ThinkPromptBuilder {
    /// Create a new prompt builder for a question.
    pub fn new(question: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            context: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// Add a context snippet with its source.
    pub fn add_context(&mut self, snippet: impl Into<String>, source: impl Into<String>) {
        self.context.push(snippet.into());
        self.sources.push(source.into());
    }

    /// Build the system prompt with JSON schema instructions.
    pub fn build_system_prompt(&self) -> String {
        r#"You are a helpful code documentation assistant. Answer the user's question based ONLY on the provided context snippets.

Your response MUST be a valid JSON object with exactly these fields:
- "answer": string - Your answer to the question, concise and accurate
- "warnings": array of strings - Any warnings about missing context or limitations
- "evidence_used": number - How many context snippets you used (0 if none)
- "sources": array of strings - The slugs of sources you cited

Rules:
1. Only use information explicitly provided in the context
2. If no relevant context exists, set evidence_used to 0 and sources to empty
3. Keep answer concise, under 300 words
4. Respond with ONLY the JSON object, no markdown code blocks, no extra text
5. Do not include any explanations outside the JSON

Example valid response:
{"answer":"ZBrain is a Rust rewrite of GBrain.","warnings":["Some newer APIs may not be documented"],"evidence_used":2,"sources":["rust-migration","zbrain-architecture"]}"#.to_string()
    }

    /// Build the user prompt with context snippets.
    pub fn build_user_prompt(&self) -> String {
        let mut prompt = format!("Question: {}\n\n", self.question);
        
        if !self.context.is_empty() {
            prompt.push_str("Context snippets (cite these in your answer):\n\n");
            for (i, (ctx, source)) in self.context.iter().zip(self.sources.iter()).enumerate() {
                prompt.push_str(&format!("[{}] Source: {}\n{}\n\n", i + 1, source, ctx));
            }
        } else {
            prompt.push_str("No context snippets available. Answer based on general knowledge but note the lack of specific context in warnings.\n\n");
        }
        
        prompt.push_str("Respond with JSON only.");
        prompt
    }

    /// Build a complete LLM request from this builder.
    pub fn build_request(&self) -> LlmRequest {
        LlmRequest {
            system_prompt: Some(self.build_system_prompt()),
            user_prompt: self.build_user_prompt(),
            context: self.context.clone(),
            max_tokens: Some(500),
            temperature: Some(0.2),
        }
    }

    /// Parse an LLM response string into a ThinkOutput.
    pub fn parse_response(content: &str) -> Result<ThinkOutput, LlmError> {
        // Try to extract JSON from markdown code blocks if present
        let json_str = if content.contains("```json") {
            content
                .split("```json")
                .nth(1)
                .and_then(|s| s.split("```").next())
                .unwrap_or(content)
                .trim()
        } else if content.contains("```") {
            content
                .split("```")
                .nth(1)
                .unwrap_or(content)
                .trim()
        } else {
            content.trim()
        };

        serde_json::from_str(json_str)
            .map_err(|e| LlmError::ParseError(format!("Failed to parse ThinkOutput JSON: {}", e)))
    }
}

// Internal mock client state
#[derive(Debug, Clone, Default)]
struct MockLlmClientState {
    responses: VecDeque<Result<LlmResponse, LlmError>>,
    default_response: String,
}

/// Mock LLM client for testing.
///
/// Returns canned responses based on request patterns.
/// Can be configured to return specific responses or errors.
#[derive(Debug, Clone)]
pub struct MockLlmClient {
    state: std::sync::Arc<Mutex<MockLlmClientState>>,
}

impl Default for MockLlmClient {
    fn default() -> Self {
        Self {
            state: std::sync::Arc::new(Mutex::new(MockLlmClientState::default())),
        }
    }
}

impl MockLlmClient {
    /// Create a new mock client with a default response.
    pub fn new(default_response: impl Into<String>) -> Self {
        Self {
            state: std::sync::Arc::new(Mutex::new(MockLlmClientState {
                responses: VecDeque::new(),
                default_response: default_response.into(),
            })),
        }
    }

    /// Queue a successful response to return.
    pub fn queue_success(&self, content: impl Into<String>) {
        self.state.lock().unwrap().responses.push_back(Ok(LlmResponse {
            content: content.into(),
            tokens_used: TokenUsage {
                prompt: 10,
                completion: 20,
                total: 30,
            },
            model: "mock-model".to_string(),
        }));
    }

    /// Queue an error response to return.
    pub fn queue_error(&self, error: LlmError) {
        self.state.lock().unwrap().responses.push_back(Err(error));
    }
}

#[async_trait::async_trait]
impl LlmClient for MockLlmClient {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut state = self.state.lock().unwrap();
        if let Some(response) = state.responses.pop_front() {
            return response;
        }

        // Default response with context awareness
        let mut content = state.default_response.clone();
        if !request.context.is_empty() {
            content.push_str(&format!("\n(Used {} context documents)", request.context.len()));
        }

        Ok(LlmResponse {
            content,
            tokens_used: TokenUsage {
                prompt: request.user_prompt.len() as u32 / 4 + 1,
                completion: 50,
                total: request.user_prompt.len() as u32 / 4 + 51,
            },
            model: "mock-model".to_string(),
        })
    }
}

/// OpenAI LLM client implementation.
///
/// Supports OpenAI-compatible APIs including:
/// - OpenAI (gpt-4o, gpt-4o-mini, gpt-5.2)
/// - Azure OpenAI
/// - Compatible endpoints (Groq, Together, etc.)
#[cfg(feature = "openai")]
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    api_key: String,
    base_url: String,
    model: String,
    org_id: Option<String>,
    project_id: Option<String>,
}

#[cfg(feature = "openai")]
impl OpenAiClient {
    /// Create an OpenAI client from environment variables.
    ///
    /// Required: `OPENAI_API_KEY`
    /// Optional: `OPENAI_BASE_URL`, `OPENAI_MODEL`, `OPENAI_ORG_ID`, `OPENAI_PROJECT`
    pub fn from_env() -> Result<Self, LlmError> {
        let api_key = env::var("OPENAI_API_KEY")
            .map_err(|_| LlmError::AuthError("OPENAI_API_KEY not set".to_string()))?;
        
        let base_url = env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        
        let model = env::var("OPENAI_MODEL")
            .unwrap_or_else(|_| "gpt-4o-mini".to_string());
        
        let org_id = env::var("OPENAI_ORG_ID").ok();
        let project_id = env::var("OPENAI_PROJECT").ok();

        Ok(Self {
            api_key,
            base_url,
            model,
            org_id,
            project_id,
        })
    }

    /// Create an OpenAI client with explicit configuration.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: model.into(),
            org_id: None,
            project_id: None,
        }
    }

    /// Set a custom base URL for compatible endpoints.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Set organization ID.
    pub fn with_org_id(mut self, org_id: impl Into<String>) -> Self {
        self.org_id = Some(org_id.into());
        self
    }

    /// Set project ID.
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }
}

#[cfg(feature = "openai")]
#[async_trait::async_trait]
impl LlmClient for OpenAiClient {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        let client = reqwest::Client::new();
        
        let mut messages = Vec::new();
        
        if let Some(system) = &request.system_prompt {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }
        
        // Append context documents to user prompt
        let mut user_content = request.user_prompt.clone();
        if !request.context.is_empty() {
            user_content.push_str("\n\nContext:\n");
            for (i, ctx) in request.context.iter().enumerate() {
                user_content.push_str(&format!("{}. {}\n", i + 1, ctx));
            }
        }
        
        messages.push(serde_json::json!({
            "role": "user",
            "content": user_content,
        }));
        
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
        });
        
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut req_builder = client.post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json");
        
        if let Some(org_id) = &self.org_id {
            req_builder = req_builder.header("OpenAI-Organization", org_id);
        }
        if let Some(project_id) = &self.project_id {
            req_builder = req_builder.header("OpenAI-Project", project_id);
        }
        
        let response = req_builder
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::NetworkError(e.to_string()))?;
        
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await
                .unwrap_or_else(|_| "No error details".to_string());
            
            match status.as_u16() {
                401 => return Err(LlmError::AuthError("Invalid API key".to_string())),
                429 => return Err(LlmError::RateLimited { retry_after_seconds: None }),
                404 => return Err(LlmError::ModelNotFound(self.model.clone())),
                _ => return Err(LlmError::NetworkError(format!("HTTP {}: {}", status, error_text))),
            }
        }
        
        let json: serde_json::Value = response.json()
            .await
            .map_err(|e| LlmError::ParseError(e.to_string()))?;
        
        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| LlmError::ParseError("No content in response".to_string()))?
            .to_string();
        
        let model = json["model"]
            .as_str()
            .unwrap_or(&self.model)
            .to_string();
        
        let tokens_used = TokenUsage {
            prompt: json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion: json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
            total: json["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32,
        };
        
        Ok(LlmResponse {
            content,
            tokens_used,
            model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_client_default_response() {
        let client = MockLlmClient::new("Hello from mock");
        let request = LlmRequest {
            system_prompt: None,
            user_prompt: "Test query".to_string(),
            context: Vec::new(),
            max_tokens: None,
            temperature: None,
        };

        let response = client.generate(request).await.unwrap();
        assert_eq!(response.content, "Hello from mock");
        assert_eq!(response.model, "mock-model");
        assert!(response.tokens_used.total > 0);
    }

    #[tokio::test]
    async fn mock_client_with_context() {
        let client = MockLlmClient::new("Response");
        let request = LlmRequest {
            system_prompt: None,
            user_prompt: "Test query".to_string(),
            context: vec!["Context 1".to_string(), "Context 2".to_string()],
            max_tokens: None,
            temperature: None,
        };

        let response = client.generate(request).await.unwrap();
        assert!(response.content.contains("2 context documents"));
    }

    #[tokio::test]
    async fn mock_client_queue_success() {
        let client = MockLlmClient::new("Default");
        client.queue_success("First response");
        client.queue_success("Second response");

        let req1 = LlmRequest {
            system_prompt: None,
            user_prompt: "Q1".to_string(),
            context: Vec::new(),
            max_tokens: None,
            temperature: None,
        };
        let req2 = req1.clone();
        let req3 = req1.clone();

        assert_eq!(client.generate(req1).await.unwrap().content, "First response");
        assert_eq!(client.generate(req2).await.unwrap().content, "Second response");
        assert_eq!(client.generate(req3).await.unwrap().content, "Default");
    }

    #[tokio::test]
    async fn mock_client_queue_error() {
        let client = MockLlmClient::new("Default");
        client.queue_error(LlmError::RateLimited { retry_after_seconds: Some(10) });

        let request = LlmRequest {
            system_prompt: None,
            user_prompt: "Test".to_string(),
            context: Vec::new(),
            max_tokens: None,
            temperature: None,
        };

        let result = client.generate(request).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LlmError::RateLimited { .. }));
    }

    #[test]
    fn llm_error_display() {
        assert_eq!(
            LlmError::NetworkError("down".to_string()).to_string(),
            "Network error: down"
        );
        assert_eq!(
            LlmError::RateLimited { retry_after_seconds: Some(5) }.to_string(),
            "Rate limited (retry after 5s)"
        );
        assert_eq!(
            LlmError::RateLimited { retry_after_seconds: None }.to_string(),
            "Rate limited"
        );
    }

    #[test]
    fn think_prompt_builder_creates_system_prompt_with_json_schema() {
        let builder = ThinkPromptBuilder::new("What is ZBrain?");
        let system_prompt = builder.build_system_prompt();
        
        assert!(system_prompt.contains("JSON object"));
        assert!(system_prompt.contains(r#""answer""#));
        assert!(system_prompt.contains(r#""warnings""#));
        assert!(system_prompt.contains(r#""evidence_used""#));
        assert!(system_prompt.contains(r#""sources""#));
    }

    #[test]
    fn think_prompt_builder_includes_context_in_user_prompt() {
        let mut builder = ThinkPromptBuilder::new("What is ZBrain?");
        builder.add_context("ZBrain is a Rust rewrite of GBrain", "rust-migration");
        builder.add_context("ZBrain replaces TypeScript with Rust", "zbrain-architecture");
        
        let user_prompt = builder.build_user_prompt();
        
        assert!(user_prompt.contains("What is ZBrain?"));
        assert!(user_prompt.contains("rust-migration"));
        assert!(user_prompt.contains("zbrain-architecture"));
        assert!(user_prompt.contains("Rust rewrite of GBrain"));
    }

    #[test]
    fn think_prompt_builder_builds_valid_llm_request() {
        let mut builder = ThinkPromptBuilder::new("What is ZBrain?");
        builder.add_context("ZBrain is Rust", "rust-doc");
        
        let request = builder.build_request();
        
        assert!(request.system_prompt.is_some());
        assert!(request.user_prompt.contains("What is ZBrain?"));
        assert_eq!(request.context.len(), 1);
        assert_eq!(request.max_tokens, Some(500));
        assert_eq!(request.temperature, Some(0.2));
    }

    #[test]
    fn think_prompt_builder_parses_valid_json_response() {
        let json_response = r#"{"answer":"ZBrain is a Rust rewrite.","warnings":["Some info missing"],"evidence_used":2,"sources":["doc1","doc2"]}"#;
        
        let output = ThinkPromptBuilder::parse_response(json_response).unwrap();
        
        assert_eq!(output.answer, "ZBrain is a Rust rewrite.");
        assert_eq!(output.warnings, vec!["Some info missing"]);
        assert_eq!(output.evidence_used, 2);
        assert_eq!(output.sources, vec!["doc1", "doc2"]);
    }

    #[test]
    fn think_prompt_builder_parses_json_from_markdown_code_block() {
        let markdown_response = r#"
Here's my answer:

```json
{"answer":"ZBrain is Rust-based","warnings":[],"evidence_used":1,"sources":["rust-guide"]}
```
"#;
        
        let output = ThinkPromptBuilder::parse_response(markdown_response).unwrap();
        
        assert_eq!(output.answer, "ZBrain is Rust-based");
        assert_eq!(output.evidence_used, 1);
    }

    #[test]
    fn think_prompt_builder_parses_json_from_plain_code_block() {
        let markdown_response = r#"
```
{"answer":"Test answer","warnings":[],"evidence_used":0,"sources":[]}
```
"#;
        
        let output = ThinkPromptBuilder::parse_response(markdown_response).unwrap();
        
        assert_eq!(output.answer, "Test answer");
    }

    #[test]
    fn think_prompt_builder_returns_parse_error_for_invalid_json() {
        let invalid = "not valid json";
        
        let result = ThinkPromptBuilder::parse_response(invalid);
        
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LlmError::ParseError(_)));
    }

    #[tokio::test]
    async fn think_prompt_builder_mock_client_full_flow() {
        let mut builder = ThinkPromptBuilder::new("What is ZBrain?");
        builder.add_context("ZBrain is a Rust rewrite of GBrain", "rust-migration");
        
        let client = MockLlmClient::new("default");
        client.queue_success(r#"{"answer":"ZBrain is a Rust rewrite of GBrain.","warnings":[],"evidence_used":1,"sources":["rust-migration"]}"#);
        
        let request = builder.build_request();
        let response = client.generate(request).await.unwrap();
        let output = ThinkPromptBuilder::parse_response(&response.content).unwrap();
        
        assert!(output.answer.contains("Rust rewrite"));
        assert_eq!(output.evidence_used, 1);
        assert!(output.sources.contains(&"rust-migration".to_string()));
    }
}

/// A structured citation emitted by the model.
///
/// Mirrors TS `ParsedCitation` (src/core/think/cite-render.ts):
/// `row_num == None` is a page-level citation (`[slug]` marker),
/// `Some(n)` is a take citation (`[slug#n]`). `citation_index` is the
/// 1-based order of the citation within the answer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    /// Cited page slug (lowercase, validatePageSlug shape).
    pub page_slug: String,
    /// Row number for take citations; `None` for page-level citations.
    #[serde(default, deserialize_with = "de_lenient_row_num")]
    pub row_num: Option<i32>,
    /// 1-based order in the answer body (0 when the model omitted it;
    /// normalization re-indexes).
    #[serde(default)]
    pub citation_index: i32,
}

/// Lenient `row_num` deserializer mirroring TS `parseInt(String(row), 10)`:
/// accepts a JSON number, numeric string, or null. Anything unparseable
/// degrades to `None` (page-level) instead of failing the whole
/// `ThinkOutput` parse — TS salvages malformed citations the same way.
fn de_lenient_row_num<'de, D>(d: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.as_i64().and_then(|x| i32::try_from(x).ok()),
        serde_json::Value::String(s) => s.trim().parse::<i32>().ok(),
        _ => None,
    })
}

/// Result of [`resolve_citations`]: the cleaned citation list, any
/// normalization warnings, and whether the regex fallback was used.
#[derive(Debug, Clone)]
pub struct ResolvedCitations {
    /// Cleaned, deduped, 1-indexed citations.
    pub citations: Vec<Citation>,
    /// Warnings about dropped/invalid entries (TS warning codes).
    pub warnings: Vec<String>,
    /// True when the structured list was empty and the answer body was
    /// regex-scanned instead.
    pub used_fallback: bool,
}

/// Extract citation markers from an answer body. Port of TS
/// `parseInlineCitations` (cite-render.ts). Recognizes `[slug#3]` (take),
/// `[slug]` (page), and multi-segment slugs `[a/b/c#7]`. Dedupes on
/// `slug#row`, indexes 1-based in body order.
pub fn parse_inline_citations(body: &str) -> Vec<Citation> {
    static RX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| {
        // Same shape as validatePageSlug: lowercase alphanumeric + hyphens
        // + forward-slash separators; optional #N suffix. (?i) mirrors the
        // TS /gi flags; slugs are lowercased after match.
        regex::Regex::new(r"(?i)\[([a-z0-9][a-z0-9\-]*(?:/[a-z0-9][a-z0-9\-]*)*)(?:#(\d+))?\]")
            .expect("inline citation regex is valid")
    });
    let mut out: Vec<Citation> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut idx: i32 = 1;
    for cap in rx.captures_iter(body) {
        let slug = cap[1].to_lowercase();
        let row_num: Option<i32> = match cap.get(2) {
            // \d+ can only be positive; unparseable (i32 overflow) is
            // skipped like the TS non-finite guard.
            Some(m) => match m.as_str().parse::<i32>() {
                Ok(n) if n > 0 => Some(n),
                _ => continue,
            },
            None => None,
        };
        let key = format!(
            "{slug}#{}",
            row_num.map_or_else(|| "_".to_string(), |n| n.to_string())
        );
        if !seen.insert(key) {
            continue;
        }
        out.push(Citation {
            page_slug: slug,
            row_num,
            citation_index: idx,
        });
        idx += 1;
    }
    out
}

/// Validate + clean a structured citations list from the model. Port of TS
/// `normalizeStructuredCitations`: lowercases slugs, drops empty slugs
/// (`CITATION_MISSING_SLUG`) and non-positive rows
/// (`CITATION_INVALID_ROW`), dedupes on `slug#row`, re-indexes 1-based.
pub fn normalize_structured_citations(raw: &[Citation]) -> (Vec<Citation>, Vec<String>) {
    let mut citations: Vec<Citation> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut idx: i32 = 1;
    for c in raw {
        let slug = c.page_slug.trim().to_lowercase();
        if slug.is_empty() {
            warnings.push("CITATION_MISSING_SLUG".to_string());
            continue;
        }
        let row_num = match c.row_num {
            Some(n) if n > 0 => Some(n),
            Some(n) => {
                warnings.push(format!("CITATION_INVALID_ROW({slug}: {n})"));
                continue;
            }
            None => None,
        };
        let key = format!(
            "{slug}#{}",
            row_num.map_or_else(|| "_".to_string(), |n| n.to_string())
        );
        if !seen.insert(key) {
            continue;
        }
        citations.push(Citation {
            page_slug: slug,
            row_num,
            citation_index: idx,
        });
        idx += 1;
    }
    (citations, warnings)
}

/// Combine structured citations + body fallback into a single resolved
/// list. Port of TS `resolveCitations` trust contract: prefer the
/// structured field; if it yields nothing, regex-scan the answer body and
/// emit `CITATIONS_REGEX_FALLBACK` so callers know the synthesis was
/// rendered without explicit structured citations.
pub fn resolve_citations(structured: &[Citation], answer_body: &str) -> ResolvedCitations {
    let (citations, warnings) = normalize_structured_citations(structured);
    if !citations.is_empty() {
        return ResolvedCitations {
            citations,
            warnings,
            used_fallback: false,
        };
    }
    let fallback = parse_inline_citations(answer_body);
    let mut warnings = warnings;
    warnings.push("CITATIONS_REGEX_FALLBACK".to_string());
    ResolvedCitations {
        citations: fallback,
        warnings,
        used_fallback: true,
    }
}
