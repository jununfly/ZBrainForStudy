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

/// 代码边输入（用于 add_code_edges）。
///
/// 对齐 TS `CodeEdgeInput`（src/core/types.ts:928）：`from_chunk_id` 恒已知；
/// `to_chunk_id` 为 `None` 时该行落入 `code_edges_symbol`（目标仅以合格符号名
/// 已知，定义 chunk 尚未导入），否则落入 `code_edges_chunk`（两端均为已知
/// chunk id）。读路径在 1-6-7-10-2 通过 UNION 两表消费。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeEdgeInput {
    pub from_chunk_id: i64,
    /// 已解析目标 chunk id。`None` → 行落入 `code_edges_symbol`。
    #[serde(default)]
    pub to_chunk_id: Option<i64>,
    pub from_symbol_qualified: String,
    pub to_symbol_qualified: String,
    pub edge_type: String,
    #[serde(default)]
    pub edge_metadata: serde_json::Value,
    #[serde(default)]
    pub source_id: Option<String>,
}

/// 代码边查询结果行。对齐 TS `CodeEdgeResult`（src/core/types.ts:946）。
///
/// `resolved = true` 表示该行来自 `code_edges_chunk`（`to_chunk_id` 已知）；
/// `resolved = false` 表示来自 `code_edges_symbol`（`to_chunk_id` 为 null）。
/// 读路径（1-6-7-10-2）对两表 UNION 后统一映射为此结构。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodeEdgeResult {
    pub id: i64,
    pub from_chunk_id: i64,
    pub to_chunk_id: Option<i64>,
    pub from_symbol_qualified: String,
    pub to_symbol_qualified: String,
    pub edge_type: String,
    pub edge_metadata: serde_json::Value,
    pub source_id: Option<String>,
    pub resolved: bool,
}

/// `get_callers_of` / `get_callees_of` 的查询选项。
#[derive(Debug, Clone, Default)]
pub struct CodeGraphQueryOpts {
    /// 结果上限；缺省 100，硬性上限 500（对齐 TS）。
    pub limit: Option<usize>,
    /// 跨 source 查询（忽略 source 作用域）。
    pub all_sources: bool,
    /// 单一 source 作用域；与 `all_sources` 互斥，`all_sources = true` 时忽略。
    pub source_id: Option<String>,
}

/// `get_edges_by_chunk` 的方向过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodeEdgeDirection {
    /// 同时返回指向该 chunk 与从该 chunk 出发的边。
    #[default]
    Both,
    /// 仅返回指向该 chunk 的边（`to_chunk_id = chunk_id`，仅 resolved 行）。
    In,
    /// 仅返回从该 chunk 出发的边（`from_chunk_id = chunk_id`）。
    Out,
}

/// `get_edges_by_chunk` 的查询选项。
#[derive(Debug, Clone, Default)]
pub struct CodeEdgeByChunkOpts {
    pub direction: CodeEdgeDirection,
    pub edge_type: Option<String>,
    /// 结果上限；缺省 50，硬性上限 200（对齐 TS）。
    pub limit: Option<usize>,
}

/// 符号定义查询结果行。对齐 TS `CodeDefResult`（`src/commands/code-def.ts:19`）。
///
/// `find_code_def` 在 `content_chunks` 上按 `symbol_name` 精确匹配，限制
/// `symbol_type IN (DEF_TYPES)` 且所属页面 `page_kind = 'code'`，JOIN `pages`
/// 取 `slug` 与 `frontmatter->>'file'`。`snippet` 为 `chunk_text` 前 500 字。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodeDefResult {
    pub slug: String,
    pub file: Option<String>,
    pub language: Option<String>,
    pub symbol_type: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub snippet: String,
}

/// 符号引用查询结果行。对齐 TS `CodeRefResult`（`src/commands/code-refs.ts:24`）。
///
/// `find_code_refs` 在 `content_chunks` 上按 `chunk_text ILIKE '%symbol%'` 模糊
/// 匹配，限制所属页面 `page_kind = 'code'`，JOIN `pages` 取 `slug` 与
/// `frontmatter->>'file'`。`snippet` 为 `chunk_text` 前 500 字。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodeRefResult {
    pub slug: String,
    pub file: Option<String>,
    pub language: Option<String>,
    pub symbol_name: Option<String>,
    pub symbol_type: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub snippet: String,
}

