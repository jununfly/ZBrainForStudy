//! #111: Embedding Gateway - TDD Red Phase
//!
//! 嵌入公共 API、AI 网关嵌入路径、上下文检索包装器

use serde::{Deserialize, Serialize};
use std::sync::Arc;

// --- 数据结构 ---

/// 嵌入客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// 模型名称 (默认: "zeroentropyai:zembed-1")
    pub model: String,
    /// 嵌入维度 (默认: 1280)
    pub dimensions: usize,
    /// API 密钥
    pub api_key: String,
    /// 自定义基础 URL (可选)
    pub base_url: Option<String>,
    /// 最大重试次数 (默认: 2)
    pub max_retries: u32,
}

/// EmbeddingClient 构建器
pub struct EmbeddingConfigBuilder {
    model: Option<String>,
    dimensions: Option<usize>,
    api_key: Option<String>,
    base_url: Option<String>,
    max_retries: Option<u32>,
}

impl EmbeddingConfigBuilder {
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn dimensions(mut self, dims: usize) -> Self {
        self.dimensions = Some(dims);
        self
    }

    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn base_url(mut self, url: Option<String>) -> Self {
        self.base_url = url;
        self
    }

    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = Some(retries);
        self
    }

    pub fn build(self) -> Result<EmbeddingConfig, EmbeddingConfigError> {
        let api_key = self.api_key.ok_or_else(|| EmbeddingConfigError::MissingField("api_key".to_string()))?;

        Ok(EmbeddingConfig {
            model: self.model.unwrap_or_else(|| "zeroentropyai:zembed-1".to_string()),
            dimensions: self.dimensions.unwrap_or(1280),
            api_key,
            base_url: self.base_url,
            max_retries: self.max_retries.unwrap_or(2),
        })
    }
}

#[derive(Debug)]
pub enum EmbeddingConfigError {
    MissingField(String),
    /// The requested embedding backend cannot be built in this binary because
    /// the `embedding` cargo feature is disabled, so there is no HTTP provider
    /// to construct. Surfaced by `EmbeddingClient::new` instead of silently
    /// falling back to a zero-vector mock (which would poison the vector
    /// index).
    FeatureDisabled,
}

impl std::fmt::Display for EmbeddingConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField(field) => write!(f, "{} is required", field),
            Self::FeatureDisabled => write!(f, "embedding feature not enabled"),
        }
    }
}

impl std::error::Error for EmbeddingConfigError {}

impl EmbeddingConfig {
    pub fn builder() -> EmbeddingConfigBuilder {
        EmbeddingConfigBuilder {
            model: None,
            dimensions: None,
            api_key: None,
            base_url: None,
            max_retries: None,
        }
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: "zeroentropyai:zembed-1".to_string(),
            dimensions: 1280,
            api_key: String::new(),
            base_url: None,
            max_retries: 2,
        }
    }
}

// --- 嵌入客户端 ---

/// 嵌入提供者抽象 trait
#[async_trait::async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[String], dims: usize) -> Result<Vec<Vec<f32>>, EmbeddingError>;
    
    /// Embed a single image from base64 data (multimodal model).
    /// 
    /// The input should be the base64-encoded image bytes already, with the
    /// data URI prefix stripped (just the raw base64 chars).
    async fn embed_image(&self, base64_image: &str, mime: Option<&str>, dims: usize) -> Result<Vec<f32>, EmbeddingError>;
}

/// 主嵌入客户端
pub struct EmbeddingClient {
    config: EmbeddingConfig,
    provider: Arc<dyn EmbeddingProvider>,
}

impl EmbeddingClient {
    /// Build a production embedding client from an explicit config.
    ///
    /// With the `embedding` feature enabled this wires the real
    /// reqwest-backed `HttpEmbeddingProvider` (mirrors `create_http_client`).
    /// Without the feature there is no HTTP provider to construct, so this
    /// returns `EmbeddingConfigError::FeatureDisabled` rather than silently
    /// installing a zero-vector mock — a silent all-zeros embedding would
    /// poison the vector index and is never what a caller wants in
    /// production. Tests that need a deterministic provider should use
    /// `with_provider` (see `MockEmbeddingProvider`, test-only).
    #[cfg(feature = "embedding")]
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbeddingError> {
        http_provider::create_http_client(config)
    }

