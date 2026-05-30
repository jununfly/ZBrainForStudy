//! Slice 6a S6-T5 + S6-T5b + S6-T5c — `LibsqlEngine::list_pages` integration tests.
//!
//! Upgrades `list_pages` from a 7-column stub (`row_to_page`) to the full
//! 30-column projection (`full_row_to_page`) with 10 filter dimensions:
//!
//! | #  | Filter          | Slice  | Field                          |
//! |----|-----------------|--------|--------------------------------|
//! |  1 | `page_type`       | S6-T5  | `PageFilters::page_type`       |
//! |  2 | `limit`           | S6-T5  | `PageFilters::limit`           |
//! |  3 | `offset`          | S6-T5  | `PageFilters::offset`          |
//! |  4 | `include_deleted` | S6-T5  | `PageFilters::include_deleted` |
//! |  5 | `sort`            | S6-T5  | `PageFilters::sort`            |
//! |  6 | `slug_prefix`     | S6-T5b | `PageFilters::slug_prefix`     |
//! |  7 | `source_id`       | S6-T5b | `PageFilters::source_id`       |
//! |  8 | `source_ids`      | S6-T5b | `PageFilters::source_ids`      |
//! |  9 | `updated_after`   | S6-T5b | `PageFilters::updated_after`   |
//! | 10 | `tag`             | S6-T5c | `PageFilters::tag`             |
//!
//! Tag filter mirrors the TS `PGLite` prototype (single-tag exact match via
//! INNER JOIN on `page_tags`). S6-T5c adds migration 0004 (`page_tags` table
//! with composite PK + FK CASCADE) and enables `PRAGMA foreign_keys = ON`
//! per-connection in `LibsqlEngine::conn()`.
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
        .put_page("proj-30col", None, &note_input("Thirty", "body"))
        .await
        .expect("put_page");

    let pages = engine
        .list_pages(&PageFilters::default())
        .await
        .expect("list_pages");

    assert_eq!(pages.len(), 1, "should return exactly 1 page");
    let p = &pages[0];

    // Columns that the 7-col stub cannot populate:
    assert_eq!(
        p.source_id, "default",
        "source_id must be 'default' from schema"
    );
    assert!(
        matches!(p.page_kind, zbrain_core::PageKind::Markdown),
        "page_kind must be Markdown (schema default)"
    );
    assert!(
        p.created_at.len() > 8,
        "created_at must be a real timestamp, got '{}'",
        p.created_at
    );
    assert!(
        p.updated_at.len() > 8,
        "updated_at must be a real timestamp, got '{}'",
        p.updated_at
    );
    assert!(
        p.deleted_at.is_none(),
        "non-deleted row must have deleted_at = None"
    );
    assert!(p.content_hash.is_none(), "content_hash default = None");

    engine.disconnect().await.expect("disconnect");
}

// ─── T5-2: page_type filter ──────────────────────────────────────────────

