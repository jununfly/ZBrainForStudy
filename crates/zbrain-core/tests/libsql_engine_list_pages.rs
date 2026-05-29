//! Slice 6a S6-T5 + S6-T5b — `LibsqlEngine::list_pages` integration tests.
//!
//! Upgrades `list_pages` from a 7-column stub (`row_to_page`) to the full
//! 30-column projection (`full_row_to_page`) with 9 filter dimensions:
//!
//! | # | Filter          | Slice  | Field                          |
//! |---|-----------------|--------|--------------------------------|
//! | 1 | `page_type`       | S6-T5  | `PageFilters::page_type`       |
//! | 2 | `limit`           | S6-T5  | `PageFilters::limit`           |
//! | 3 | `offset`          | S6-T5  | `PageFilters::offset`          |
//! | 4 | `include_deleted` | S6-T5  | `PageFilters::include_deleted` |
//! | 5 | `sort`            | S6-T5  | `PageFilters::sort`            |
//! | 6 | `slug_prefix`     | S6-T5b | `PageFilters::slug_prefix`     |
//! | 7 | `source_id`       | S6-T5b | `PageFilters::source_id`       |
//! | 8 | `source_ids`      | S6-T5b | `PageFilters::source_ids`      |
//! | 9 | `updated_after`   | S6-T5b | `PageFilters::updated_after`   |
//!
//! `tag` filter is deferred to S6-T5c because it requires a new `page_tags`
//! table that the current migrations do not create.
//!
//! Test strategy:
//! - Each test allocates its own `NamedTempFile` (no cross-contamination).
//! - Tests insert rows via `put_page`, then call `list_pages` with various
//!   filters and assert on count, ordering, and 30-column field defaults.
//! - Red phase: all tests should fail because the current `list_pages`
//!   implementation only returns 7 columns and ignores `offset` / `sort` /
//!   `include_deleted`.

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageFilters, PageInput, PageSort};
use zbrain_core::libsql::LibsqlEngine;

/// Build a connected, schema-initialized engine on a fresh temp file.
async fn init_clean_engine() -> (LibsqlEngine, NamedTempFile) {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path)
}

fn note_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

fn topic_input(title: &str, body: &str) -> PageInput {
    PageInput {
        page_type: "topic".to_string(),
        title: title.to_string(),
        compiled_truth: body.to_string(),
        ..Default::default()
    }
}

// ─── T5-1: 30-column projection defaults ──────────────────────────────────

#[tokio::test]
async fn list_pages_projects_all_30_columns() {
    // A single row inserted via `put_page` should come back with non-default
    // 30-column fields.  Specifically we assert on columns that the 7-col stub
    // always leaves at `Default::default()`:
    //   - source_id       (should be "default")
    //   - page_kind       (should be Markdown, schema default 'markdown')
    //   - content_hash    (should be None, not absent)
    //   - deleted_at      (should be None, not absent)
    //   - created_at / updated_at  (should be non-empty ISO-8601)
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("proj-30col", &note_input("Thirty", "body"))
        .await
        .expect("put_page");

    let pages = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages");

    assert_eq!(pages.len(), 1, "should return exactly 1 page");
    let p = &pages[0];

    // Columns that the 7-col stub cannot populate:
    assert_eq!(p.source_id, "default", "source_id must be 'default' from schema");
    assert!(matches!(p.page_kind, zbrain_core::PageKind::Markdown), "page_kind must be Markdown (schema default)");
    assert!(p.created_at.len() > 8, "created_at must be a real timestamp, got '{}'", p.created_at);
    assert!(p.updated_at.len() > 8, "updated_at must be a real timestamp, got '{}'", p.updated_at);
    assert!(p.deleted_at.is_none(), "non-deleted row must have deleted_at = None");
    assert!(p.content_hash.is_none(), "content_hash default = None");

    engine.disconnect().await.expect("disconnect");
}

// ─── T5-2: page_type filter ──────────────────────────────────────────────

#[tokio::test]
async fn list_pages_filters_by_page_type() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("note-a", &note_input("NoteA", "n"))
        .await
        .expect("put_page note");
    engine
        .put_page("topic-b", &topic_input("TopicB", "t"))
        .await
        .expect("put_page topic");
    engine
        .put_page("note-c", &note_input("NoteC", "n2"))
        .await
        .expect("put_page note 2");

    let filters = PageFilters {
        page_type: Some("note".to_string()),
        ..Default::default()
    };

    let notes = engine
        .list_pages(&filters)
        .await
        .expect("list_pages filtered");

    assert_eq!(notes.len(), 2, "only 'note' pages should appear");
    assert!(notes.iter().all(|p| p.page_type == "note"), "all results must be note type");

    // Without filter → all 3 pages
    let all = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages all");

    assert_eq!(all.len(), 3, "unfiltered must return all 3 pages");
    engine.disconnect().await.expect("disconnect");
}

