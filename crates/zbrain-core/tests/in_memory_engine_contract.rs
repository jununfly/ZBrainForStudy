use std::time::Duration;

use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine, PageInput};
use zbrain_core::PurgeResult;

async fn init_in_memory() -> InMemoryEngine {
    let engine = InMemoryEngine::default();
    engine
        .connect(&EngineConfig::default())
        .await
        .expect("connect");
    engine.init_schema().await.expect("init_schema");
    engine
}

fn page_input(slug: &str) -> PageInput {
    PageInput {
        page_type: "note".to_string(),
        title: slug.to_string(),
        compiled_truth: format!("truth for {slug}"),
        ..PageInput::default()
    }
}

fn get_opts(source_id: &str, include_deleted: bool) -> GetPageOpts {
    GetPageOpts {
        source_id: Some(source_id.to_string()),
        include_deleted,
    }
}

fn assert_purge_result(actual: &PurgeResult, expected_sorted: &[&str]) {
    let mut got = actual.slugs.clone();
    got.sort();
    let want: Vec<String> = expected_sorted.iter().map(ToString::to_string).collect();
    assert_eq!(got, want, "purged slug set mismatch");
    assert_eq!(
        actual.count,
        expected_sorted.len() as u64,
        "PurgeResult.count must match returned slugs length"
    );
}

