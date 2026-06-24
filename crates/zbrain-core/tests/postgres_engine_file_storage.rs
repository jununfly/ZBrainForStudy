mod support;

use serde_json::json;
use zbrain_core::engine::{BrainEngine, PageInput};
use zbrain_core::FileSpec;

fn note_input(title: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: title.to_string(),
        compiled_truth: "body".to_string(),
        ..PageInput::default()
    }
}

fn file_spec(storage_path: &str, content_hash: &str) -> FileSpec {
    FileSpec {
        source_id: None,
        page_slug: None,
        page_id: None,
        filename: "photo.jpg".to_string(),
        storage_path: storage_path.to_string(),
        mime_type: Some("image/jpeg".to_string()),
        size_bytes: Some(12345),
        content_hash: content_hash.to_string(),
        metadata: Some(json!({"alt": "Photo"})),
    }
}

#[tokio::test]
async fn postgres_upsert_file_inserts_and_get_file_round_trips_metadata() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    let inserted = engine
        .upsert_file(&file_spec("originals/photos/photo.jpg", "sha256:abc"))
        .await
        .expect("upsert file");
    assert!(inserted.created, "first upsert should create a row");
    assert!(inserted.id > 0, "file id must be assigned");

    let row = engine
        .get_file("default", "originals/photos/photo.jpg")
        .await
        .expect("get file")
        .expect("file exists");

    assert_eq!(row.id, inserted.id);
    assert_eq!(row.source_id, "default");
    assert_eq!(row.filename, "photo.jpg");
    assert_eq!(row.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(row.size_bytes, Some(12345));
    assert_eq!(row.content_hash, "sha256:abc");
    assert_eq!(row.metadata, json!({"alt": "Photo"}));
}

#[tokio::test]
async fn postgres_upsert_file_updates_existing_storage_path_in_place() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;

    let first = engine
        .upsert_file(&file_spec("originals/photos/photo.jpg", "sha256:v1"))
        .await
        .expect("first upsert");
    let mut replacement = file_spec("originals/photos/photo.jpg", "sha256:v2");
    replacement.size_bytes = Some(9999);
    let second = engine
        .upsert_file(&replacement)
        .await
        .expect("second upsert");

    assert_eq!(second.id, first.id, "upsert must keep stable id");
    assert!(!second.created, "second upsert should update in place");
    let row = engine
        .get_file("default", "originals/photos/photo.jpg")
        .await
        .expect("get file")
        .expect("file exists");
    assert_eq!(row.content_hash, "sha256:v2");
    assert_eq!(row.size_bytes, Some(9999));
}

#[tokio::test]
async fn postgres_get_file_is_source_and_path_scoped() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    let mut spec = file_spec("photos/a.jpg", "sha256:a");
    spec.source_id = Some("default".to_string());
    engine.upsert_file(&spec).await.expect("upsert file");

    assert!(engine
        .get_file("default", "photos/a.jpg")
        .await
        .expect("matching source/path")
        .is_some());
    assert!(engine
        .get_file("src-2", "photos/a.jpg")
        .await
        .expect("wrong source")
        .is_none());
    assert!(engine
        .get_file("default", "photos/missing.jpg")
        .await
        .expect("wrong path")
        .is_none());
}

#[tokio::test]
async fn postgres_list_files_for_page_returns_only_matching_page_id() {
    let fix = support::pg_fixture::PgFixture::start().await;
    let engine = &fix.engine;
    let page = engine
        .put_page("page-7", None, &note_input("Page 7"))
        .await
        .expect("seed page 7");
    let other_page = engine
        .put_page("page-8", None, &note_input("Page 8"))
        .await
        .expect("seed page 8");
    let mut first = file_spec("photos/page-7-a.jpg", "sha256:a");
    first.page_id = Some(page.id);
    first.page_slug = Some(page.slug.clone());
    first.filename = "a.jpg".to_string();
    let mut second = file_spec("photos/page-7-b.jpg", "sha256:b");
    second.page_id = Some(page.id);
    second.page_slug = Some(page.slug.clone());
    second.filename = "b.jpg".to_string();
    let mut other = file_spec("photos/page-8.jpg", "sha256:c");
    other.page_id = Some(other_page.id);
    other.filename = "c.jpg".to_string();

    engine.upsert_file(&first).await.expect("upsert first");
    engine.upsert_file(&second).await.expect("upsert second");
    engine.upsert_file(&other).await.expect("upsert other");

    let mut filenames: Vec<String> = engine
        .list_files_for_page(page.id)
        .await
        .expect("list files")
        .into_iter()
        .map(|file| file.filename)
        .collect();
    filenames.sort();
    assert_eq!(filenames, vec!["a.jpg", "b.jpg"]);
}
