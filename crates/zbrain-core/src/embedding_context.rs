//! #111: Context retrieval wrapper for embeddings
//!
//! 三层阶梯：none → title → per_chunk_synopsis

use serde::{Deserialize, Serialize};

/// 上下文模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextMode {
    /// 不使用上下文（默认）
    #[default]
    None,
    /// 仅使用标题
    Title,
    /// 使用标题 + 每段摘要（最高质量）
    PerChunkSynopsis,
}

/// 构建上下文前缀（用于嵌入）
pub fn build_contextual_prefix(
    title: Option<&str>,
    synopsis: Option<&str>,
    mode: ContextMode,
) -> String {
    match mode {
        ContextMode::None => String::new(),
        ContextMode::Title => {
            if let Some(t) = title {
                format!("Title: {}\n\n", t.trim())
            } else {
                String::new()
            }
        }
        ContextMode::PerChunkSynopsis => {
            let mut parts = Vec::new();
            if let Some(t) = title {
                parts.push(format!("Title: {}", t.trim()));
            }
            if let Some(s) = synopsis {
                parts.push(format!("Synopsis: {}", s.trim()));
            }
            if parts.is_empty() {
                String::new()
            } else {
                format!("{}\n\n", parts.join("\n"))
            }
        }
    }
}

/// 为 chunk 文本包装上下文前缀
pub fn wrap_chunk_for_embedding(
    chunk_text: &str,
    title: Option<&str>,
    synopsis: Option<&str>,
    mode: ContextMode,
) -> String {
    let prefix = build_contextual_prefix(title, synopsis, mode);
    if prefix.is_empty() {
        chunk_text.to_string()
    } else {
        format!("{}{}", prefix, chunk_text)
    }
}

// --- 测试 ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_contextual_prefix_none_mode() {
        let prefix = build_contextual_prefix(Some("Test Title"), Some("Test synopsis"), ContextMode::None);
        assert!(prefix.is_empty());
    }

    #[test]
    fn build_contextual_prefix_title_mode_with_title() {
        let prefix = build_contextual_prefix(Some("My Document"), None, ContextMode::Title);
        assert_eq!(prefix, "Title: My Document\n\n");
    }

    #[test]
    fn build_contextual_prefix_title_mode_without_title() {
        let prefix = build_contextual_prefix(None, Some("synopsis"), ContextMode::Title);
        assert!(prefix.is_empty());
    }

    #[test]
    fn build_contextual_prefix_per_chunk_synopsis_mode() {
        let prefix = build_contextual_prefix(
            Some("My Doc"),
            Some("This doc is about..."),
            ContextMode::PerChunkSynopsis,
        );
        assert_eq!(prefix, "Title: My Doc\nSynopsis: This doc is about...\n\n");
    }

    #[test]
    fn build_contextual_prefix_per_chunk_synopsis_mode_title_only() {
        let prefix = build_contextual_prefix(Some("My Doc"), None, ContextMode::PerChunkSynopsis);
        assert_eq!(prefix, "Title: My Doc\n\n");
    }

    #[test]
    fn build_contextual_prefix_per_chunk_synopsis_mode_synopsis_only() {
        let prefix = build_contextual_prefix(None, Some("Overview..."), ContextMode::PerChunkSynopsis);
        assert_eq!(prefix, "Synopsis: Overview...\n\n");
    }

    #[test]
    fn wrap_chunk_for_embedding_none_mode() {
        let result = wrap_chunk_for_embedding("Chunk text here", None, None, ContextMode::None);
        assert_eq!(result, "Chunk text here");
    }

    #[test]
    fn wrap_chunk_for_embedding_title_mode() {
        let result = wrap_chunk_for_embedding(
            "Chunk text",
            Some("My Title"),
            None,
            ContextMode::Title,
        );
        assert_eq!(result, "Title: My Title\n\nChunk text");
    }

    #[test]
    fn wrap_chunk_for_embedding_full_mode() {
        let result = wrap_chunk_for_embedding(
            "Chunk text",
            Some("Doc"),
            Some("Summary"),
            ContextMode::PerChunkSynopsis,
        );
        assert_eq!(result, "Title: Doc\nSynopsis: Summary\n\nChunk text");
    }

    #[test]
    fn context_mode_default_is_none() {
        assert_eq!(ContextMode::default(), ContextMode::None);
    }
}