#[tokio::test]
async fn in_memory_restore_page_restores_soft_deleted_exact_source() {
    let engine = init_in_memory().await;
    let slug = "restore-me";
    let source_id = "src-1";

    engine
        .put_page(slug, Some(source_id), &page_input(slug))
        .await
        .expect("seed page");
    engine
        .soft_delete_page(slug, Some(source_id))
        .await
        .expect("soft delete page");
    assert!(
        engine
            .get_page(slug, &get_opts(source_id, false))
            .await
            .expect("get hidden deleted page")
            .is_none(),
        "precondition: soft-deleted page is hidden from normal reads"
    );
    let deleted_page = engine
        .get_page(slug, &get_opts(source_id, true))
        .await
        .expect("get deleted page")
        .expect("deleted page exists with include_deleted");
    assert!(
        deleted_page.deleted_at.is_some(),
        "precondition: deleted_at is set before restore"
    );
    let deleted_updated_at = deleted_page.updated_at.clone();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let restored = engine
        .restore_page(slug, Some(source_id))
        .await
        .expect("restore page");

    assert!(restored, "soft-deleted exact-source row should restore");
    let page = engine
        .get_page(slug, &get_opts(source_id, false))
        .await
        .expect("get restored page")
        .expect("restored page is visible");
    assert_eq!(page.deleted_at, None, "restore must clear deleted_at");
    assert_ne!(
        page.updated_at, deleted_updated_at,
        "restore must refresh updated_at"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_restore_page_returns_false_for_live_missing_or_wrong_source() {
    let engine = init_in_memory().await;

    engine
        .put_page("live-row", Some("src-1"), &page_input("live-row"))
        .await
        .expect("seed live page");
    engine
        .put_page("scoped-row", Some("src-1"), &page_input("scoped-row"))
        .await
        .expect("seed scoped page");
    engine
        .soft_delete_page("scoped-row", Some("src-1"))
        .await
        .expect("soft delete scoped page");

    let live = engine
        .restore_page("live-row", Some("src-1"))
        .await
        .expect("restore live row");
    let missing = engine
        .restore_page("missing-row", Some("src-1"))
        .await
        .expect("restore missing row");
    let wrong_source = engine
        .restore_page("scoped-row", Some("src-2"))
        .await
        .expect("restore wrong source");

    assert!(!live, "live row must not be restored");
    assert!(!missing, "missing row must not be restored");
    assert!(!wrong_source, "source mismatch must not restore");
    let still_deleted = engine
        .get_page("scoped-row", &get_opts("src-1", true))
        .await
        .expect("get scoped row")
        .expect("scoped row still exists");
    assert!(
        still_deleted.deleted_at.is_some(),
        "wrong-source restore must leave deleted_at set"
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_purge_deleted_pages_removes_deleted_rows_and_keeps_live_rows() {
    let engine = init_in_memory().await;

    for slug in ["deleted-a", "deleted-b", "live-row"] {
        engine
            .put_page(slug, Some("src-1"), &page_input(slug))
            .await
            .unwrap_or_else(|e| panic!("seed page {slug}: {e}"));
    }
    engine
        .soft_delete_page("deleted-a", Some("src-1"))
        .await
        .expect("soft delete deleted-a");
    engine
        .soft_delete_page("deleted-b", Some("src-1"))
        .await
        .expect("soft delete deleted-b");

    let result = engine
        .purge_deleted_pages(24)
        .await
        .expect("purge deleted pages");

    assert_purge_result(&result, &["deleted-a", "deleted-b"]);
    assert!(
        engine
            .get_page("deleted-a", &get_opts("src-1", true))
            .await
            .expect("get deleted-a after purge")
            .is_none(),
        "purge must remove deleted rows"
    );
    assert!(
        engine
            .get_page("deleted-b", &get_opts("src-1", true))
            .await
            .expect("get deleted-b after purge")
            .is_none(),
        "purge must remove deleted rows"
    );
    assert!(
        engine
            .get_page("live-row", &get_opts("src-1", false))
            .await
            .expect("get live row after purge")
            .is_some(),
        "purge must keep live rows"
    );
    engine.disconnect().await.expect("disconnect");
}

// ── Tag contract tests (C1 Task 2) ──────────────────────────────────────────

#[tokio::test]
async fn in_memory_tags_are_idempotent_sorted_and_source_scoped() {
    let engine = init_in_memory().await;

    // Create page "tagged" under source "src-1"
    engine
        .put_page("tagged", Some("src-1"), &page_input("tagged"))
        .await
        .expect("seed tagged page src-1");

    // Add tags — "beta" then "alpha" to test sort, then duplicate "beta" for idempotent
    engine
        .add_tag("tagged", "beta", Some("src-1"))
        .await
        .expect("add beta");
    engine
        .add_tag("tagged", "alpha", Some("src-1"))
        .await
        .expect("add alpha");
    engine
        .add_tag("tagged", "beta", Some("src-1"))
        .await
        .expect("add beta again (idempotent)");

    // get_tags should return sorted, deduped
    let tags_src1 = engine
        .get_tags("tagged", Some("src-1"))
        .await
        .expect("get tags src-1");
    assert_eq!(
        tags_src1,
        vec!["alpha", "beta"],
        "tags must be sorted and deduped"
    );

    // Create page "tagged" under source "src-2" with same slug but different source
    engine
        .put_page("tagged", Some("src-2"), &page_input("tagged"))
        .await
        .expect("seed tagged page src-2");
    engine
        .add_tag("tagged", "gamma", Some("src-2"))
        .await
        .expect("add gamma src-2");

    // Source scoped: src-1 still only has alpha, beta
    let tags_src1_again = engine
        .get_tags("tagged", Some("src-1"))
        .await
        .expect("get tags src-1 after src-2 add");
    assert_eq!(
        tags_src1_again,
        vec!["alpha", "beta"],
        "src-1 tags unaffected by src-2"
    );

    // src-2 has only gamma
    let tags_src2 = engine
        .get_tags("tagged", Some("src-2"))
        .await
        .expect("get tags src-2");
    assert_eq!(tags_src2, vec!["gamma"], "src-2 tags are independent");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_remove_tag_is_silent_for_absent_tag_and_missing_page() {
    let engine = init_in_memory().await;

    // Create page "r-tag" under source "src-1"
    engine
        .put_page("r-tag", Some("src-1"), &page_input("r-tag"))
        .await
        .expect("seed r-tag page");
    engine
        .add_tag("r-tag", "keep", Some("src-1"))
        .await
        .expect("add keep tag");

    // Remove absent tag — should return Ok(()) silently
    engine
        .remove_tag("r-tag", "absent", Some("src-1"))
        .await
        .expect("remove absent tag should be silent");

    // Remove tag from missing page — should return Ok(()) silently
    engine
        .remove_tag("missing", "any", Some("src-1"))
        .await
        .expect("remove tag from missing page should be silent");

    // Only the kept tag remains
    let tags = engine
        .get_tags("r-tag", Some("src-1"))
        .await
        .expect("get tags after remove");
    assert_eq!(tags, vec!["keep"], "only the kept tag remains");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_add_tag_ignores_soft_deleted_pages() {
    let engine = init_in_memory().await;

    // Create page "del-tag" under source "src-1", then soft-delete it
    engine
        .put_page("del-tag", Some("src-1"), &page_input("del-tag"))
        .await
        .expect("seed del-tag page");
    engine
        .soft_delete_page("del-tag", Some("src-1"))
        .await
        .expect("soft delete del-tag");

    // add_tag on soft-deleted page should return Err (page not found)
    let result = engine.add_tag("del-tag", "ghost", Some("src-1")).await;
    assert!(
        result.is_err(),
        "add_tag on soft-deleted page must return Err"
    );

    // get_tags on soft-deleted page should return Ok([]) — no tags on deleted page
    let tags = engine
        .get_tags("del-tag", Some("src-1"))
        .await
        .expect("get tags on deleted page");
    assert_eq!(tags, Vec::<String>::new(), "deleted page has no tags");

    engine.disconnect().await.expect("disconnect");
}