    /// Feature-disabled variant: see the `#[cfg(feature = "embedding")]`
    /// twin above for the rationale. Refuses loudly instead of returning a
    /// zero-vector client.
    #[cfg(not(feature = "embedding"))]
    pub fn new(_config: EmbeddingConfig) -> Result<Self, EmbeddingError> {
        Err(EmbeddingError::Config(EmbeddingConfigError::FeatureDisabled))
    }
    
    pub fn with_provider(config: EmbeddingConfig, provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self { config, provider }
    }
    
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let texts = vec![text.to_string()];
        let mut results = self.provider.embed(&texts, self.config.dimensions).await?;
        let embedding = results.remove(0);
        
        // 检查维度是否匹配。`dimensions == 0` 是哨兵：跳过校验并原样接受
        // provider 返回的原生维度（用于多模态列等维度不固定的场景）。
        if self.config.dimensions != 0 && embedding.len() != self.config.dimensions {
            return Err(EmbeddingError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: embedding.len(),
            });
        }
        
        Ok(embedding)
    }
    
    pub async fn embed_batch(&self, texts: &[String], on_progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>>) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let batch_size = 100;
        let mut all_results = Vec::new();
        
        for (i, chunk) in texts.chunks(batch_size).enumerate() {
            if let Some(ref cb) = on_progress {
                cb(i * batch_size, texts.len());
            }
            
            let chunk_results = self.provider.embed(chunk, self.config.dimensions).await?;
            all_results.extend(chunk_results);
        }
        
        if let Some(ref cb) = on_progress {
            cb(texts.len(), texts.len());
        }
        
        Ok(all_results)
    }
    
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed(text).await
    }
    
    /// Embed a single image for search-by-image.
    /// 
    /// - `base64_image`: base64-encoded image bytes (data URI prefix is allowed
    ///   and will be stripped automatically).
    /// - `image_mime`: optional MIME type hint (when ambiguous). Magic-byte sniffing
    ///   is authoritative if provided and hint mismatches are ignored.
    pub async fn embed_image(&self, base64_image: &str, image_mime: Option<&str>) -> Result<Vec<f32>, EmbeddingError> {
        let embedding = self.provider.embed_image(
            strip_data_uri_prefix(base64_image), 
            image_mime, 
            self.config.dimensions
        ).await?;
        
        // 检查维度是否匹配。`dimensions == 0` 是哨兵：跳过校验（见 embed）。
        if self.config.dimensions != 0 && embedding.len() != self.config.dimensions {
            return Err(EmbeddingError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: embedding.len(),
            });
        }
        
        Ok(embedding)
    }

    /// Model identifier backing this client (e.g. `voyage-multimodal-3`).
    /// Used for spend-audit attribution by `search_by_image`.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Build a production embedding client reading the API key from
    /// `ZEROENTROPY_API_KEY`. Returns `None` when the var is unset/empty so the
    /// caller can leave the vector path disabled (query search degrades to
    /// lexical-only) rather than fail. Mirrors
    /// `ZeroEntropyRerankClient::from_env` — secrets never live in the config
    /// file, and a missing key is a soft-off, not an error.
    ///
    /// Gated behind the `embedding` feature because it wires the real
    /// reqwest-backed `HttpEmbeddingProvider`; without the feature there is no
    /// HTTP provider to construct, so query search stays lexical-only.
    #[cfg(feature = "embedding")]
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("ZEROENTROPY_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())?;
        let config = EmbeddingConfig {
            api_key,
            ..EmbeddingConfig::default()
        };
        http_provider::create_http_client(config).ok()
    }
}

#[derive(Debug)]
pub enum EmbeddingError {
    Config(EmbeddingConfigError),
    Provider(String),
    DimensionMismatch { expected: usize, actual: usize },
}

impl From<EmbeddingConfigError> for EmbeddingError {
    fn from(err: EmbeddingConfigError) -> Self {
        Self::Config(err)
    }
}

impl std::fmt::Display for EmbeddingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(e) => write!(f, "Config error: {}", e),
            Self::Provider(msg) => write!(f, "Provider error: {}", msg),
            Self::DimensionMismatch { expected, actual } => {
                write!(f, "Dimension mismatch: expected {}, actual {}", expected, actual)
            }
        }
    }
}