// ─── T5-3: limit ─────────────────────────────────────────────────────────

#[tokio::test]
async fn list_pages_respects_limit() {
    let (engine, _tmp) = init_clean_engine().await;
    for i in 0..5 {
        engine
            .put_page(&format!("lim-{i}"), &note_input(&format!("L{i}"), "b"))
            .await
            .expect("put_page");
    }

    let filters = PageFilters {
        limit: Some(2),
        ..Default::default()
    };

    let limited = engine
        .list_pages(&filters)
        .await
        .expect("list_pages limit=2");

    assert_eq!(limited.len(), 2, "limit=2 must cap results to 2");

    let all = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages no limit");

    assert_eq!(all.len(), 5, "no limit must return all 5");
    engine.disconnect().await.expect("disconnect");
}

// ─── T5-4: offset ────────────────────────────────────────────────────────

#[tokio::test]
async fn list_pages_respects_offset() {
    let (engine, _tmp) = init_clean_engine().await;
    for i in 0..5 {
        engine
            .put_page(&format!("off-{i}"), &note_input(&format!("O{i}"), "b"))
            .await
            .expect("put_page");
    }

    let filters = PageFilters {
        offset: Some(3),
        ..Default::default()
    };

    let paged = engine
        .list_pages(&filters)
        .await
        .expect("list_pages offset=3");

    assert_eq!(paged.len(), 2, "5 rows minus offset 3 = 2 remaining");

    // Verify the skipped slugs are not in the result
    let slugs: Vec<&str> = paged.iter().map(|p| p.slug.as_str()).collect();
    assert!(!slugs.contains(&"off-0"), "offset must skip first 3 rows");
    assert!(!slugs.contains(&"off-1"), "offset must skip first 3 rows");
    assert!(!slugs.contains(&"off-2"), "offset must skip first 3 rows");

    engine.disconnect().await.expect("disconnect");
}

// ─── T5-5: include_deleted ───────────────────────────────────────────────

#[tokio::test]
async fn list_pages_excludes_soft_deleted_by_default() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alive", &note_input("Alive", "b"))
        .await
        .expect("put_page alive");
    engine
        .put_page("zombie", &note_input("Zombie", "b"))
        .await
        .expect("put_page zombie");

    // Soft-delete the second page
    let hit = engine
        .soft_delete_page("zombie", None)
        .await
        .expect("soft_delete_page");
    assert_eq!(hit.as_deref(), Some("zombie"), "soft_delete must hit");

    // Default filter: include_deleted = false
    let pages = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages default");

    assert_eq!(pages.len(), 1, "soft-deleted rows must be excluded by default");
    assert_eq!(pages[0].slug, "alive", "only the non-deleted page remains");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_includes_soft_deleted_when_flag_set() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alive", &note_input("Alive", "b"))
        .await
        .expect("put_page alive");
    engine
        .put_page("zombie", &note_input("Zombie", "b"))
        .await
        .expect("put_page zombie");

    engine
        .soft_delete_page("zombie", None)
        .await
        .expect("soft_delete_page");

    let filters = PageFilters {
        include_deleted: true,
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages include_deleted=true");

    assert_eq!(pages.len(), 2, "include_deleted must show both rows");

    // Verify the deleted row actually carries a deleted_at marker
    let zombie = pages.iter().find(|p| p.slug == "zombie").expect("zombie must be present");
    assert!(zombie.deleted_at.is_some(), "soft-deleted row must have deleted_at set");

    engine.disconnect().await.expect("disconnect");
}

// ─── T5-6: sort ──────────────────────────────────────────────────────────

