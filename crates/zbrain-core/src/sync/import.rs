//! Single-file import: read → capture → parse_markdown → put_page + add_tag.
//!
//! This is the core unit of work for the sync pipeline. Each call imports
//! one file from disk into the knowledge base via the engine.

use crate::capture::{capture_content, CaptureOpts};
use crate::chunkers::chunk_text;
use crate::cjk::count_cjk_aware_words;
use crate::engine::{BrainEngine, PageInput};
use crate::import::{ChunkInput, ChunkSource};
use crate::markdown::parse_markdown;
use std::sync::Arc;

/// Error type for import operations.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("failed to read file: {0}")]
    Io(#[from] std::io::Error),

    #[error("capture failed: {0}")]
    Capture(#[from] crate::capture::CaptureError),

    #[error("engine error: {0}")]
    Engine(#[from] crate::error::Error),
}

/// Options for importing a single file.
#[derive(Debug, Clone)]
pub struct ImportOnePathOpts {
    /// Source identifier for the page.
    pub source_id: String,
    /// Absolute path to the file on disk.
    pub abs_path: std::path::PathBuf,
    /// Path relative to the source root (used as `source_path` on the page).
    pub rel_path: std::path::PathBuf,
    /// Chunker version to stamp on the page.
    pub chunker_version: Option<i32>,
    /// Optional path prefix map for slug inference.
    pub path_prefixes: Option<std::collections::HashMap<String, String>>,
}

/// Result of importing a single file.
#[derive(Debug, Clone)]
pub struct ImportOnePathResult {
    /// The slug of the created/updated page.
    pub slug: String,
    /// The title of the page.
    pub title: String,
    /// Whether the content hash changed (i.e., this was an actual update).
    pub content_changed: bool,
    /// Number of markdown body chunks upserted for this page.
    pub chunks_upserted: usize,
}

/// Import a single file into the knowledge base.
///
/// # Pipeline
///
/// 1. Read the file from disk.
/// 2. Run through `capture_content` for binary detection, UTF-8 decode,
///    frontmatter parsing, and content hashing.
/// 3. Run through `parse_markdown` for slug/title/tag inference.
/// 4. Call `engine.put_page()` with the resulting `PageInput`.
/// 5. Call `engine.add_tag()` for each inferred tag.
pub async fn import_one_path(
    engine: &Arc<dyn BrainEngine>,
    opts: &ImportOnePathOpts,
) -> Result<ImportOnePathResult, ImportError> {
    // 1. Read the file
    let raw = tokio::fs::read(&opts.abs_path).await?;

    // 2. Capture: binary detection, UTF-8 decode, frontmatter parsing, content hash
    let capture_result = capture_content(
        &raw,
        &CaptureOpts {
            page_type: None,
            source: Some(opts.source_id.clone()),
            captured_at: Some(crate::time::current_utc_iso8601()),
        },
    )?;

    // 3. Parse markdown: slug, title, tags, type inference
    let rel_path_str = opts.rel_path.to_string_lossy().to_string();
    let parsed = parse_markdown(
        &capture_result.body,
        &rel_path_str,
        opts.path_prefixes.as_ref(),
    );

    // Determine page type from parsed markdown (validate against known types)
    let page_type = if crate::types::is_base_page_type(&parsed.type_) {
        parsed.type_.clone()
    } else {
        "note".to_string()
    };

    // Determine title: frontmatter title takes precedence over inferred title
    let title = capture_result
        .frontmatter
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(parsed.title.clone());

    // 4. Build PageInput and put_page
    let timeline = if parsed.timeline.is_empty() {
        None
    } else {
        Some(parsed.timeline)
    };

    let page_input = PageInput {
        page_type,
        title: title.clone(),
        compiled_truth: parsed.compiled_truth.clone(),
        timeline,
        frontmatter: Some(capture_result.frontmatter),
        content_hash: Some(capture_result.content_hash.clone()),
        page_kind: Some(crate::types::PageKind::Markdown),
        effective_date: None,
        effective_date_source: None,
        import_filename: None,
        chunker_version: opts.chunker_version,
        source_path: Some(rel_path_str),
        source_kind: Some("git".to_string()),
        source_uri: None,
        ingested_via: None,
        embedding: None,
        ingested_at: None,
        last_retrieved_at: None,
    };

    let _page = engine
        .put_page(&parsed.slug, Some(&opts.source_id), &page_input)
        .await?;

    let chunks = chunk_text(&parsed.compiled_truth, None);
    let chunk_inputs: Vec<ChunkInput> = chunks
        .into_iter()
        .map(|chunk| ChunkInput {
            chunk_index: chunk.index,
            token_count: Some(count_cjk_aware_words(&chunk.text)),
            chunk_text: chunk.text,
            chunk_source: ChunkSource::CompiledTruth,
            embedding: None,
            language: None,
            symbol_name: None,
            symbol_type: None,
            start_line: None,
            end_line: None,
            parent_symbol_path: vec![],
            symbol_name_qualified: None,
        })
        .collect();
    engine.upsert_chunks(&parsed.slug, &chunk_inputs).await?;
    let chunks_upserted = chunk_inputs.len();

    // 5. Add tags
    for tag in &parsed.tags {
        // Best-effort: don't fail the import if add_tag fails
        let _ = engine
            .add_tag(&parsed.slug, tag, Some(&opts.source_id))
            .await;
    }

    Ok(ImportOnePathResult {
        slug: parsed.slug,
        title,
        content_changed: true,
        chunks_upserted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{GetPageOpts, InMemoryEngine};
    use std::io::Write;
    use std::path::Path;

    async fn setup() -> (Arc<dyn BrainEngine>, tempfile::TempDir) {
        let engine = Arc::new(InMemoryEngine::default()) as Arc<dyn BrainEngine>;
        let dir = tempfile::TempDir::new().unwrap();

        // Create a test source
        engine
            .create_source(&crate::engine::CreateSourceInput {
                id: "test-source".to_string(),
                name: "Test".to_string(),
                config: None,
            })
            .await
            .unwrap();

        (engine, dir)
    }

    fn write_file(dir: &std::path::Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn import_simple_markdown_file() {
        let (engine, dir) = setup().await;

        write_file(
            dir.path(),
            "readme.md",
            "# Hello World\n\nThis is a test file.\n",
        );

        let opts = ImportOnePathOpts {
            source_id: "test-source".to_string(),
            abs_path: dir.path().join("readme.md"),
            rel_path: Path::new("readme.md").to_path_buf(),
            chunker_version: Some(1),
            path_prefixes: None,
        };

        let result = import_one_path(&engine, &opts).await.unwrap();

        assert!(!result.slug.is_empty());
        assert_eq!(result.title, "Hello World");
        assert!(result.content_changed);

        // Verify the page was stored
        let page = engine
            .get_page(
                &result.slug,
                &GetPageOpts {
                    source_id: Some("test-source".to_string()),
                    include_deleted: false,
                },
            )
            .await
            .unwrap()
            .expect("page should exist");

        assert_eq!(page.title, "Hello World");
        assert_eq!(page.source_id, "test-source");
        assert!(page.compiled_truth.contains("This is a test file"));
    }

    #[tokio::test]
    async fn import_markdown_file_upserts_chunks_from_parsed_body() {
        let (engine, dir) = setup().await;

        write_file(
            dir.path(),
            "chunked.md",
            "# Chunked Doc\n\nBody text should become a chunk.\n\n<!-- timeline -->\n\n2024: Timeline stays out of chunks.\n",
        );

        let opts = ImportOnePathOpts {
            source_id: "test-source".to_string(),
            abs_path: dir.path().join("chunked.md"),
            rel_path: Path::new("chunked.md").to_path_buf(),
            chunker_version: Some(1),
            path_prefixes: None,
        };

        let result = import_one_path(&engine, &opts).await.unwrap();

        assert_eq!(result.chunks_upserted, 1);

        let chunks = engine.get_chunks_for_page(&result.slug).await.unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(matches!(
            chunks[0].chunk_source,
            crate::import::ChunkSource::CompiledTruth
        ));
        assert!(chunks[0]
            .chunk_text
            .contains("Body text should become a chunk"));
        assert!(!chunks[0]
            .chunk_text
            .contains("Timeline stays out of chunks"));
        assert!(chunks[0].token_count.unwrap_or_default() > 0);
    }

    #[tokio::test]
    async fn import_fails_when_chunk_upsert_fails() {
        let engine = InMemoryEngine::new();
        engine.fail_chunk_upserts_for_tests(crate::error::StructuredError::engine(
            "chunk write failed",
        ));
        let engine = Arc::new(engine) as Arc<dyn BrainEngine>;
        let dir = tempfile::TempDir::new().unwrap();

        engine
            .create_source(&crate::engine::CreateSourceInput {
                id: "test-source".to_string(),
                name: "Test".to_string(),
                config: None,
            })
            .await
            .unwrap();

        write_file(dir.path(), "fails.md", "# Fails\n\nBody text.\n");

        let opts = ImportOnePathOpts {
            source_id: "test-source".to_string(),
            abs_path: dir.path().join("fails.md"),
            rel_path: Path::new("fails.md").to_path_buf(),
            chunker_version: Some(1),
            path_prefixes: None,
        };

        let err = import_one_path(&engine, &opts).await.unwrap_err();

        assert!(matches!(err, ImportError::Engine(_)));
        assert!(err.to_string().contains("chunk write failed"));
    }

    #[tokio::test]
    async fn import_markdown_with_frontmatter() {
        let (engine, dir) = setup().await;

        write_file(
            dir.path(),
            "post.md",
            "---\ntitle: Custom Title\ntags: [rust, sync]\n---\n\n# Actual Content\n\nBody text.\n",
        );

        let opts = ImportOnePathOpts {
            source_id: "test-source".to_string(),
            abs_path: dir.path().join("post.md"),
            rel_path: Path::new("post.md").to_path_buf(),
            chunker_version: Some(1),
            path_prefixes: None,
        };

        let result = import_one_path(&engine, &opts).await.unwrap();

        // Frontmatter title should take precedence
        assert_eq!(result.title, "Custom Title");

        let page = engine
            .get_page(
                &result.slug,
                &GetPageOpts {
                    source_id: Some("test-source".to_string()),
                    include_deleted: false,
                },
            )
            .await
            .unwrap()
            .expect("page should exist");

        assert_eq!(page.title, "Custom Title");
    }

    #[tokio::test]
    async fn import_twice_same_content_is_idempotent() {
        let (engine, dir) = setup().await;

        write_file(dir.path(), "doc.md", "# Same Content\n\nNo changes.\n");

        let opts = ImportOnePathOpts {
            source_id: "test-source".to_string(),
            abs_path: dir.path().join("doc.md"),
            rel_path: Path::new("doc.md").to_path_buf(),
            chunker_version: Some(1),
            path_prefixes: None,
        };

        let result1 = import_one_path(&engine, &opts).await.unwrap();
        assert!(result1.content_changed);

        // Second import with same content — should not error
        let result2 = import_one_path(&engine, &opts).await.unwrap();
        assert_eq!(result2.slug, result1.slug);
    }

    #[tokio::test]
    async fn import_nonexistent_file_errors() {
        let (engine, _dir) = setup().await;

        let opts = ImportOnePathOpts {
            source_id: "test-source".to_string(),
            abs_path: Path::new("/nonexistent/file.md").to_path_buf(),
            rel_path: Path::new("file.md").to_path_buf(),
            chunker_version: Some(1),
            path_prefixes: None,
        };

        let result = import_one_path(&engine, &opts).await;
        assert!(result.is_err());
    }
}
