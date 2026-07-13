//! Individual brain tool implementations (roadmap 1-4-1).
//!
//! Each tool wraps one or more [`BrainEngine`] methods behind the [`ToolDef`]
//! trait. Input is JSON from the LLM, output is serialized JSON.
//!
//! ## Tool list (mirrors TS `operations.ts` allowlist)
//!
//! | Tool name              | Engine method(s)               | Idempotent |
//! |------------------------|--------------------------------|------------|
//! | `brain_resolve_slugs`  | `resolve_slugs`                | yes        |
//! | `brain_get_backlinks`  | `get_backlinks`                | yes        |
//! | `brain_get_recent_salience` | `get_recent_salience`      | yes        |
//! | `brain_list_pages`     | `list_pages`                   | yes        |
//! | `brain_get_page`       | `get_page` + `get_tags`        | yes        |
//! | `brain_search`         | `search_pages`                 | yes        |
//! | `brain_traverse_graph` | `traverse_paths`               | yes        |
//! | `brain_put_page`       | `put_page`                     | **no**     |
//!
//! ## Deferred (KNOWN-GAPS)
//!
//! - `brain_query` (hybridSearchCached → too complex for v1, use brain_search)
//! - `brain_find_anomalies` (engine method not yet ported)
//! - `brain_get_ingest_log` (engine method not yet ported)
//! - `brain_file_list` / `brain_file_url` (raw SQL, not engine)

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::engine::BrainEngine;
use crate::types::SalienceResult;
use crate::Result;

use super::ToolDef;

// ─── Helper: extract a single field from JSON input ──────────────────────────

fn required_string(input: &Value, field: &str) -> Result<String> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            crate::StructuredError::new(
                "InvalidInput",
                "bad_request",
                format!("tool input missing required field '{field}'"),
            )
            .into()
        })
}

fn optional_string(input: &Value, field: &str) -> Option<String> {
    input.get(field).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn optional_u32(input: &Value, field: &str) -> Option<u32> {
    input.get(field).and_then(|v| v.as_u64()).map(|n| n as u32)
}

fn optional_usize(input: &Value, field: &str) -> Option<usize> {
    input.get(field).and_then(|v| v.as_u64()).map(|n| n as usize)
}

// ─── brain_resolve_slugs ─────────────────────────────────────────────────────

struct ResolveSlugsTool;

#[async_trait]
impl ToolDef for ResolveSlugsTool {
    fn name(&self) -> &str {
        "brain_resolve_slugs"
    }

    fn description(&self) -> &str {
        "Resolve partial or fuzzy page slugs into exact slugs. Use this when \
         you have an approximate or partial page title and need the canonical slug."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "partial": {
                    "type": "string",
                    "description": "Partial or fuzzy slug to resolve"
                }
            },
            "required": ["partial"]
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use when you need to find the exact slug of a page from a partial name")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let partial = required_string(&input, "partial")?;
        let opts = crate::ResolveSlugsOpts {
            source_id: optional_string(&input, "source_id"),
            ..Default::default()
        };
        let slugs = engine.resolve_slugs(&partial, &opts).await?;
        Ok(serde_json::to_value(slugs).unwrap_or(Value::Null))
    }
}

// ─── brain_get_backlinks ─────────────────────────────────────────────────────

struct GetBacklinksTool;

#[async_trait]
impl ToolDef for GetBacklinksTool {
    fn name(&self) -> &str {
        "brain_get_backlinks"
    }

    fn description(&self) -> &str {
        "Get all pages that link to a given page. Use this to understand \
         what other pages reference a specific page."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "The page slug to get backlinks for"
                },
                "source_id": {
                    "type": "string",
                    "description": "Optional source scope"
                }
            },
            "required": ["slug"]
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use to discover which pages link to a given page")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let slug = required_string(&input, "slug")?;
        let source_id = optional_string(&input, "source_id");
        let links = engine
            .get_backlinks(&slug, source_id.as_deref())
            .await?;
        Ok(serde_json::to_value(links).unwrap_or(Value::Null))
    }
}

// ─── brain_get_recent_salience ───────────────────────────────────────────────

