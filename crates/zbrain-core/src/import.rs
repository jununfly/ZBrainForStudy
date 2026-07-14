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
#[derive(Debug, Clone, serde::Serialize)]
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
/// 2. 生成 embeddings (如果提供 embedding_client) — 尚未接线，chunk.embedding 恒 None，
///    无 embedding_client 参数。registered in docs/plans/KNOWN-GAPS.md (G25)
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
/// 2. 通过 tree-sitter 解析代码（函数/类/方法）
/// 3. 调用 engine.upsert_chunks()
/// 4. 返回 ImportResult
pub async fn import_code_file(
    engine: &dyn crate::engine::BrainEngine,
    slug: &str,
    file_path: &str,
    _language: &str,
    tags: &[String],
) -> Result<ImportResult> {
    // 1. 读取文件内容
    let content = std::fs::read_to_string(file_path).map_err(|e| {
        crate::error::StructuredError::new(
            "IO",
            "io_error",
            format!("Failed to read {}: {}", file_path, e),
        )
    })?;

    // 2. 通过 tree-sitter 切分代码
    let code_chunks = zbrain_chunking::chunk::chunk_code_text(&content, file_path).map_err(|e| {
        crate::error::StructuredError::new(
            "Chunking",
            "chunking_error",
            format!("Code chunking failed for {}: {}", file_path, e),
        )
    })?;

    // 3. CodeChunk → ChunkInput 映射
    let chunks: Vec<ChunkInput> = code_chunks
        .into_iter()
        .map(|cc| ChunkInput {
            chunk_index: cc.index,
            chunk_text: cc.text,
            chunk_source: ChunkSource::CompiledTruth,
            embedding: None,
            token_count: None,
            language: Some(cc.metadata.language),
            symbol_name: cc.metadata.symbol_name,
            symbol_type: Some(cc.metadata.symbol_type),
            start_line: Some(cc.metadata.start_line),
            end_line: Some(cc.metadata.end_line),
            parent_symbol_path: cc.metadata.parent_symbol_path,
            symbol_name_qualified: cc.metadata.symbol_name_qualified,
        })
        .collect();

    let chunks_created = chunks.len();

    // 4. 写入引擎
    engine.upsert_chunks(slug, &chunks).await?;

    // 5. 返回结果
    Ok(ImportResult {
        slug: slug.to_string(),
        title: None,
        chunks_created,
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

        // Create a real temp file (stub was replaced with real implementation)
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("test.rs");
        std::fs::write(&rs_path, "fn main() {}\n").unwrap();

        let tags = vec!["code".to_string()];
        let result = import_code_file(
            &engine,
            slug,
            rs_path.to_str().unwrap(),
            "rust",
            &tags,
        )
        .await;
        assert!(result.is_ok());

        let import_result = result.unwrap();
        assert_eq!(import_result.slug, slug);
        assert_eq!(import_result.tags_added, 1);
        assert!(import_result.chunks_created > 0);
    }

    /// RED: import_code_file with a real Rust source file should produce > 0 chunks.
    /// This test is expected to FAIL initially because import_code_file is a stub
    /// returning chunks_created: 0.
    #[tokio::test]
    async fn import_code_file_creates_chunks_from_real_file() {
        let engine = create_test_engine();
        let slug = "test-real-code";

        // Create a temp dir + .rs file with two Rust functions
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("test.rs");
        let content = "fn main() {}\n\nfn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        std::fs::write(&rs_path, content).unwrap();

        let tags = vec!["code".to_string()];
        let result = import_code_file(
            &engine,
            slug,
            rs_path.to_str().unwrap(),
            "rust",
            &tags,
        )
        .await;

        assert!(result.is_ok(), "import_code_file should succeed: {:?}", result.err());
        let import_result = result.unwrap();
        assert_eq!(import_result.slug, slug);
        assert_eq!(import_result.tags_added, 1);

        // This assertion will FAIL because the stub returns chunks_created: 0.
        assert!(
            import_result.chunks_created > 0,
            "Expected > 0 chunks from real code file, got {}",
            import_result.chunks_created
        );
    }

    /// Verify chunk metadata from tree-sitter code chunking: symbol names,
    /// language, line ranges, and parent paths should be populated.
    /// Uses a struct with methods to ensure multiple chunks (not merged).
    #[tokio::test]
    async fn import_code_file_chunk_metadata() {
        let engine = create_test_engine();
        let slug = "test-code-meta";

        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("lib.rs");
        // A Rust struct with two methods — won't be merged by mergeSmallSiblings
        let content = "\
pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn add(&mut self, x: i32) {
        self.value += x;
    }

    pub fn multiply(&mut self, x: i32) {
        self.value *= x;
    }
}
";
        std::fs::write(&rs_path, content).unwrap();

        let result = import_code_file(
            &engine,
            slug,
            rs_path.to_str().unwrap(),
            "rust",
            &[],
        )
        .await
        .unwrap();

        // Struct + 2 methods → at least 2 chunks
        assert!(result.chunks_created >= 2,
            "Expected >= 2 chunks, got {}", result.chunks_created);

        // Verify chunks are stored with metadata
        let stored = engine.get_chunks_for_page(slug).await.unwrap();
        assert!(!stored.is_empty());

        // Check that symbol metadata exists (for tree-sitter chunks)
        let has_symbols = stored.iter().any(|c| c.symbol_name.is_some());
        assert!(has_symbols, "Expected at least one chunk with symbol_name");

        let has_language = stored.iter().any(|c| c.language.as_deref() == Some("Rust"));
        assert!(has_language, "Expected at least one chunk with language=Rust");

        let has_line_range = stored.iter().any(|c| c.start_line.is_some() && c.end_line.is_some());
        assert!(has_line_range, "Expected at least one chunk with start/end line");

        // Check parent_symbol_path exists for method chunks (nested in impl)
        let has_parent = stored.iter().any(|c| !c.parent_symbol_path.is_empty());
        assert!(has_parent, "Expected at least one chunk with parent_symbol_path");
    }
}