/// `find_code_def` / `find_code_refs` 的共享查询选项。
#[derive(Debug, Clone, Default)]
pub struct CodeSymbolQueryOpts {
    /// 结果上限；缺省 20（def）/ 50（refs），硬性上限 500（对齐 TS）。
    pub limit: Option<i64>,
    /// 单一语言作用域过滤（`cc.language = ?`）；`None` 不过滤。
    pub language: Option<String>,
}

/// 1-6-7-10-4 符号消歧：裸名（bare name）→ 合格符号名（`symbol_name_qualified`）解析。
///
/// 对齐 TS `disambiguateSymbol`（`src/core/code-intel/recursive-walk.ts:77`），
/// 被 `runRecursiveWalk` 在递归遍历前用于把用户输入的裸符号名解析为唯一合格名。
///
/// - `matches`：精确命中——`DISTINCT symbol_name_qualified`，其中 chunk 的
///   `symbol_name` 或 `symbol_name_qualified` 等于裸名（递归遍历据此判定
///   `ambiguous`（>1）/ `not_found`（0）/ 继续（1））。
/// - `suggestions`：仅当 `matches` 为空时填充——`symbol_name_qualified ILIKE '%bare%'`
///   的 `did_you_mean` 候选（递归遍历的 `not_found` 信封）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SymbolDisambiguation {
    /// 精确命中（裸名 = `symbol_name` 或 `symbol_name_qualified`）。
    pub matches: Vec<String>,
    /// 近似候选（裸名作为子串，大小写不敏感），仅当 `matches` 为空时非空。
    pub suggestions: Vec<String>,
}

/// 递归遍历方向：`callers`（向上爆炸 / code_blast）或 `callees`（向下流动 / code_flow）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalkDirection {
    /// 向上爆炸：找所有调用当前符号的 caller。
    Callers,
    /// 向下流动：找当前符号调用的所有 callee。
    Callees,
}

impl Default for WalkDirection {
    fn default() -> Self {
        Self::Callers
    }
}

/// `run_recursive_walk` 遍历选项。对齐 TS `WalkOpts`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecursiveWalkOpts {
    /// 遍历方向：callers / callees。
    pub direction: WalkDirection,
    /// 跳数硬上限；缺省：callers=5，callees=8（对齐 TS）。
    pub depth_cap: Option<usize>,
    /// 总节点数硬上限；缺省 200（对齐 TS）。
    pub max_nodes: Option<usize>,
    /// 必须限定 source 作用域（所有遍历都限定 source，所以必填）。
    pub source_id: String,
    /// 是否强制精确匹配（跳过裸名消歧）；当输入已经是 `symbol_name_qualified` 时为 true。
    pub exact: Option<bool>,
}

/// BFS 遍历树中一个深度分组里的单个节点。对齐 TS `WalkNode`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecursiveWalkNode {
    /// 合格符号名（`symbol_name_qualified`）。
    pub symbol: String,
    /// 产生该边的原始 chunk id（可选）。
    pub chunk_id: Option<i64>,
    /// 终端节点的 sink 种类（仅 callee 方向向下遍历时分类）。
    pub sink_kind: Option<String>,
}

/// BFS 按深度分组返回结果。对齐 TS `DepthGroup`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DepthGroup {
    /// 当前深度（从 1 开始计数）。
    pub depth: usize,
    /// 该深度发现的所有新节点。
    pub nodes: Vec<RecursiveWalkNode>,
    /// 置信度：`1 / (1 + 0.3 * depth)`， clamped to `[0.05, 1.0]`。
    pub confidence: f32,
}

/// 截断原因：什么原因导致遍历提前结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalkTruncation {
    /// 无截断，遍历自然结束。
    None,
    /// 命中节点总数上限。
    MaxNodes,
    /// 命中深度上限。
    DepthCap,
    /// 同时命中深度和节点上限。
    Both,
}

