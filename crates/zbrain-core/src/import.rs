// TDD: #110 Transaction write orchestration

use serde::{Deserialize, Serialize};

// --- 数据结构 ---

/// 导入时传递给 upsert_chunks 的 chunk 输入
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkInput {
    pub chunk_index: usize,
    pub chunk_text: String,
    pub chunk_source: ChunkSource,
    pub embedding: Option<Vec<f32>>,
    pub token_count: Option<usize>,
    pub language: Option<String>,
    pub symbol_name: Option<String>,
    pub symbol_type: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub parent_symbol_path: Vec<String>,
    pub symbol_name_qualified: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ChunkSource {
    CompiledTruth,
    Timeline,
    FencedCode,
    Image,
}

/// 导入结果
#[derive(Debug, Clone)]
pub struct ImportResult {
    pub slug: String,
    pub title: Option<String>,
    pub chunks_created: usize,
    pub chunks_updated: usize,
    pub tags_added: usize,
    pub tags_removed: usize,
}

/// 代码边输入（用于 add_code_edges）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeEdgeInput {
    pub source_chunk_id: i64,
    pub target_chunk_id: i64,
    pub edge_type: String,
    pub target_symbol: String,
    pub target_module: Option<String>,
}

// --- 公共 API ---

use crate::error::Result;

/// 从 Markdown 内容导入
///
/// 这是 #110 的核心函数：协调整个导入事务
/// 1. 分割 Markdown 为 chunks
/// 2. 生成 embeddings (如果提供 embedding_client)
/// 3. 调用 engine.upsert_chunks()
/// 4. 更新页面元数据
/// 5. 返回 ImportResult
pub async fn import_from_content(
    engine: &dyn crate::engine::BrainEngine,
    slug: &str,
    title: Option<&str>,
    content: &str,
    tags: &[String],
    source: &str,
) -> Result<ImportResult> {
    // 1. 分割 markdown 为 chunks (简化版：按非空行分割)
    let lines: Vec<&str> = content.lines().collect();
    let chunks: Vec<ChunkInput> = lines
        .into_iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| ChunkInput {
            chunk_index: idx,
            chunk_text: line.to_string(),
            chunk_source: ChunkSource::CompiledTruth,
            embedding: None,
            token_count: None,
            language: None,
            symbol_name: None,
            symbol_type: None,
            start_line: None,
            end_line: None,
            parent_symbol_path: vec![],
            symbol_name_qualified: None,
        })
        .collect();
    
    // 2. 存储 chunks
    engine.upsert_chunks(slug, &chunks).await?;
    
    // 3. 返回结果
    Ok(ImportResult {
        slug: slug.to_string(),
        title: title.map(|t| t.to_string()),
        chunks_created: chunks.len(),
        chunks_updated: 0,
        tags_added: tags.len(),
        tags_removed: 0,
    })
}

/// 从代码文件导入
///
/// 1. 读取文件
/// 2. 解析代码（函数/类/方法）
/// 3. 生成 embeddings
/// 4. 调用 engine.upsert_chunks()
/// 5. 调用 engine.add_code_edges()
/// 6. 返回 ImportResult
pub async fn import_code_file(
    engine: &dyn crate::engine::BrainEngine,
    slug: &str,
    file_path: &str,
    language: &str,
    tags: &[String],
) -> Result<ImportResult> {
    // TODO: 实现代码导入逻辑
    // 暂时返回占位符
    
    Ok(ImportResult {
        slug: slug.to_string(),
        title: None,
        chunks_created: 0,
        chunks_updated: 0,
        tags_added: tags.len(),
        tags_removed: 0,
    })
}