impl std::error::Error for EmbeddingError {}

// Mock provider for testing. Test-only: it returns all-zero vectors, which
// must never reach a production vector index. Not compiled into release
// binaries. Wire it explicitly via `EmbeddingClient::with_provider`.
#[cfg(test)]
struct MockEmbeddingProvider;

#[cfg(test)]
#[async_trait::async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, texts: &[String], dims: usize) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|_| vec![0.0; dims]).collect())
    }

    async fn embed_image(
        &self,
        _base64_image: &str,
        _mime: Option<&str>,
        dims: usize,
    ) -> Result<Vec<f32>, EmbeddingError> {
        Ok(vec![0.0; dims])
    }
}

// --- HTTP Provider (requires `embedding` feature) ---

#[cfg(feature = "embedding")]
mod http_provider {
    use super::*;
    use reqwest::Client;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    /// OpenAI-compatible embedding request (text-only).
    #[derive(Debug, Serialize, Deserialize)]
    pub struct EmbeddingRequest {
        pub model: String,
        pub input: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub dimensions: Option<usize>,
    }

    /// Single multimodal input entry for a multimodal embedding request.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MultimodalInput {
        #[serde(rename = "type")]
        pub input_type: String,
        pub image: String,
    }

    /// OpenAI-compatible multimodal embedding request (single image).
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MultimodalEmbeddingRequest {
        pub model: String,
        pub inputs: Vec<MultimodalInput>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub dimensions: Option<usize>,
    }

    /// OpenAI-compatible embedding response
    #[derive(Debug, Deserialize)]
    pub struct EmbeddingResponse {
        pub data: Vec<EmbeddingData>,
        pub model: String,
        pub usage: Usage,
    }

    #[derive(Debug, Deserialize)]
    pub struct EmbeddingData {
        pub object: String,
        pub index: usize,
        #[serde(deserialize_with = "deserialize_embedding")]
        pub embedding: Vec<f32>,
    }

    /// 自定义反序列化：将 JSON 数组反序列化为 Vec<f32>
    fn deserialize_embedding<'de, D>(deserializer: D) -> std::result::Result<Vec<f32>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let values: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
        let mut result = Vec::with_capacity(values.len());
        for v in values {
            if let Some(f) = v.as_f64() {
                result.push(f as f32);
            } else {
                return Err(serde::de::Error::custom("Expected f64 for embedding value"));
            }
        }
        Ok(result)
    }

    #[derive(Debug, Deserialize)]
    pub struct Usage {
        pub prompt_tokens: usize,
    }

    /// HTTP-based embedding provider for OpenAI-compatible APIs
    pub struct HttpEmbeddingProvider {
        client: Client,
        base_url: String,
        api_key: String,
        model: String,
    }

    impl HttpEmbeddingProvider {
        pub fn new(client: Client, config: &EmbeddingConfig) -> Self {
            let base_url = config.base_url.clone().unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Self {
                client,
                base_url,
                api_key: config.api_key.clone(),
                model: config.model.clone(),
            }
        }

        fn build_url(&self) -> String {
            format!("{}/embeddings", self.base_url.trim_end_matches('/'))
        }
        
        /// 获取基础 URL（用于测试）
        pub fn base_url(&self) -> &str {
            &self.base_url
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for HttpEmbeddingProvider {
        async fn embed(&self, texts: &[String], dims: usize) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            let request = EmbeddingRequest {
                model: self.model.clone(),
                input: texts.to_vec(),
                dimensions: if dims > 0 { Some(dims) } else { None },
            };

            let response = self.client
                .post(self.build_url())
                .bearer_auth(&self.api_key)
                .json(&request)
                .send()
                .await
                .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                return Err(EmbeddingError::Provider(format!("HTTP {}: {}", status, error_text)));
            }

            let embedding_response: EmbeddingResponse = response
                .json()
                .await
                .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

            let embeddings: Vec<Vec<f32>> = embedding_response
                .data
                .into_iter()
                .map(|d| d.embedding)
                .collect();

            Ok(embeddings)
        }

        async fn embed_image(
            &self,
            base64_image: &str,
            _mime: Option<&str>,
            dims: usize,
        ) -> Result<Vec<f32>, EmbeddingError> {
            // Voyage multimodal expects:
            // {
            //   "model": "voyage-multimodal-3",
            //   "inputs": [{"type": "image", "image": "base64..."}]
            // }
            let input = MultimodalInput {
                input_type: "image".to_string(),
                image: base64_image.to_string(),
            };
            let request = MultimodalEmbeddingRequest {
                model: self.model.clone(),
                inputs: vec![input],
                dimensions: if dims > 0 { Some(dims) } else { None },
            };

            let response = self.client
                .post(self.build_url())
                .bearer_auth(&self.api_key)
                .json(&request)
                .send()
                .await
                .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                return Err(EmbeddingError::Provider(format!("HTTP {}: {}", status, error_text)));
            }

            let embedding_response: EmbeddingResponse = response
                .json()
                .await
                .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

            let mut embeddings: Vec<Vec<f32>> = embedding_response
                .data
                .into_iter()
                .map(|d| d.embedding)
                .collect();

            if embeddings.is_empty() {
                return Err(EmbeddingError::Provider("No embedding returned for image".to_string()));
            }

            Ok(embeddings.remove(0))
        }
    }

    /// 创建使用 HTTP provider（OpenAI 兼容）的 EmbeddingClient
    pub fn create_http_client(config: EmbeddingConfig) -> Result<EmbeddingClient, EmbeddingError> {
        let http_client = Client::new();
        let provider = Arc::new(HttpEmbeddingProvider::new(http_client, &config));
        
        Ok(EmbeddingClient {
            config,
            provider,
        })
    }
}