impl Default for WalkTruncation {
    fn default() -> Self {
        Self::None
    }
}

/// 遍历结果的整体枚举状态。对齐 TS `WalkResult`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RecursiveWalkResult {
    /// 遍历成功完成（可能有截断）。
    Ok {
        depth_groups: Vec<DepthGroup>,
        cycles_detected: bool,
        truncation: WalkTruncation,
        freshness: WalkFreshness,
        terminal_nodes: Option<Vec<TerminalNode>>,
    },
    /// 起始符号找不到（消歧后零匹配），返回 did_you_mean 候选。
    NotFound { did_you_mean: Vec<DidYouMeanCandidate> },
    /// 起始符号消歧后多匹配，需要用户选择。
    Ambiguous { candidates: Vec<AmbiguousCandidate> },
    /// 起始符号语言不支持（目前只支持 TS/JS/Python）。
    UnsupportedLanguage { supported: Vec<String> },
}

/// `NotFound` 信封里的 did_you_mean 候选。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DidYouMeanCandidate {
    pub symbol_qualified: String,
    pub score: f32,
}

/// `Ambiguous` 信封里的多匹配候选。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AmbiguousCandidate {
    pub symbol_qualified: String,
    pub lang: Option<String>,
    pub file: Option<String>,
    pub lines: Option<String>,
}

/// 向下遍历（callees）找到的终端节点（sink）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalNode {
    pub symbol: String,
    pub sink_kind: String,
}

/// 数据新鲜度：v0.34 W3b 这里是缓存 freshness；Rust 实现暂固定为 `fresh`（因为我们直接查询 engine，不读缓存）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalkFreshness {
    Fresh,
    Partial,
}

impl Default for WalkFreshness {
    fn default() -> Self {
        Self::Fresh
    }
}

// --- 公共 API ---

use crate::error::Result;