struct GetRecentSalienceTool;

#[async_trait]
impl ToolDef for GetRecentSalienceTool {
    fn name(&self) -> &str {
        "brain_get_recent_salience"
    }

    fn description(&self) -> &str {
        "Get recently salient pages (pages with high activity or importance \
         scores). Use this to discover trending or important topics."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "days": {
                    "type": "integer",
                    "description": "Lookback window in days (default 7)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 20)"
                },
                "slug_prefix": {
                    "type": "string",
                    "description": "Optional slug prefix filter"
                }
            }
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use to discover trending or important pages")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let days = optional_u32(&input, "days").unwrap_or(7);
        let limit = optional_u32(&input, "limit").unwrap_or(20);
        let slug_prefix = optional_string(&input, "slug_prefix");
        let results: Vec<SalienceResult> = engine
            .get_recent_salience(days, limit, slug_prefix.as_deref())
            .await?;
        Ok(serde_json::to_value(results).unwrap_or(Value::Null))
    }
}

// ─── brain_list_pages ────────────────────────────────────────────────────────

struct ListPagesTool;

#[async_trait]
impl ToolDef for ListPagesTool {
    fn name(&self) -> &str {
        "brain_list_pages"
    }

    fn description(&self) -> &str {
        "List pages in the brain, optionally filtered by type, source, or \
         search query. Use this to browse available pages."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "page_type": {
                    "type": "string",
                    "description": "Filter by page type (e.g. 'guide', 'doc')"
                },
                "source_id": {
                    "type": "string",
                    "description": "Filter by source"
                },
                "query": {
                    "type": "string",
                    "description": "Search query to filter pages"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 50)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Pagination offset (default 0)"
                }
            }
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use to browse or search for pages in the brain")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let filters = crate::PageFilters {
            page_type: optional_string(&input, "page_type"),
            source_id: optional_string(&input, "source_id"),
            tag: optional_string(&input, "tag"),
            slug_prefix: optional_string(&input, "slug_prefix"),
            limit: optional_usize(&input, "limit"),
            offset: optional_usize(&input, "offset"),
            ..Default::default()
        };
        let pages = engine.list_pages(&filters).await?;
        Ok(serde_json::to_value(pages).unwrap_or(Value::Null))
    }
}

// ─── brain_get_page ──────────────────────────────────────────────────────────

struct GetPageTool;

#[async_trait]
impl ToolDef for GetPageTool {
    fn name(&self) -> &str {
        "brain_get_page"
    }

    fn description(&self) -> &str {
        "Get the full content and metadata of a page by its slug. Use this \
         when you need to read a page's body, tags, and other details."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "The exact page slug"
                },
                "source_id": {
                    "type": "string",
                    "description": "Optional source scope"
                }
            },
            "required": ["slug"]
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use to read a page's full content and metadata")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let slug = required_string(&input, "slug")?;
        let source_id = optional_string(&input, "source_id");

        let opts = crate::GetPageOpts {
            source_id: source_id.clone(),
            ..Default::default()
        };
        let page = engine.get_page(&slug, &opts).await?;

        let tags = engine
            .get_tags(&slug, source_id.as_deref())
            .await
            .unwrap_or_default();

        Ok(json!({
            "page": page,
            "tags": tags,
        }))
    }
}

// ─── brain_search ────────────────────────────────────────────────────────────

struct SearchTool;

#[async_trait]
impl ToolDef for SearchTool {
    fn name(&self) -> &str {
        "brain_search"
    }

    fn description(&self) -> &str {
        "Search pages by keyword. Use this to find pages matching a query \
         string. Results include relevance scores and snippet highlights."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query string"
                },
                "source_id": {
                    "type": "string",
                    "description": "Optional source scope"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 10)"
                }
            },
            "required": ["query"]
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use to search for pages by keyword")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let query = required_string(&input, "query")?;
        let source_id = optional_string(&input, "source_id");

        let opts = crate::SearchOpts {
            keywords: vec![query],
            source_id,
            limit: optional_usize(&input, "limit"),
            ..Default::default()
        };
        let results = engine.search_pages(&opts).await?;
        Ok(serde_json::to_value(results).unwrap_or(Value::Null))
    }
}