// --- HTTP Provider 序列化测试 ---

#[cfg(test)]
#[cfg(feature = "embedding")]
mod http_provider_tests {
    use super::*;
    use http_provider::*;

    #[test]
    fn embedding_request_serialization() {
        let request = EmbeddingRequest {
            model: "text-embedding-3-small".to_string(),
            input: vec!["Hello".to_string(), "World".to_string()],
            dimensions: Some(1536),
        };
        
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"input\""));
        assert!(json.contains("\"dimensions\""));
        
        // 反序列化检查
        let deserialized: EmbeddingRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.model, "text-embedding-3-small");
        assert_eq!(deserialized.input.len(), 2);
        assert_eq!(deserialized.dimensions, Some(1536));
    }

    #[test]
    fn embedding_request_serialization_without_dimensions() {
        let request = EmbeddingRequest {
            model: "text-embedding-3-small".to_string(),
            input: vec!["Hello".to_string()],
            dimensions: None,
        };
        
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("\"dimensions\""));
    }

    #[test]
    fn embedding_response_deserialization() {
        let json = r#"{
            "data": [
                {
                    "object": "embedding",
                    "index": 0,
                    "embedding": [0.1, 0.2, 0.3]
                }
            ],
            "model": "text-embedding-3-small",
            "usage": {
                "prompt_tokens": 5
            }
        }"#;
        
        let response: EmbeddingResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].index, 0);
        assert_eq!(response.data[0].embedding.len(), 3);
        assert_eq!(response.model, "text-embedding-3-small");
        assert_eq!(response.usage.prompt_tokens, 5);
    }

    #[test]
    fn http_embedding_provider_new() {
        let config = EmbeddingConfig::builder()
            .api_key("sk-test")
            .build()
            .unwrap();
        
        let client = reqwest::Client::new();
        let provider = HttpEmbeddingProvider::new(client, &config);
        
        // 检查基础 URL（默认 OpenAI）
        assert!(provider.base_url().contains("openai.com"));
    }

    #[test]
    fn http_embedding_provider_new_with_custom_base_url() {
        let config = EmbeddingConfig::builder()
            .api_key("sk-test")
            .base_url(Some("https://custom-api.example.com/v1".to_string()))
            .build()
            .unwrap();
        
        let client = reqwest::Client::new();
        let provider = HttpEmbeddingProvider::new(client, &config);
        
        assert!(provider.base_url().contains("custom-api.example.com"));
    }
}

// --- Token budget splitting ---

