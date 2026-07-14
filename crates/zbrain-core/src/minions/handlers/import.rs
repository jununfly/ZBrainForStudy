//! Import handler — imports a markdown file as a brain page.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts:1272` — `runImport` from `src/commands/import.ts`.
//! v1 is a thin wrapper: takes page data from the job payload and delegates
//! to `engine.put_page`. Full markdown parsing / file-system import is in the
//! CLI layer (port pending).
//!
//! ## Job data shape
//!
//! - `slug` (required): page slug.
//! - `title` (required): page title.
//! - `compiled_truth` (required): page body.
//! - `source_id` (optional): source to associate the page with.
//! - `page_type` (optional, default "page").

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::PageInput;
use crate::{Error, Result};

/// Imports a markdown file / content as a brain page.
pub struct ImportHandler;

#[async_trait]
impl MinionHandler for ImportHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let slug = ctx
            .data
            .get("slug")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::new(
                    "InvalidJobData",
                    "missing_slug",
                    "import job data must contain a non-empty \"slug\" field",
                )
            })?;

        let title = ctx
            .data
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::new(
                    "InvalidJobData",
                    "missing_title",
                    "import job data must contain a non-empty \"title\" field",
                )
            })?;

        let compiled_truth = ctx
            .data
            .get("compiled_truth")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let source_id = ctx.data.get("source_id").and_then(|v| v.as_str());

        let page_type = ctx
            .data
            .get("page_type")
            .and_then(|v| v.as_str())
            .unwrap_or("page")
            .to_string();

        let input = PageInput {
            page_type,
            title: title.to_string(),
            compiled_truth: compiled_truth.to_string(),
            ..Default::default()
        };

        let engine = ctx.engine();
        let page = engine.put_page(slug, source_id, &input).await?;
        Ok(serde_json::to_value(&page).unwrap_or(Value::Null))
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

    fn ctx(engine: &Arc<dyn BrainEngine>, data: Value) -> MinionJobContext {
        MinionJobContext::new(
            Arc::clone(engine),
            1,
            "import".to_string(),
            data,
            0,
            "test-token".to_string(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn import_handler_errors_on_missing_slug() {
        let eng = engine();
        let handler = ImportHandler;
        let context = ctx(&eng, json!({"title": "T"}));

        let err = handler.handle(&context).await.unwrap_err();
        assert!(err.to_string().contains("slug"));
    }

    #[tokio::test]
    async fn import_handler_errors_on_missing_title() {
        let eng = engine();
        let handler = ImportHandler;
        let context = ctx(&eng, json!({"slug": "s", "compiled_truth": "body"}));

        let err = handler.handle(&context).await.unwrap_err();
        assert!(err.to_string().contains("title"));
    }

    #[tokio::test]
    async fn import_handler_creates_page() {
        let eng = engine();
        let handler = ImportHandler;
        let context = ctx(
            &eng,
            json!({
                "slug": "test-import",
                "title": "Test Import",
                "compiled_truth": "Hello world",
                "source_id": "src1"
            }),
        );

        let result = handler.handle(&context).await.expect("handle should succeed");
        assert_eq!(result["slug"], "test-import");
        assert_eq!(result["title"], "Test Import");
        assert_eq!(result["compiledTruth"], "Hello world");
    }
}