#[tokio::test]
async fn list_pages_sort_by_updated_desc_default() {
    // Insert pages, then verify that the default sort (UpdatedDesc) returns
    // them with the most-recently-updated first.  We insert in order and
    // rely on the auto-timestamp to give each row a distinct updated_at.
    //
    // NOTE: SQLite's `CURRENT_TIMESTAMP` resolution is **1 second**
    // (`YYYY-MM-DD HH:MM:SS`).  To guarantee distinct updated_at values we
    // sleep 1.1 s between inserts.  A future schema upgrade can switch to
    // `strftime('%Y-%m-%d %H:%M:%f', 'now')` for millisecond precision.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("first", &note_input("First", "b"))
        .await
        .expect("put_page first");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    engine
        .put_page("second", &note_input("Second", "b"))
        .await
        .expect("put_page second");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    engine
        .put_page("third", &note_input("Third", "b"))
        .await
        .expect("put_page third");

    let filters = PageFilters {
        sort: Some(PageSort::UpdatedDesc),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages sort=UpdatedDesc");

    assert_eq!(pages.len(), 3);
    // Most recently inserted/updated should come first
    assert_eq!(pages[0].slug, "third", "UpdatedDesc: newest first");
    assert_eq!(pages[2].slug, "first", "UpdatedDesc: oldest last");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_sort_by_slug_asc() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("charlie", &note_input("C", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("alpha", &note_input("A", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("bravo", &note_input("B", "b"))
        .await
        .expect("put_page");

    let filters = PageFilters {
        sort: Some(PageSort::Slug),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages sort=Slug");

    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0].slug, "alpha", "Slug sort: alpha first");
    assert_eq!(pages[1].slug, "bravo", "Slug sort: bravo second");
    assert_eq!(pages[2].slug, "charlie", "Slug sort: charlie third");

    engine.disconnect().await.expect("disconnect");
}

// ─── T5-7: combined filters ──────────────────────────────────────────────

#[tokio::test]
async fn list_pages_combined_page_type_limit_offset_sort() {
    // Insert 2 notes + 2 topics, filter to notes, apply limit + offset + sort
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("note-d", &note_input("D", "b"))
        .await
        .expect("put_page");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    engine
        .put_page("topic-x", &topic_input("X", "b"))
        .await
        .expect("put_page");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    engine
        .put_page("note-e", &note_input("E", "b"))
        .await
        .expect("put_page");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    engine
        .put_page("topic-y", &topic_input("Y", "b"))
        .await
        .expect("put_page");

    let filters = PageFilters {
        page_type: Some("note".to_string()),
        sort: Some(PageSort::Slug),
        offset: Some(1),
        limit: Some(10),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages combined");

    // Notes sorted by slug: [note-d, note-e], offset 1 → [note-e]
    assert_eq!(pages.len(), 1, "2 notes minus offset 1 = 1");
    assert_eq!(pages[0].slug, "note-e", "after skipping note-d, only note-e remains");
    assert_eq!(pages[0].page_type, "note", "result must match page_type filter");

    engine.disconnect().await.expect("disconnect");
}

// ─── S6-T5b: slug_prefix filter ─────────────────────────────────────────

#[tokio::test]
async fn list_pages_filters_by_slug_prefix() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("docs/readme", &note_input("Readme", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("docs/changelog", &note_input("Changelog", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("src/main", &note_input("Main", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("src/lib", &note_input("Lib", "b"))
        .await
        .expect("put_page");

    // Filter: slug_prefix = "docs/"
    let filters = PageFilters {
        slug_prefix: Some("docs/".to_string()),
        ..Default::default()
    };

    let docs = engine
        .list_pages(&filters)
        .await
        .expect("list_pages slug_prefix=docs/");

    assert_eq!(docs.len(), 2, "only 'docs/' prefixed slugs should appear");
    assert!(
        docs.iter().all(|p| p.slug.starts_with("docs/")),
        "all results must start with 'docs/'"
    );

    // Filter: slug_prefix = "src/"
    let filters_src = PageFilters {
        slug_prefix: Some("src/".to_string()),
        ..Default::default()
    };

    let srcs = engine
        .list_pages(&filters_src)
        .await
        .expect("list_pages slug_prefix=src/");

    assert_eq!(srcs.len(), 2, "only 'src/' prefixed slugs should appear");

    // No filter → all 4
    let all = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages all");

    assert_eq!(all.len(), 4, "unfiltered must return all 4 pages");

    engine.disconnect().await.expect("disconnect");
}

// ─── S6-T5b: source_id filter ───────────────────────────────────────────

#[tokio::test]
async fn list_pages_filters_by_source_id() {
    // `put_page` always inserts source_id = 'default' (schema default).
    // To test source_id filtering we need rows with different source_ids,
    // so we inject them via a raw libsql connection on the same file.
    let (engine, tmp) = init_clean_engine().await;

    // Insert via put_page (source_id = 'default')
    engine
        .put_page("default-page", &note_input("Default", "b"))
        .await
        .expect("put_page default");

    // Inject a row with source_id = 'wiki' via raw SQL
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "INSERT INTO sources (id, name) VALUES ('wiki', 'wiki')",
            (),
        )
        .await
        .expect("inject wiki source");
    raw_conn
        .execute(
            "INSERT INTO pages (slug, type, title, compiled_truth, source_id) \
             VALUES ('wiki-page', 'note', 'Wiki', 'w', 'wiki')",
            (),
        )
        .await
        .expect("inject wiki row");

    // Filter: source_id = 'default'
    let filters_default = PageFilters {
        source_id: Some("default".to_string()),
        ..Default::default()
    };

    let default_pages = engine
        .list_pages(&filters_default)
        .await
        .expect("list_pages source_id=default");

    assert_eq!(default_pages.len(), 1, "only default-source page should appear");
    assert_eq!(default_pages[0].slug, "default-page");

    // Filter: source_id = 'wiki'
    let filters_wiki = PageFilters {
        source_id: Some("wiki".to_string()),
        ..Default::default()
    };

    let wiki_pages = engine
        .list_pages(&filters_wiki)
        .await
        .expect("list_pages source_id=wiki");

    assert_eq!(wiki_pages.len(), 1, "only wiki-source page should appear");
    assert_eq!(wiki_pages[0].slug, "wiki-page");

    // No filter → both
    let all = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages all");

    assert_eq!(all.len(), 2, "unfiltered must return all 2 pages");

    engine.disconnect().await.expect("disconnect");
}

// ─── S6-T5b: source_ids filter (IN clause) ──────────────────────────────

#[tokio::test]
async fn list_pages_filters_by_source_ids() {
    let (engine, tmp) = init_clean_engine().await;

    // Insert default row via put_page
    engine
        .put_page("default-page", &note_input("Default", "b"))
        .await
        .expect("put_page default");

    // Inject rows with source_id = 'wiki' and 'notion' via raw SQL
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let raw_conn = db.connect().expect("raw conn");
    raw_conn
        .execute(
            "INSERT INTO sources (id, name) VALUES ('wiki', 'wiki')",
            (),
        )
        .await
        .expect("inject wiki source");
    raw_conn
        .execute(
            "INSERT INTO sources (id, name) VALUES ('notion', 'notion')",
            (),
        )
        .await
        .expect("inject notion source");
    raw_conn
        .execute(
            "INSERT INTO pages (slug, type, title, compiled_truth, source_id) \
             VALUES ('wiki-page', 'note', 'Wiki', 'w', 'wiki')",
            (),
        )
        .await
        .expect("inject wiki row");
    raw_conn
        .execute(
            "INSERT INTO pages (slug, type, title, compiled_truth, source_id) \
             VALUES ('notion-page', 'note', 'Notion', 'n', 'notion')",
            (),
        )
        .await
        .expect("inject notion row");

    // Filter: source_ids = ['wiki', 'notion']
    let filters = PageFilters {
        source_ids: Some(vec!["wiki".to_string(), "notion".to_string()]),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages source_ids=[wiki,notion]");

    assert_eq!(pages.len(), 2, "only wiki + notion pages should appear");
    let slugs: Vec<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    assert!(slugs.contains(&"wiki-page"), "wiki-page must be present");
    assert!(slugs.contains(&"notion-page"), "notion-page must be present");
    assert!(!slugs.contains(&"default-page"), "default-page must be excluded");

    // source_ids = [] should return nothing (empty IN set)
    let empty_filters = PageFilters {
        source_ids: Some(vec![]),
        ..Default::default()
    };

    let empty = engine
        .list_pages(&empty_filters)
        .await
        .expect("list_pages source_ids=[]");

    assert_eq!(empty.len(), 0, "empty source_ids must return 0 rows");

    engine.disconnect().await.expect("disconnect");
}

// ─── S6-T5b: updated_after filter ───────────────────────────────────────

#[tokio::test]
async fn list_pages_filters_by_updated_after() {
    // SQLite CURRENT_TIMESTAMP is second-precision. We insert a row,
    // capture its updated_at, sleep 1.1s, insert another, then filter
    // by the first row's timestamp to isolate the second.
    let (engine, _tmp) = init_clean_engine().await;

    engine
        .put_page("old-page", &note_input("Old", "b"))
        .await
        .expect("put_page old");

    // Read old-page's updated_at via get_page
    let old_page = engine
        .get_page("old-page", &zbrain_core::engine::GetPageOpts::default())
        .await
        .expect("get_page old")
        .expect("old page must exist");
    let cutoff = old_page.updated_at.clone();

    // Sleep past the second boundary so the next insert gets a later timestamp
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    engine
        .put_page("new-page", &note_input("New", "b"))
        .await
        .expect("put_page new");

    // Filter: updated_after = old_page's updated_at
    let filters = PageFilters {
        updated_after: Some(cutoff),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages updated_after");

    // "updated_after" means strictly after, so old-page should NOT appear
    // if both share the same second. With 1.1s sleep, new-page has a later
    // timestamp and must be the only result.
    assert!(
        pages.iter().any(|p| p.slug == "new-page"),
        "new-page must appear (updated_at > cutoff)"
    );
    assert!(
        !pages.iter().any(|p| p.slug == "old-page"),
        "old-page must NOT appear (updated_at = cutoff, not strictly after)"
    );

    engine.disconnect().await.expect("disconnect");
}