/// 估算文本的 token 数（简化版：1 token ≈ 4 chars for English, 1 char ≈ 1 token for CJK）
fn estimate_tokens(text: &str) -> usize {
    let cjk_count = text.chars().filter(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(c) ||  // CJK Unified
        ('\u{3040}'..='\u{309F}').contains(c) ||  // Hiragana
        ('\u{30A0}'..='\u{30FF}').contains(c) ||  // Katakana
        ('\u{AC00}'..='\u{D7AF}').contains(c)      // Hangul
    }).count();
    
    let ascii_count = text.len() - cjk_count;
    ((ascii_count / 4) + cjk_count).max(1)  // 至少 1 token
}

/// 按 token 预算拆分文本列表为多个批次
/// - `texts`: 待嵌入的文本列表
/// - `max_tokens`: 每批次最大 token 数（如模型的 token 限制）
/// - `safety_factor`: 安全因子 (0.05~0.8)，用于预留余量
/// 返回：批次列表，每个批次是一个文本切片
pub fn split_by_token_budget(texts: &[String], max_tokens: usize, safety_factor: f32) -> Vec<Vec<String>> {
    if texts.is_empty() {
        return vec![];
    }
    
    let effective_budget = (max_tokens as f32 * safety_factor.max(0.05).min(0.8)) as usize;
    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_tokens = 0usize;
    
    for text in texts {
        let text_tokens = estimate_tokens(text);
        
        // 如果单个文本超过预算，单独成一个批次
        if text_tokens > effective_budget {
            if !current_batch.is_empty() {
                batches.push(current_batch);
                current_batch = Vec::new();
                current_tokens = 0;
            }
            batches.push(vec![text.clone()]);
            continue;
        }
        
        // 如果加入这个文本会超限，先结束当前批次
        if current_tokens + text_tokens > effective_budget && !current_batch.is_empty() {
            batches.push(current_batch);
            current_batch = Vec::new();
            current_tokens = 0;
        }
        
        current_batch.push(text.clone());
        current_tokens += text_tokens;
    }
    
    if !current_batch.is_empty() {
        batches.push(current_batch);
    }
    
    batches
}