// ─── brain_traverse_graph ────────────────────────────────────────────────────

struct TraverseGraphTool;

#[async_trait]
impl ToolDef for TraverseGraphTool {
    fn name(&self) -> &str {
        "brain_traverse_graph"
    }

    fn description(&self) -> &str {
        "Traverse the link graph starting from a page. Use this to explore \
         related pages, find connection paths, or understand the page's \
         neighborhood in the knowledge graph."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "Starting page slug"
                },
                "depth": {
                    "type": "integer",
                    "description": "Traversal depth (default 1)"
                },
                "link_type": {
                    "type": "string",
                    "description": "Filter by link type"
                },
                "direction": {
                    "type": "string",
                    "description": "'inbound', 'outbound', or null for both"
                }
            },
            "required": ["slug"]
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use to explore related pages in the knowledge graph")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let slug = required_string(&input, "slug")?;
        let depth = optional_u32(&input, "depth");
        let link_type = optional_string(&input, "link_type");
        let direction = optional_string(&input, "direction");

        let paths = engine
            .traverse_paths(
                &slug,
                depth,
                link_type.as_deref(),
                direction.as_deref(),
                None,
                None,
            )
            .await?;
        Ok(serde_json::to_value(paths).unwrap_or(Value::Null))
    }
}

// ─── brain_put_page ──────────────────────────────────────────────────────────

struct PutPageTool;

#[async_trait]
impl ToolDef for PutPageTool {
    fn name(&self) -> &str {
        "brain_put_page"
    }

    fn description(&self) -> &str {
        "Create or update a page in the brain. Use this to write new \
         information or update existing pages."
    }

    fn idempotent(&self) -> bool {
        false // write operation
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "Page slug (URL-safe identifier)"
                },
                "title": {
                    "type": "string",
                    "description": "Page title"
                },
                "body": {
                    "type": "string",
                    "description": "Page body content (markdown)"
                },
                "page_type": {
                    "type": "string",
                    "description": "Page type (e.g. 'note', 'guide')"
                },
                "source_id": {
                    "type": "string",
                    "description": "Optional source scope"
                }
            },
            "required": ["slug", "title", "body"]
        })
    }

    fn usage_hint(&self) -> Option<&str> {
        Some("Use to create or update a page — this is the only write tool")
    }

    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        _signal: CancellationToken,
    ) -> Result<Value> {
        let slug = required_string(&input, "slug")?;
        let title = required_string(&input, "title")?;
        let body = required_string(&input, "body")?;
        let page_type = input
            .get("page_type")
            .and_then(|v| v.as_str())
            .unwrap_or("note")
            .to_string();
        let source_id = optional_string(&input, "source_id");

        let page_input = crate::PageInput {
            page_type,
            title,
            compiled_truth: body,
            ..Default::default()
        };

        let page = engine
            .put_page(&slug, source_id.as_deref(), &page_input)
            .await?;
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    }
}

// ─── Registration ────────────────────────────────────────────────────────────

