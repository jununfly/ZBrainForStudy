//! Contextual-reindex handler — re-embed pages with an LLM-generated context
//! paragraph prepended to each page's text (contextual embeddings). Maps the TS
//! `contextual_reindex_per_chunk` job, which used a Haiku LLM to synthesize a
//! per-chunk synopsis before embedding.
//!
//! ## Status (G84 resolved)
//!
//! Fully wired. A [`ChatProvider`] is injected at registration time through the
//! same dependency-injection seam the worker uses for the `subagent` handler
//! (see [`register_builtin_handlers`](crate::minions::handlers::registry::register_builtin_handlers)):
//! the handler stores the provider and uses it in `handle`. `handle` synthesizes
//! a short per-page context paragraph via the LLM, then embeds
//! `context + compiled_truth`. LLM failures fall back to the raw
//! `compiled_truth` (page-level fallback) so a single provider error never
//! blocks the whole re-embed. The embedding half is feature-gated behind
//! `embedding`; without a configured embedding provider it returns a clear
//! error.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::ai::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::engine::BrainEngine;
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::{Error, Result};

/// Contextual-reindex handler. Owns an `Arc<dyn ChatProvider>` injected at
/// construction (mirrors [`crate::minions::handlers::subagent::SubagentHandler`]).
pub struct ContextualReindexHandler {
    chat_provider: Arc<dyn ChatProvider>,
}

impl ContextualReindexHandler {
    /// Create a contextual-reindex handler wired to the given chat provider.
    #[must_use]
    pub fn new(chat_provider: Arc<dyn ChatProvider>) -> Self {
        Self { chat_provider }
    }
}

/// Synthesize a single concise context paragraph for a page, to be prepended to
/// its text before embedding. The paragraph helps retrieval match broader
/// queries that don't share wording with the page body.
///
/// LLM errors propagate: the caller decides whether to fall back to the raw
/// `compiled_truth` (this handler does, per-page).
async fn synthesize_page_context(
    chat: &dyn ChatProvider,
    page: &crate::engine::Page,
    model: &Option<String>,
) -> Result<String> {
    let prompt = format!(
        "Write ONE concise paragraph (max 100 words) of surrounding context for the \
         following note from a personal knowledge base. Describe what the note is \
         about, the key topics it covers, and how it might relate to other notes. \
         This context is prepended to the note's text before embedding so retrieval \
         can match broader queries. Output ONLY the context paragraph, no preamble.\n\n\
         TITLE: {}\n\nBODY:\n{}",
        page.title, page.compiled_truth
    );
    let opts = ChatOpts {
        model: model.clone(),
        system: Some(
            "You produce short, factual context paragraphs to improve embedding retrieval."
                .to_string(),
        ),
        messages: vec![ChatMessage::text(ChatRole::User, prompt)],
        tools: vec![],
        max_tokens: None,
        cache_system: false,
    };
    let result = chat
        .chat(opts)
        .await
        .map_err(|e| Error::new("ChatError", "contextual_reindex", &e.to_string()))?;
    Ok(result.text.trim().to_string())
}

#[async_trait]
impl MinionHandler for ContextualReindexHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let data = &ctx.data;
        let source_id = data
            .get("source_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let dry_run = data
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let model = data.get("model").and_then(|v| v.as_str()).map(String::from);
        let batch = data
            .get("batch")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(50)
            .max(1);

        #[cfg(feature = "embedding")]
        {
            use crate::minions::handlers::embed_util::embed_pages_augmented;

            let client = crate::embedding::EmbeddingClient::from_env().ok_or_else(|| {
                Error::new(
                    "ConfigurationError",
                    "contextual-reindex",
                    "embedding provider not configured: set ZEROENTROPY_API_KEY to re-embed pages",
                )
            })?;

            let engine = ctx.engine();
            let mut offset: usize = 0;
            let mut total_scanned = 0usize;
            let mut total_embedded = 0usize;
            let mut llm_failures = 0usize;

            loop {
                let stale = engine.list_stale_pages().await?;
                let take: Vec<_> = stale
                    .into_iter()
                    .filter(|p| source_id.as_ref().map_or(true, |s| &p.source_id == s))
                    .skip(offset)
                    .take(batch)
                    .collect();
                if take.is_empty() {
                    break;
                }

                // Synthesize per-page context (fail-soft → fallback to raw text).
                let mut items = Vec::with_capacity(take.len());
                for p in take {
                    let augmented = match synthesize_page_context(
                        self.chat_provider.as_ref(),
                        &p,
                        &model,
                    )
                    .await
                    {
                        Ok(ctx_text) if !ctx_text.is_empty() => {
                            format!("{}\n\n{}", ctx_text, p.compiled_truth)
                        }
                        _ => {
                            llm_failures += 1;
                            p.compiled_truth.clone()
                        }
                    };
                    items.push((p.slug, p.source_id, augmented));
                }

                let (scanned, embedded) =
                    embed_pages_augmented(engine.as_ref(), &client, items, dry_run).await?;
                total_scanned += scanned;
                total_embedded += embedded;
                offset += scanned;
                if dry_run {
                    continue;
                }
            }

            return Ok(json!({
                "status": "ok",
                "dry_run": dry_run,
                "scanned": total_scanned,
                "embedded": total_embedded,
                "llm_context_failures": llm_failures,
                "llm_context_augmentation": if llm_failures == 0 { "applied" } else { "partial" },
            }));
        }

        #[cfg(not(feature = "embedding"))]
        {
            Err(Error::new(
                "Unsupported",
                "contextual-reindex",
                "the 'embedding' feature is not enabled in this build; contextual-reindex requires it",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;
    use crate::engine::Page;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn handler_ctx(
        name: &str,
        data: Value,
        chat: Arc<dyn ChatProvider>,
    ) -> (Arc<dyn BrainEngine>, MinionJobContext, ContextualReindexHandler) {
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let context = MinionJobContext::new(
            Arc::clone(&engine),
            1,
            name.to_string(),
            data,
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        let handler = ContextualReindexHandler::new(chat);
        (engine, context, handler)
    }

    #[tokio::test]
    async fn contextual_reindex_fails_without_embedding_feature() {
        let (_e, context, handler) = handler_ctx(
            "contextual_reindex_per_chunk",
            json!({"dry_run": true}),
            Arc::new(MockChatProvider::new("unused")),
        );
        let r = handler.handle(&context).await;
        assert!(
            r.is_err(),
            "contextual-reindex must not succeed without the embedding feature"
        );
    }

    #[tokio::test]
    async fn synthesize_page_context_calls_llm_with_page_content() {
        let provider = Arc::new(MockChatProvider::new(
            "This note covers Rust async runtimes and the tokio scheduler.",
        ));
        let page = Page {
            title: "Rust Async".into(),
            compiled_truth: "Tokio is an async runtime for Rust.".into(),
            slug: "rust-async".into(),
            ..Default::default()
        };
        let ctx_text = synthesize_page_context(provider.as_ref(), &page, &None)
            .await
            .expect("llm call");
        assert!(
            ctx_text.to_lowercase().contains("rust") || ctx_text.to_lowercase().contains("tokio"),
            "context should reference the page: {ctx_text}"
        );
    }
}