// --- 测试 ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_config_default() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.model, "zeroentropyai:zembed-1");
        assert_eq!(config.dimensions, 1280);
        assert_eq!(config.max_retries, 2);
        assert!(config.base_url.is_none());
    }

    #[test]
    fn embedding_config_builder_with_custom_values() {
        let config = EmbeddingConfig::builder()
            .model("text-embedding-3-small")
            .dimensions(1536)
            .api_key("sk-test-key")
            .base_url(Some("https://api.openai.com".to_string()))
            .max_retries(3)
            .build()
            .unwrap();
        
        assert_eq!(config.model, "text-embedding-3-small");
        assert_eq!(config.dimensions, 1536);
        assert_eq!(config.api_key, "sk-test-key");
        assert_eq!(config.base_url, Some("https://api.openai.com".to_string()));
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn embedding_config_builder_fails_without_api_key() {
        let result = EmbeddingConfig::builder().build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("api_key is required"));
    }

    #[tokio::test]
    async fn embedding_client_embed_single_text() {
        let config = EmbeddingConfig::builder()
            .model("text-embedding-3-small")
            .api_key("sk-test-key")
            .build()
            .unwrap();
        
        let client = EmbeddingClient::with_provider(config, Arc::new(MockEmbeddingProvider));
        let result = client.embed("Hello, world!").await.unwrap();
        
        assert_eq!(result.len(), 1280); // default dims
    }

    #[tokio::test]
    async fn embedding_client_embed_batch() {
        let config = EmbeddingConfig::builder()
            .api_key("sk-test-key")
            .dimensions(768)
            .build()
            .unwrap();
        
        let client = EmbeddingClient::with_provider(config, Arc::new(MockEmbeddingProvider));
        let texts = vec!["Hello".to_string(), "World".to_string()];
        let results = client.embed_batch(&texts, None).await.unwrap();
        
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].len(), 768);
    }

    #[tokio::test]
    async fn embedding_client_embed_query() {
        let config = EmbeddingConfig::builder()
            .api_key("sk-test-key")
            .build()
            .unwrap();
        
        let client = EmbeddingClient::with_provider(config, Arc::new(MockEmbeddingProvider));
        let result = client.embed_query("search query").await.unwrap();
        
        assert_eq!(result.len(), 1280);
    }

    #[tokio::test]
    async fn embedding_client_detects_dimension_mismatch() {
        // 创建一个返回错误维度的 mock provider
        use async_trait::async_trait;
        
        struct BadMockProvider;
        
        #[async_trait]
        impl EmbeddingProvider for BadMockProvider {
            async fn embed(&self, texts: &[String], _dims: usize) -> Result<Vec<Vec<f32>>, EmbeddingError> {
                // 返回错误维度 (512 instead of requested 768)
                Ok(texts.iter().map(|_| vec![0.0; 512]).collect())
            }

            async fn embed_image(
                &self,
                _base64_image: &str,
                _mime: Option<&str>,
                _dims: usize,
            ) -> Result<Vec<f32>, EmbeddingError> {
                Ok(vec![0.0; 512])
            }
        }
        
        let config = EmbeddingConfig::builder()
            .api_key("sk-test-key")
            .dimensions(768)
            .build()
            .unwrap();
        
        // 用 with_provider 直接注入返回错误维度的 provider
        let client = EmbeddingClient::with_provider(config, Arc::new(BadMockProvider));
        
        let result = client.embed("test").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            EmbeddingError::DimensionMismatch { expected, actual } => {
                assert_eq!(expected, 768);
                assert_eq!(actual, 512);
            }
            _ => panic!("Expected DimensionMismatch error"),
        }
    }

    /// Without the `embedding` feature, `new` must refuse loudly rather than
    /// hand back a zero-vector mock client. Guards the "silent all-zeros
    /// poisons the vector index" regression.
    #[cfg(not(feature = "embedding"))]
    #[test]
    fn embedding_client_new_refuses_without_feature() {
        let config = EmbeddingConfig::builder()
            .api_key("sk-test-key")
            .build()
            .unwrap();

        let result = EmbeddingClient::new(config);
        match result {
            Err(EmbeddingError::Config(EmbeddingConfigError::FeatureDisabled)) => {}
            Err(other) => panic!("Expected FeatureDisabled, got {other:?}"),
            Ok(_) => panic!("Expected FeatureDisabled error, got a client"),
        }
    }

    /// With the `embedding` feature, `new` builds a real HTTP-backed client
    /// (no zero-vector mock). We only assert it constructs — no network call.
    #[cfg(feature = "embedding")]
    #[test]
    fn embedding_client_new_builds_http_client_with_feature() {
        let config = EmbeddingConfig::builder()
            .api_key("sk-test-key")
            .build()
            .unwrap();

        assert!(EmbeddingClient::new(config).is_ok());
    }
}

// --- Token budget splitting tests ---

#[cfg(test)]
mod token_budget_tests {
    use super::*;

    /// 估算文本的 token 数（简化版：1 token ≈ 4 chars for English, 1 char ≈ 1 token for CJK）
    fn estimate_tokens(text: &str) -> usize {
        // 简化实现：英文约 4 chars/token，CJK 1 char/token
        let cjk_count = text.chars().filter(|c| {
            ('\u{4E00}'..='\u{9FFF}').contains(c) ||  // CJK Unified
            ('\u{3040}'..='\u{309F}').contains(c) ||  // Hiragana
            ('\u{30A0}'..='\u{30FF}').contains(c) ||  // Katakana
            ('\u{AC00}'..='\u{D7AF}').contains(c)      // Hangul
        }).count();
        
        let ascii_count = text.len() - cjk_count;
        (ascii_count / 4) + cjk_count + 1  // +1 避免 0
    }

    #[test]
    fn split_by_token_budget_empty_input() {
        let texts: Vec<String> = vec![];
        let batches = split_by_token_budget(&texts, 1000, 0.5);
        assert!(batches.is_empty());
    }

    #[test]
    fn split_by_token_budget_single_text_within_budget() {
        let texts = vec!["Hello world".to_string()];
        let batches = split_by_token_budget(&texts, 1000, 0.5);
        
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
    }