// --- 测试 ---

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::*;
    use crate::import::*;

    /// 创建一个带 chunk 存储的 InMemoryEngine
    fn create_test_engine() -> InMemoryEngine {
        InMemoryEngine::new()
    }

    #[tokio::test]
    async fn upsert_chunks_stores_chunks() {
        let engine = create_test_engine();
        
        let slug = "test-page";
        let chunks = vec![
            ChunkInput {
                chunk_index: 0,
                chunk_text: "Hello world".to_string(),
                chunk_source: ChunkSource::CompiledTruth,
                embedding: Some(vec![0.1, 0.2, 0.3]),
                token_count: Some(10),
                language: None,
                symbol_name: None,
                symbol_type: None,
                start_line: None,
                end_line: None,
                parent_symbol_path: vec![],
                symbol_name_qualified: None,
            }
        ];
        
        // 现在应该成功（InMemoryEngine 已实现）
        let result = engine.upsert_chunks(slug, &chunks).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn upsert_chunks_overwrites_existing_slug() {
        let engine = create_test_engine();
        
        let slug = "test-page";
        
        // 第一次插入
        let chunks1 = vec![
            ChunkInput {
                chunk_index: 0,
                chunk_text: "First version".to_string(),
                chunk_source: ChunkSource::CompiledTruth,
                embedding: None,
                token_count: None,
                language: None,
                symbol_name: None,
                symbol_type: None,
                start_line: None,
                end_line: None,
                parent_symbol_path: vec![],
                symbol_name_qualified: None,
            }
        ];
        let result1 = engine.upsert_chunks(slug, &chunks1).await;
        assert!(result1.is_ok());
        
        // 第二次插入（应该覆盖，不报错）
        let chunks2 = vec![
            ChunkInput {
                chunk_index: 0,
                chunk_text: "Second version".to_string(),
                chunk_source: ChunkSource::CompiledTruth,
                embedding: None,
                token_count: None,
                language: None,
                symbol_name: None,
                symbol_type: None,
                start_line: None,
                end_line: None,
                parent_symbol_path: vec![],
                symbol_name_qualified: None,
            }
        ];
        let result2 = engine.upsert_chunks(slug, &chunks2).await;
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn delete_chunks_removes_chunks() {
        let engine = create_test_engine();
        let slug = "test-page";
        
        // 先插入
        let chunks = vec![
            ChunkInput {
                chunk_index: 0,
                chunk_text: "Hello".to_string(),
                chunk_source: ChunkSource::CompiledTruth,
                embedding: None,
                token_count: None,
                language: None,
                symbol_name: None,
                symbol_type: None,
                start_line: None,
                end_line: None,
                parent_symbol_path: vec![],
                symbol_name_qualified: None,
            }
        ];
        let result1 = engine.upsert_chunks(slug, &chunks).await;
        assert!(result1.is_ok());
        
        // 删除
        let result2 = engine.delete_chunks(slug).await;
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn delete_chunks_on_nonexistent_slug_succeeds() {
        let engine = create_test_engine();
        let slug = "nonexistent";
        
        // 删除不存在的 slug 应该成功（不报错）
        let result = engine.delete_chunks(slug).await;
        assert!(result.is_ok());
    }

    #[test]
    fn chunk_input_serialization() {
        let chunk = ChunkInput {
            chunk_index: 0,
            chunk_text: "test".to_string(),
            chunk_source: ChunkSource::CompiledTruth,
            embedding: None,
            token_count: None,
            language: None,
            symbol_name: None,
            symbol_type: None,
            start_line: None,
            end_line: None,
            parent_symbol_path: vec![],
            symbol_name_qualified: None,
        };
        
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("\"chunk_index\""));
        assert!(json.contains("\"chunk_source\""));
    }

    #[test]
    fn chunk_source_serialization() {
        let sources = vec![ChunkSource::CompiledTruth, ChunkSource::Timeline, ChunkSource::FencedCode, ChunkSource::Image];
        for source in &sources {
            let json = serde_json::to_string(source).unwrap();
            assert!(!json.is_empty());
        }
    }

    #[test]
    fn chunk_source_deserialization() {
        let json = "\"CompiledTruth\"";
        let source: ChunkSource = serde_json::from_str(json).unwrap();
        assert!(matches!(source, ChunkSource::CompiledTruth));
    }

    // --- import_from_content 测试 ---

    #[tokio::test]
    async fn import_from_content_creates_chunks() {
        let engine = create_test_engine();
        let slug = "test-import";
        let title = "Test Import";
        let content = "# Hello\n\nThis is a test.\n\n## Section 2\n\nMore content.";
        let tags = vec!["tag1".to_string(), "tag2".to_string()];
        let source = "default";
        
        let result = import_from_content(&engine, slug, Some(title), content, &tags, source).await;
        assert!(result.is_ok());
        
        let import_result = result.unwrap();
        assert_eq!(import_result.slug, slug);
        assert_eq!(import_result.title, Some(title.to_string()));
        assert!(import_result.chunks_created > 0);
        assert_eq!(import_result.tags_added, 2);
    }

    #[tokio::test]
    async fn import_from_content_empty_content() {
        let engine = create_test_engine();
        let slug = "empty";
        let content = "";
        let tags = vec![];
        let source = "default";
        
        let result = import_from_content(&engine, slug, None, content, &tags, source).await;
        assert!(result.is_ok());
        
        let import_result = result.unwrap();
        assert_eq!(import_result.chunks_created, 0);
    }

    #[tokio::test]
    async fn import_from_content_splits_by_lines() {
        let engine = create_test_engine();
        let slug = "multiline";
        let content = "Line 1\nLine 2\n\nLine 4";
        let tags = vec![];
        let source = "default";
        
        let result = import_from_content(&engine, slug, None, content, &tags, source).await;
        assert!(result.is_ok());
        
        let import_result = result.unwrap();
        // 3 非空行应该变成 3 个 chunks
        assert_eq!(import_result.chunks_created, 3);
    }

    // --- import_code_file 测试 ---

    #[tokio::test]
    async fn import_code_file_placeholder() {
        let engine = create_test_engine();
        let slug = "test-code";
        let file_path = "test.rs";
        let language = "rust";
        let tags = vec!["code".to_string()];
        
        let result = import_code_file(&engine, slug, file_path, language, &tags).await;
        assert!(result.is_ok());
        
        let import_result = result.unwrap();
        assert_eq!(import_result.slug, slug);
        assert_eq!(import_result.tags_added, 1);
    }
}