/// 从 Markdown 内容导入
///
/// 这是 #110 的核心函数：协调整个导入事务
/// 1. 分割 Markdown 为 chunks
/// 2. 生成 chunk-level embeddings（当提供 `embedding_client` 时）— G25.
///    对每个 chunk_text 批量 embed 填 `ChunkInput.embedding`。fail-open：
///    embedding provider 失败时降级为 `None`（chunk 仍存，向量路降级为 lexical-only），
///    绝不因 embedding 抖动而让整个导入失败。`None` client → 全部 chunk.embedding 为 None。
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
    embedding_client: Option<&crate::embedding::EmbeddingClient>,
) -> Result<ImportResult> {
    // 1. 分割 markdown 为 chunks (简化版：按非空行分割)
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks: Vec<ChunkInput> = lines
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

    // 2. 生成 chunk embedding（G25，fail-open）。
    if let Some(client) = embedding_client {
        if !chunks.is_empty() {
            let texts: Vec<String> = chunks.iter().map(|c| c.chunk_text.clone()).collect();
            match client.embed_batch(&texts, None).await {
                Ok(vectors) if vectors.len() == chunks.len() => {
                    for (chunk, vec) in chunks.iter_mut().zip(vectors) {
                        chunk.embedding = Some(vec);
                    }
                }
                Ok(_) => {
                    // 数量不匹配：视为不可信，整体降级为无 embedding（保持 None）。
                }
                Err(_) => {
                    // fail-open：embedding provider 抖动不影响导入，chunk.embedding 保持 None。
                }
            }
        }
    }

    // 3. 存储 chunks
    engine.upsert_chunks(slug, &chunks).await?;

    // 4. 返回结果
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
    let code_chunks =
        zbrain_chunking::chunk::chunk_code_text(&content, file_path).map_err(|e| {
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
    use std::sync::Arc;

    /// 创建一个带 chunk 存储的 InMemoryEngine
    fn create_test_engine() -> InMemoryEngine {
        InMemoryEngine::new()
    }

    #[tokio::test]
    async fn upsert_chunks_stores_chunks() {
        let engine = create_test_engine();

        let slug = "test-page";
        let chunks = vec![ChunkInput {
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
        }];

        // 现在应该成功（InMemoryEngine 已实现）
        let result = engine.upsert_chunks(slug, &chunks).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn upsert_chunks_overwrites_existing_slug() {
        let engine = create_test_engine();

        let slug = "test-page";

        // 第一次插入
        let chunks1 = vec![ChunkInput {
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
        }];
        let result1 = engine.upsert_chunks(slug, &chunks1).await;
        assert!(result1.is_ok());

        // 第二次插入（应该覆盖，不报错）
        let chunks2 = vec![ChunkInput {
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
        }];
        let result2 = engine.upsert_chunks(slug, &chunks2).await;
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn delete_chunks_removes_chunks() {
        let engine = create_test_engine();
        let slug = "test-page";

        // 先插入
        let chunks = vec![ChunkInput {
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
        }];
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
        let sources = vec![
            ChunkSource::CompiledTruth,
            ChunkSource::Timeline,
            ChunkSource::FencedCode,
            ChunkSource::Image,
        ];
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

        let result =
            import_from_content(&engine, slug, Some(title), content, &tags, source, None).await;
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

        let result = import_from_content(&engine, slug, None, content, &tags, source, None).await;
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

        let result = import_from_content(&engine, slug, None, content, &tags, source, None).await;
        assert!(result.is_ok());

        let import_result = result.unwrap();
        // 3 非空行应该变成 3 个 chunks
        assert_eq!(import_result.chunks_created, 3);
    }

    // --- G25: chunk embedding 生成测试 ---

    /// 确定性 stub provider：每个 text 返回 [len, 1.0, 2.0]（维度=3），
    /// 便于断言"每个 chunk 拿到自己的向量"。
    struct StubEmbedProvider;

    #[async_trait::async_trait]
    impl crate::embedding::EmbeddingProvider for StubEmbedProvider {
        async fn embed(
            &self,
            texts: &[String],
            dims: usize,
        ) -> std::result::Result<Vec<Vec<f32>>, crate::embedding::EmbeddingError> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![t.len() as f32];
                    v.resize(dims, 1.0);
                    v
                })
                .collect())
        }

        async fn embed_image(
            &self,
            _base64_image: &str,
            _mime: Option<&str>,
            dims: usize,
        ) -> std::result::Result<Vec<f32>, crate::embedding::EmbeddingError> {
            Ok(vec![1.0; dims])
        }
    }

    /// 总是失败的 provider，验证 fail-open。
    struct FailingEmbedProvider;

    #[async_trait::async_trait]
    impl crate::embedding::EmbeddingProvider for FailingEmbedProvider {
        async fn embed(
            &self,
            _texts: &[String],
            _dims: usize,
        ) -> std::result::Result<Vec<Vec<f32>>, crate::embedding::EmbeddingError> {
            Err(crate::embedding::EmbeddingError::Provider("boom".into()))
        }

        async fn embed_image(
            &self,
            _base64_image: &str,
            _mime: Option<&str>,
            _dims: usize,
        ) -> std::result::Result<Vec<f32>, crate::embedding::EmbeddingError> {
            Err(crate::embedding::EmbeddingError::Provider("boom".into()))
        }
    }

    fn stub_client(
        provider: Arc<dyn crate::embedding::EmbeddingProvider>,
    ) -> crate::embedding::EmbeddingClient {
        let config = crate::embedding::EmbeddingConfig {
            dimensions: 3,
            ..crate::embedding::EmbeddingConfig::default()
        };
        crate::embedding::EmbeddingClient::with_provider(config, provider)
    }

    #[tokio::test]
    async fn import_from_content_generates_chunk_embeddings_when_client_present() {
        let engine = create_test_engine();
        let slug = "with-emb";
        let content = "alpha\nbeta\ngamma";
        let tags: Vec<String> = vec![];
        let client = stub_client(Arc::new(StubEmbedProvider));

        let result = import_from_content(
            &engine,
            slug,
            None,
            content,
            &tags,
            "default",
            Some(&client),
        )
        .await
        .expect("import should succeed");
        assert_eq!(result.chunks_created, 3);

        let stored = engine
            .get_chunks_for_page(slug)
            .await
            .expect("read chunks back");
        assert_eq!(stored.len(), 3);
        for chunk in &stored {
            let emb = chunk
                .embedding
                .as_ref()
                .expect("each chunk must carry a generated embedding (G25)");
            assert_eq!(emb.len(), 3, "embedding dimension must match provider dims");
        }
        // 每个 chunk 的向量首元素 = 文本长度，验证一一对应而非串位。
        let by_idx: std::collections::HashMap<usize, &ChunkInput> =
            stored.iter().map(|c| (c.chunk_index, c)).collect();
        assert_eq!(
            by_idx[&0].embedding.as_ref().unwrap()[0],
            "alpha".len() as f32
        );
    }

    #[tokio::test]
    async fn import_from_content_no_embedding_when_client_absent() {
        let engine = create_test_engine();
        let slug = "no-emb";
        let content = "alpha\nbeta";
        let tags: Vec<String> = vec![];

        import_from_content(&engine, slug, None, content, &tags, "default", None)
            .await
            .expect("import should succeed");

        let stored = engine.get_chunks_for_page(slug).await.expect("read chunks");
        assert_eq!(stored.len(), 2);
        for chunk in &stored {
            assert!(
                chunk.embedding.is_none(),
                "without an embedding client, chunk.embedding must stay None"
            );
        }
    }

    #[tokio::test]
    async fn import_from_content_fail_open_when_embedding_provider_errors() {
        let engine = create_test_engine();
        let slug = "fail-open";
        let content = "alpha\nbeta";
        let tags: Vec<String> = vec![];
        let client = stub_client(Arc::new(FailingEmbedProvider));

        // 导入必须成功，chunk 照存，只是 embedding 降级为 None。
        let result = import_from_content(
            &engine,
            slug,
            None,
            content,
            &tags,
            "default",
            Some(&client),
        )
        .await
        .expect("import must not fail when embedding provider errors (fail-open)");
        assert_eq!(result.chunks_created, 2);

        let stored = engine.get_chunks_for_page(slug).await.expect("read chunks");
        for chunk in &stored {
            assert!(
                chunk.embedding.is_none(),
                "on embedding error, chunk stored with embedding=None (fail-open)"
            );
        }
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
        let result =
            import_code_file(&engine, slug, rs_path.to_str().unwrap(), "rust", &tags).await;
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
        let result =
            import_code_file(&engine, slug, rs_path.to_str().unwrap(), "rust", &tags).await;

        assert!(
            result.is_ok(),
            "import_code_file should succeed: {:?}",
            result.err()
        );
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

        let result = import_code_file(&engine, slug, rs_path.to_str().unwrap(), "rust", &[])
            .await
            .unwrap();

        // Struct + 2 methods → at least 2 chunks
        assert!(
            result.chunks_created >= 2,
            "Expected >= 2 chunks, got {}",
            result.chunks_created
        );

        // Verify chunks are stored with metadata
        let stored = engine.get_chunks_for_page(slug).await.unwrap();
        assert!(!stored.is_empty());

        // Check that symbol metadata exists (for tree-sitter chunks)
        let has_symbols = stored.iter().any(|c| c.symbol_name.is_some());
        assert!(has_symbols, "Expected at least one chunk with symbol_name");

        let has_language = stored.iter().any(|c| c.language.as_deref() == Some("Rust"));
        assert!(
            has_language,
            "Expected at least one chunk with language=Rust"
        );

        let has_line_range = stored
            .iter()
            .any(|c| c.start_line.is_some() && c.end_line.is_some());
        assert!(
            has_line_range,
            "Expected at least one chunk with start/end line"
        );

        // Check parent_symbol_path exists for method chunks (nested in impl)
        let has_parent = stored.iter().any(|c| !c.parent_symbol_path.is_empty());
        assert!(
            has_parent,
            "Expected at least one chunk with parent_symbol_path"
        );
    }
}