    #[test]
    fn split_by_token_budget_multiple_texts_split_needed() {
        // 每个文本约 100 tokens，budget 是 250 tokens，safety_factor=0.5 → 有效 budget=125
        // 所以每个 batch 约 1 个文本
        let texts: Vec<String> = (0..10)
            .map(|i| "Hello world, this is a test.".repeat(10) + &format!("Text {}", i))
            .collect();
        
        let batches = split_by_token_budget(&texts, 250, 0.5);
        
        // 应该分成多个 batches
        assert!(batches.len() > 1);
        
        // 每个 batch 的 token 数应该不超过 budget * safety_factor
        for batch in &batches {
            let batch_tokens: usize = batch.iter().map(|t| estimate_tokens(t)).sum();
            assert!(batch_tokens <= (250 as f32 * 0.5) as usize);
        }
    }

    #[test]
    fn split_by_token_budget_respects_safety_factor() {
        let texts: Vec<String> = (0..20)
            .map(|i| "A".repeat(100) + &format!(" {}", i))
            .collect();
        
        let batches_05 = split_by_token_budget(&texts, 1000, 0.5);
        let batches_08 = split_by_token_budget(&texts, 1000, 0.8);
        
        // safety_factor 越大，每个 batch 越多文本，batch 数量越少
        assert!(batches_05.len() >= batches_08.len());
    }
}

// --- Adaptive safety factor ---

/// 当批次因 token 超限被拒绝时，缩小安全因子（减少每批文本数）
/// 缩小幅度：当前值 × 0.6，但不低于 0.05
pub fn shrink_on_miss(safety_factor: &mut f32) {
    *safety_factor = (*safety_factor * 0.6).max(0.05);
}

/// 当批次成功处理时，尝试恢复安全因子（增加每批文本数）
/// 恢复幅度：当前值 + (0.8 - 当前值) × 0.3，但不超过 0.8
pub fn grow_on_hit(safety_factor: &mut f32) {
    *safety_factor = (*safety_factor + (0.8 - *safety_factor) * 0.3).min(0.8);
}

// --- Adaptive safety factor tests ---

#[cfg(test)]
mod adaptive_safety_tests {
    use super::*;

    #[test]
    fn shrink_on_miss_reduces_safety_factor() {
        let mut safety_factor = 0.8;
        shrink_on_miss(&mut safety_factor);
        
        assert!(safety_factor < 0.8);
        assert!(safety_factor >= 0.05);
    }

    #[test]
    fn shrink_on_miss_has_lower_bound() {
        let mut safety_factor = 0.06;
        shrink_on_miss(&mut safety_factor);
        
        // 不应该低于 0.05
        assert!(safety_factor >= 0.05);
    }

    #[test]
    fn grow_on_hit_increases_safety_factor() {
        let mut safety_factor = 0.3;
        grow_on_hit(&mut safety_factor);
        
        assert!(safety_factor > 0.3);
        assert!(safety_factor <= 0.8);
    }

    #[test]
    fn grow_on_hit_has_upper_bound() {
        let mut safety_factor = 0.79;
        grow_on_hit(&mut safety_factor);
        
        // 不应该超过 0.8
        assert!(safety_factor <= 0.8);
    }

    #[test]
    fn adaptive_loop_shrinks_then_grows() {
        let mut safety_factor = 0.8;
        
        // 模拟 3 次 miss（缩小）
        shrink_on_miss(&mut safety_factor);
        shrink_on_miss(&mut safety_factor);
        shrink_on_miss(&mut safety_factor);
        let after_shrinks = safety_factor;
        
        // 模拟 2 次 hit（恢复）
        grow_on_hit(&mut safety_factor);
        grow_on_hit(&mut safety_factor);
        
        assert!(after_shrinks < 0.8);
        assert!(safety_factor > after_shrinks);
        assert!(safety_factor <= 0.8);
    }
}

/// Strip the data: URI prefix from base64 image input if present.
/// 
/// Returns the raw base64 string without the prefix. If no prefix is present,
/// returns the input unchanged.
/// 
/// Handles: `data:image/png;base64,...` → `...`
fn strip_data_uri_prefix(input: &str) -> &str {
    if let Some((prefix, base64)) = input.split_once(",") {
        if prefix.starts_with("data:") {
            return base64;
        }
    }
    input
}