/// Register all brain tools. Called by [`super::build_brain_tools`] once per
/// tool as each implementation lands.
pub fn register_all(tools: &mut Vec<Arc<dyn ToolDef>>) {
    tools.push(Arc::new(ResolveSlugsTool));
    tools.push(Arc::new(GetBacklinksTool));
    tools.push(Arc::new(GetRecentSalienceTool));
    tools.push(Arc::new(ListPagesTool));
    tools.push(Arc::new(GetPageTool));
    tools.push(Arc::new(SearchTool));
    tools.push(Arc::new(TraverseGraphTool));
    tools.push(Arc::new(PutPageTool));
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryEngine;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    fn signal() -> CancellationToken {
        CancellationToken::new()
    }

    // Helper: verify a tool executes successfully on InMemory.
    async fn assert_tool_ok(tool: &dyn ToolDef, input: Value) {
        let result = tool.execute(input, engine(), signal()).await;
        assert!(result.is_ok(), "{} should succeed on InMemory: {:?}", tool.name(), result.err());
    }

    // ── resolve_slugs ───────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_slugs_succeeds_on_inmemory() {
        assert_tool_ok(
            &ResolveSlugsTool,
            json!({"partial": "rust"}),
        )
        .await;
    }

    #[tokio::test]
    async fn resolve_slugs_missing_partial_returns_error() {
        let result = ResolveSlugsTool
            .execute(json!({}), engine(), signal())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("partial"));
    }

    // ── get_backlinks ───────────────────────────────────────────────────

    #[tokio::test]
    async fn get_backlinks_succeeds_on_inmemory() {
        assert_tool_ok(
            &GetBacklinksTool,
            json!({"slug": "rust-guide"}),
        )
        .await;
    }

    // ── get_recent_salience ─────────────────────────────────────────────

    #[tokio::test]
    async fn get_recent_salience_succeeds_on_inmemory() {
        assert_tool_ok(
            &GetRecentSalienceTool,
            json!({"days": 7, "limit": 5}),
        )
        .await;
    }

    #[tokio::test]
    async fn get_recent_salience_uses_defaults_when_fields_missing() {
        // Should not panic on missing fields — uses defaults.
        let result = GetRecentSalienceTool
            .execute(json!({}), engine(), signal())
            .await;
        assert!(result.is_ok(), "defaults should work: {:?}", result.err());
    }

    // ── list_pages ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_pages_succeeds_on_inmemory() {
        assert_tool_ok(&ListPagesTool, json!({})).await;
    }

    // ── get_page ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_page_succeeds_on_inmemory() {
        assert_tool_ok(
            &GetPageTool,
            json!({"slug": "rust-guide"}),
        )
        .await;
    }

    // ── search ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn search_succeeds_on_inmemory() {
        assert_tool_ok(
            &SearchTool,
            json!({"query": "rust async"}),
        )
        .await;
    }

    #[tokio::test]
    async fn search_missing_query_returns_error() {
        let result = SearchTool
            .execute(json!({}), engine(), signal())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("query"));
    }

    // ── traverse_graph ──────────────────────────────────────────────────

    #[tokio::test]
    async fn traverse_graph_succeeds_on_inmemory() {
        assert_tool_ok(
            &TraverseGraphTool,
            json!({"slug": "start", "depth": 2}),
        )
        .await;
    }

    // ── put_page ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn put_page_is_not_idempotent() {
        assert!(!PutPageTool.idempotent());
    }

    #[tokio::test]
    async fn put_page_succeeds_on_inmemory() {
        assert_tool_ok(
            &PutPageTool,
            json!({"slug": "new", "title": "New", "body": "content"}),
        )
        .await;
    }

    #[tokio::test]
    async fn put_page_missing_required_fields_returns_error() {
        let result = PutPageTool
            .execute(json!({"slug": "x"}), engine(), signal())
            .await;
        assert!(result.is_err());
    }

    // ── register_all populates all 8 tools ──────────────────────────────

    #[test]
    fn register_all_adds_eight_tools() {
        let mut tools: Vec<Arc<dyn ToolDef>> = Vec::new();
        register_all(&mut tools);
        assert_eq!(tools.len(), 8);

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"brain_resolve_slugs"));
        assert!(names.contains(&"brain_get_backlinks"));
        assert!(names.contains(&"brain_get_recent_salience"));
        assert!(names.contains(&"brain_list_pages"));
        assert!(names.contains(&"brain_get_page"));
        assert!(names.contains(&"brain_search"));
        assert!(names.contains(&"brain_traverse_graph"));
        assert!(names.contains(&"brain_put_page"));
    }

    #[test]
    fn all_read_tools_are_idempotent() {
        let mut tools: Vec<Arc<dyn ToolDef>> = Vec::new();
        register_all(&mut tools);
        for tool in &tools {
            if tool.name() == "brain_put_page" {
                assert!(!tool.idempotent(), "put_page must not be idempotent");
            } else {
                assert!(tool.idempotent(), "{} should be idempotent", tool.name());
            }
        }
    }
}