#[tokio::test]
async fn list_pages_filters_by_page_type() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("note-a", None, &note_input("NoteA", "n"))
        .await
        .expect("put_page note");
    engine
        .put_page("topic-b", None, &topic_input("TopicB", "t"))
        .await
        .expect("put_page topic");
    engine
        .put_page("note-c", None, &note_input("NoteC", "n2"))
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
    assert!(
        notes.iter().all(|p| p.page_type == "note"),
        "all results must be note type"
    );

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
            .put_page(
                &format!("lim-{i}"),
                None,
                &note_input(&format!("L{i}"), "b"),
            )
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
            .put_page(
                &format!("off-{i}"),
                None,
                &note_input(&format!("O{i}"), "b"),
            )
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
        .put_page("alive", None, &note_input("Alive", "b"))
        .await
        .expect("put_page alive");
    engine
        .put_page("zombie", None, &note_input("Zombie", "b"))
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

    assert_eq!(
        pages.len(),
        1,
        "soft-deleted rows must be excluded by default"
    );
    assert_eq!(pages[0].slug, "alive", "only the non-deleted page remains");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_includes_soft_deleted_when_flag_set() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alive", None, &note_input("Alive", "b"))
        .await
        .expect("put_page alive");
    engine
        .put_page("zombie", None, &note_input("Zombie", "b"))
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
    let zombie = pages
        .iter()
        .find(|p| p.slug == "zombie")
        .expect("zombie must be present");
    assert!(
        zombie.deleted_at.is_some(),
        "soft-deleted row must have deleted_at set"
    );

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
        .put_page("first", None, &note_input("First", "b"))
        .await
        .expect("put_page first");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    engine
        .put_page("second", None, &note_input("Second", "b"))
        .await
        .expect("put_page second");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    engine
        .put_page("third", None, &note_input("Third", "b"))
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
        .put_page("charlie", None, &note_input("C", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("alpha", None, &note_input("A", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("bravo", None, &note_input("B", "b"))
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
        .put_page("note-d", None, &note_input("D", "b"))
        .await
        .expect("put_page");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    engine
        .put_page("topic-x", None, &topic_input("X", "b"))
        .await
        .expect("put_page");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    engine
        .put_page("note-e", None, &note_input("E", "b"))
        .await
        .expect("put_page");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    engine
        .put_page("topic-y", None, &topic_input("Y", "b"))
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
    assert_eq!(
        pages[0].slug, "note-e",
        "after skipping note-d, only note-e remains"
    );
    assert_eq!(
        pages[0].page_type, "note",
        "result must match page_type filter"
    );

    engine.disconnect().await.expect("disconnect");
}

// ─── S6-T5b: slug_prefix filter ─────────────────────────────────────────

#[tokio::test]
async fn list_pages_filters_by_slug_prefix() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("docs/readme", None, &note_input("Readme", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("docs/changelog", None, &note_input("Changelog", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("src/main", None, &note_input("Main", "b"))
        .await
        .expect("put_page");
    engine
        .put_page("src/lib", None, &note_input("Lib", "b"))
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
        .put_page("default-page", None, &note_input("Default", "b"))
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
        .execute("INSERT INTO sources (id, name) VALUES ('wiki', 'wiki')", ())
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

    assert_eq!(
        default_pages.len(),
        1,
        "only default-source page should appear"
    );
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
        .put_page("default-page", None, &note_input("Default", "b"))
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
        .execute("INSERT INTO sources (id, name) VALUES ('wiki', 'wiki')", ())
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
    assert!(
        slugs.contains(&"notion-page"),
        "notion-page must be present"
    );
    assert!(
        !slugs.contains(&"default-page"),
        "default-page must be excluded"
    );

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
        .put_page("old-page", None, &note_input("Old", "b"))
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
        .put_page("new-page", None, &note_input("New", "b"))
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

// ─── S6-T5c: tag filter (JOIN on page_tags) ─────────────────────────────
//
// Test matrix (10 cases):
//  1. schema_creates_page_tags_table          — migration 0004 created the table
//  2. schema_page_tags_composite_pk           — (page_id, tag) PK prevents duplicates
//  3. schema_page_tags_cascade_on_page_delete — FK CASCADE fires when PRAGMA FK=ON
//  4. list_pages_filters_by_tag_basic         — single-tag exact match
//  5. list_pages_tag_filter_excludes_others   — pages without the tag are excluded
//  6. list_pages_tag_filter_no_dup_multi_tags — page with many tags → no duplicate rows
//  7. list_pages_tag_filter_unknown_tag       — non-existent tag → empty result
//  8. list_pages_tag_filter_combines_page_type — tag + page_type AND
//  9. list_pages_tag_filter_with_include_deleted — tag respects soft-delete
// 10. list_pages_tag_filter_with_limit_offset — tag + pagination

/// Helper: open a raw libsql connection to the same temp file, enabling FK.
/// Used to inject `page_tags` rows since `put_page` does not write tags (deferred
/// to S6-T6).
async fn raw_conn_for(tmp: &NamedTempFile) -> (::libsql::Database, ::libsql::Connection) {
    let path_str = tmp.path().to_string_lossy().into_owned();
    let db = ::libsql::Builder::new_local(&path_str)
        .build()
        .await
        .expect("raw db open");
    let conn = db.connect().expect("raw conn");
    conn.execute("PRAGMA foreign_keys = ON", ())
        .await
        .expect("enable FK on raw conn");
    (db, conn)
}

// --- Schema tests (1-3) ---

#[tokio::test]
async fn schema_creates_page_tags_table() {
    // Verify that init_schema creates the `page_tags` table.
    let (engine, tmp) = init_clean_engine().await;

    let (_, raw_conn) = raw_conn_for(&tmp).await;

    // Table must exist
    let mut rows = raw_conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='page_tags'",
            (),
        )
        .await
        .expect("query sqlite_master");
    let row = rows.next().await.expect("fetch row");
    assert!(
        row.is_some(),
        "page_tags table must exist after migration 0004"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn schema_page_tags_composite_pk() {
    // Verify that (page_id, tag) composite PK prevents duplicate entries.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("test-page", None, &note_input("Test", "b"))
        .await
        .expect("put_page");

    let (_, raw_conn) = raw_conn_for(&tmp).await;

    // Get the page id
    let mut rows = raw_conn
        .query("SELECT id FROM pages WHERE slug = 'test-page'", ())
        .await
        .expect("get page id");
    let row = rows.next().await.expect("fetch").expect("must exist");
    let page_id: i64 = row.get(0).expect("page_id");

    // First insert succeeds
    raw_conn
        .execute(
            "INSERT INTO page_tags (page_id, tag) VALUES (?, 'rust')",
            [page_id],
        )
        .await
        .expect("first tag insert");

    // Duplicate insert should fail (PK violation)
    let dup_result = raw_conn
        .execute(
            "INSERT INTO page_tags (page_id, tag) VALUES (?, 'rust')",
            [page_id],
        )
        .await;
    assert!(
        dup_result.is_err(),
        "duplicate (page_id, tag) must violate composite PK"
    );

    // Different tag for same page is fine
    raw_conn
        .execute(
            "INSERT INTO page_tags (page_id, tag) VALUES (?, 'ai')",
            [page_id],
        )
        .await
        .expect("second tag insert");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn schema_page_tags_cascade_on_page_delete() {
    // Verify ON DELETE CASCADE: deleting a page removes its page_tags rows.
    // This requires PRAGMA foreign_keys = ON, which conn() now enforces.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("doom-page", None, &note_input("Doom", "b"))
        .await
        .expect("put_page");

    let (_, raw_conn) = raw_conn_for(&tmp).await;

    // Get the page id
    let mut rows = raw_conn
        .query("SELECT id FROM pages WHERE slug = 'doom-page'", ())
        .await
        .expect("get page id");
    let row = rows.next().await.expect("fetch").expect("must exist");
    let page_id: i64 = row.get(0).expect("page_id");

    // Insert tags for this page
    raw_conn
        .execute(
            "INSERT INTO page_tags (page_id, tag) VALUES (?, 'cascade-test')",
            [page_id],
        )
        .await
        .expect("insert tag");

    // Hard-delete the page via raw SQL (bypassing soft-delete)
    raw_conn
        .execute("DELETE FROM pages WHERE id = ?", [page_id])
        .await
        .expect("hard delete page");

    // Verify tag row is gone (CASCADE)
    let mut rows = raw_conn
        .query(
            "SELECT COUNT(*) FROM page_tags WHERE page_id = ?",
            [page_id],
        )
        .await
        .expect("count tags");
    let row = rows.next().await.expect("fetch").expect("must exist");
    let count: i64 = row.get(0).expect("count");
    assert_eq!(count, 0, "page_tags rows must be cascade-deleted with page");

    engine.disconnect().await.expect("disconnect");
}

// --- Functional tests (4-10) ---

#[tokio::test]
async fn list_pages_filters_by_tag_basic() {
    // Basic tag filter: only pages with the specified tag should appear.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("tagged-page", None, &note_input("Tagged", "b"))
        .await
        .expect("put_page tagged");
    engine
        .put_page("untagged-page", None, &note_input("Untagged", "b"))
        .await
        .expect("put_page untagged");

    // Tag only the first page
    let (_, raw_conn) = raw_conn_for(&tmp).await;
    let mut rows = raw_conn
        .query("SELECT id FROM pages WHERE slug = 'tagged-page'", ())
        .await
        .expect("get page id");
    let row = rows.next().await.expect("fetch").expect("must exist");
    let page_id: i64 = row.get(0).expect("page_id");
    raw_conn
        .execute(
            "INSERT INTO page_tags (page_id, tag) VALUES (?, 'rust')",
            [page_id],
        )
        .await
        .expect("insert tag");

    let filters = PageFilters {
        tag: Some("rust".to_string()),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages tag=rust");

    assert_eq!(pages.len(), 1, "only tagged page should appear");
    assert_eq!(pages[0].slug, "tagged-page");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_tag_filter_excludes_others() {
    // Page with tag 'ai' should NOT appear when filtering for tag 'rust'.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("rust-page", None, &note_input("Rust", "b"))
        .await
        .expect("put_page rust");
    engine
        .put_page("ai-page", None, &note_input("AI", "b"))
        .await
        .expect("put_page ai");

    let (_, raw_conn) = raw_conn_for(&tmp).await;

    // Tag both pages with different tags
    for (slug, tag) in [("rust-page", "rust"), ("ai-page", "ai")] {
        let mut rows = raw_conn
            .query(&format!("SELECT id FROM pages WHERE slug = '{slug}'"), ())
            .await
            .expect("get page id");
        let row = rows.next().await.expect("fetch").expect("must exist");
        let page_id: i64 = row.get(0).expect("page_id");
        raw_conn
            .execute(
                "INSERT INTO page_tags (page_id, tag) VALUES (?, ?)",
                ::libsql::params![page_id, tag],
            )
            .await
            .expect("insert tag");
    }

    let filters = PageFilters {
        tag: Some("rust".to_string()),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages tag=rust");

    assert_eq!(pages.len(), 1, "only rust-tagged page");
    assert_eq!(pages[0].slug, "rust-page");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_tag_filter_no_dup_multi_tags() {
    // A page with 3 tags should still appear exactly once when filtering
    // for any one of those tags — composite PK + single-tag exact match
    // guarantees at most 1 matching row per page, so JOIN produces no dups.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("multi-page", None, &note_input("Multi", "b"))
        .await
        .expect("put_page");

    let (_, raw_conn) = raw_conn_for(&tmp).await;

    let mut rows = raw_conn
        .query("SELECT id FROM pages WHERE slug = 'multi-page'", ())
        .await
        .expect("get page id");
    let row = rows.next().await.expect("fetch").expect("must exist");
    let page_id: i64 = row.get(0).expect("page_id");

    for tag in &["rust", "ai", "research"] {
        raw_conn
            .execute(
                "INSERT INTO page_tags (page_id, tag) VALUES (?, ?)",
                ::libsql::params![page_id, tag],
            )
            .await
            .expect("insert tag");
    }

    let filters = PageFilters {
        tag: Some("rust".to_string()),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages tag=rust");

    assert_eq!(pages.len(), 1, "multi-tagged page appears exactly once");
    assert_eq!(pages[0].slug, "multi-page");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_tag_filter_unknown_tag() {
    // Filtering by a tag that no page has → empty result.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("some-page", None, &note_input("Some", "b"))
        .await
        .expect("put_page");

    let (_, raw_conn) = raw_conn_for(&tmp).await;
    let mut rows = raw_conn
        .query("SELECT id FROM pages WHERE slug = 'some-page'", ())
        .await
        .expect("get page id");
    let row = rows.next().await.expect("fetch").expect("must exist");
    let page_id: i64 = row.get(0).expect("page_id");
    raw_conn
        .execute(
            "INSERT INTO page_tags (page_id, tag) VALUES (?, 'existing')",
            [page_id],
        )
        .await
        .expect("insert tag");

    let filters = PageFilters {
        tag: Some("nonexistent".to_string()),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages tag=nonexistent");

    assert_eq!(pages.len(), 0, "unknown tag must return empty");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_tag_filter_combines_page_type() {
    // tag + page_type AND: only pages matching BOTH should appear.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("note-rust", None, &note_input("Note Rust", "b"))
        .await
        .expect("put_page note");
    engine
        .put_page("topic-rust", None, &topic_input("Topic Rust", "b"))
        .await
        .expect("put_page topic");

    let (_, raw_conn) = raw_conn_for(&tmp).await;

    for slug in &["note-rust", "topic-rust"] {
        let mut rows = raw_conn
            .query(&format!("SELECT id FROM pages WHERE slug = '{slug}'"), ())
            .await
            .expect("get page id");
        let row = rows.next().await.expect("fetch").expect("must exist");
        let page_id: i64 = row.get(0).expect("page_id");
        raw_conn
            .execute(
                "INSERT INTO page_tags (page_id, tag) VALUES (?, 'rust')",
                [page_id],
            )
            .await
            .expect("insert tag");
    }

    // Filter: tag=rust AND page_type=note
    let filters = PageFilters {
        tag: Some("rust".to_string()),
        page_type: Some("note".to_string()),
        ..Default::default()
    };

    let pages = engine
        .list_pages(&filters)
        .await
        .expect("list_pages tag=rust + page_type=note");

    assert_eq!(pages.len(), 1, "only note+rust page should appear");
    assert_eq!(pages[0].slug, "note-rust");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_tag_filter_with_include_deleted() {
    // Tag filter should respect include_deleted semantics:
    // soft-deleted page should be excluded by default, included when flagged.
    let (engine, tmp) = init_clean_engine().await;

    engine
        .put_page("alive-page", None, &note_input("Alive", "b"))
        .await
        .expect("put_page alive");
    engine
        .put_page("deleted-page", None, &note_input("Deleted", "b"))
        .await
        .expect("put_page deleted");

    // Soft-delete one page
    engine
        .soft_delete_page("deleted-page", None)
        .await
        .expect("soft delete");

    // Tag both pages
    let (_, raw_conn) = raw_conn_for(&tmp).await;
    for slug in &["alive-page", "deleted-page"] {
        let mut rows = raw_conn
            .query(&format!("SELECT id FROM pages WHERE slug = '{slug}'"), ())
            .await
            .expect("get page id");
        let row = rows.next().await.expect("fetch").expect("must exist");
        let page_id: i64 = row.get(0).expect("page_id");
        raw_conn
            .execute(
                "INSERT INTO page_tags (page_id, tag) VALUES (?, 'rust')",
                [page_id],
            )
            .await
            .expect("insert tag");
    }

    // Default: exclude soft-deleted
    let filters_default = PageFilters {
        tag: Some("rust".to_string()),
        ..Default::default()
    };
    let pages = engine
        .list_pages(&filters_default)
        .await
        .expect("list_pages tag=rust (default)");
    assert_eq!(pages.len(), 1, "soft-deleted page excluded by default");
    assert_eq!(pages[0].slug, "alive-page");

    // include_deleted = true
    let filters_include = PageFilters {
        tag: Some("rust".to_string()),
        include_deleted: true,
        ..Default::default()
    };
    let pages_all = engine
        .list_pages(&filters_include)
        .await
        .expect("list_pages tag=rust + include_deleted");
    assert_eq!(pages_all.len(), 2, "both pages with include_deleted=true");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn list_pages_tag_filter_with_limit_offset() {
    // Tag filter + limit/offset: pagination works on tag-filtered results.
    let (engine, tmp) = init_clean_engine().await;

    // Insert 3 pages, all tagged 'rust'
    for i in 0..3 {
        engine
            .put_page(
                &format!("page-{i}"),
                None,
                &note_input(&format!("P{i}"), "b"),
            )
            .await
            .expect("put_page");
    }

    let (_, raw_conn) = raw_conn_for(&tmp).await;
    for i in 0..3 {
        let slug = format!("page-{i}");
        let mut rows = raw_conn
            .query(&format!("SELECT id FROM pages WHERE slug = '{slug}'"), ())
            .await
            .expect("get page id");
        let row = rows.next().await.expect("fetch").expect("must exist");
        let page_id: i64 = row.get(0).expect("page_id");
        raw_conn
            .execute(
                "INSERT INTO page_tags (page_id, tag) VALUES (?, 'rust')",
                [page_id],
            )
            .await
            .expect("insert tag");
    }

    // Page 1: limit=2, offset=0
    let page1 = engine
        .list_pages(&PageFilters {
            tag: Some("rust".to_string()),
            limit: Some(2),
            ..Default::default()
        })
        .await
        .expect("list_pages tag=rust limit=2");
    assert_eq!(page1.len(), 2, "first page should have 2 results");

    // Page 2: limit=2, offset=2
    let page2 = engine
        .list_pages(&PageFilters {
            tag: Some("rust".to_string()),
            limit: Some(2),
            offset: Some(2),
            ..Default::default()
        })
        .await
        .expect("list_pages tag=rust limit=2 offset=2");
    assert_eq!(page2.len(), 1, "second page should have 1 result");

    engine.disconnect().await.expect("disconnect");
}
