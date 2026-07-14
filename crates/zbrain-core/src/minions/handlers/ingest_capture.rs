//! Ingest-capture handler — import captured content into the brain.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts` — `ingest_capture` handler (127 lines). Calls
//! `importFromContent` to chunk + store markdown content as brain pages.
//!
//! ## Rust mapping
//!
//! Delegates to [`crate::import::import_from_content`] (1-7-1-5), which is
//! already ported. Parses `slug`, `title`, `content`, `tags`, `source` from
//! job data.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::StructuredError;
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::import::import_from_content;
use crate::Result;

pub struct IngestCaptureHandler;

/// Extract a required string field from a JSON object.
fn required_string(data: &Value, key: &str) -> Result<String> {
    data.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| StructuredError::new("handler", "invalid_input", &format!("missing required field: {key}")))
}

/// Extract an optional string field from a JSON object.
fn optional_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract an optional array of strings from a JSON object.
fn optional_string_array(data: &Value, key: &str) -> Vec<String> {
    data.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[async_trait]
impl MinionHandler for IngestCaptureHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let slug = required_string(&ctx.data, "slug")?;
        let title = optional_string(&ctx.data, "title");
        let content = required_string(&ctx.data, "content")?;
        let tags = optional_string_array(&ctx.data, "tags");
        let source = required_string(&ctx.data, "source")?;

        let result = import_from_content(
            ctx.engine().as_ref(),
            &slug,
            title.as_deref(),
            &content,
            &tags,
            &source,
        )
        .await?;

        serde_json::to_value(&result).map_err(|e| StructuredError::new("handler", "serialize_error", &e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    #[tokio::test]
    async fn ingest_capture_imports_content() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "ingest_capture".into(),
            json!({
                "slug": "test-page",
                "title": "Test Page",
                "content": "Line one\nLine two\n\nLine three",
                "tags": ["test", "demo"],
                "source": "capture"
            }),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = IngestCaptureHandler;
        let result = handler.handle(&context).await.expect("should succeed");

        assert_eq!(result["slug"], "test-page");
        assert_eq!(result["title"], "Test Page");
        assert_eq!(result["chunks_created"], 3); // 3 non-empty lines
    }

    #[tokio::test]
    async fn ingest_capture_missing_slug_returns_error() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "ingest_capture".into(),
            json!({"content": "hello", "source": "test"}),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = IngestCaptureHandler;
        assert!(handler.handle(&context).await.is_err());
    }

    #[tokio::test]
    async fn ingest_capture_missing_content_returns_error() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "ingest_capture".into(),
            json!({"slug": "test", "source": "test"}),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = IngestCaptureHandler;
        assert!(handler.handle(&context).await.is_err());
    }

    #[tokio::test]
    async fn ingest_capture_empty_content_zero_chunks() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "ingest_capture".into(),
            json!({
                "slug": "empty",
                "content": "",
                "source": "test"
            }),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = IngestCaptureHandler;
        let result = handler.handle(&context).await.expect("should succeed");
        assert_eq!(result["chunks_created"], 0);
    }

    #[tokio::test]
    async fn ingest_capture_optional_title_tags_default() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "ingest_capture".into(),
            json!({
                "slug": "minimal",
                "content": "just one line",
                "source": "test"
            }),
            0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = IngestCaptureHandler;
        let result = handler.handle(&context).await.expect("should succeed");
        assert_eq!(result["slug"], "minimal");
        assert!(result["title"].is_null());
        assert_eq!(result["chunks_created"], 1);
    }
}
