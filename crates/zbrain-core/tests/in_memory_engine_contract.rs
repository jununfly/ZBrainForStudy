use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;
use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts, InMemoryEngine, PageInput};
use zbrain_core::types::{CRMode, OrphanPage, PageRef, RefreshPageBodyArgs};
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

// ── Content refresh contract tests (C1 Task 3) ──────────────────────────────

#[tokio::test]
async fn in_memory_refresh_page_body_updates_fields_and_noops_for_missing_or_deleted() {
    let engine = init_in_memory().await;
    let slug = "refresh-me";
    let source_id = "src-1";

    engine
        .put_page(slug, Some(source_id), &page_input(slug))
        .await
        .expect("seed refresh-me page");

    let args = RefreshPageBodyArgs {
        slug: slug.to_string(),
        source_id: source_id.to_string(),
        compiled_truth: "updated truth".to_string(),
        timeline: serde_json::json!([{"event": "reimported"}]),
        content_hash: "sha256:abc123".to_string(),
    };

    engine
        .refresh_page_body(&args)
        .await
        .expect("refresh page body");

    let page = engine
        .get_page(slug, &get_opts(source_id, false))
        .await
        .expect("get refreshed page")
        .expect("refreshed page exists");
    assert_eq!(
        page.compiled_truth, "updated truth",
        "refresh must update compiled_truth"
    );
    assert_eq!(
        page.timeline,
        serde_json::json!([{"event":"reimported"}]).to_string(),
        "refresh must store timeline as JSON string"
    );
    assert_eq!(
        page.content_hash,
        Some("sha256:abc123".to_string()),
        "refresh must wrap content_hash into Some"
    );

    // Missing slug → silent Ok(())
    let missing_args = RefreshPageBodyArgs {
        slug: "no-such".to_string(),
        source_id: source_id.to_string(),
        compiled_truth: "ignored".to_string(),
        timeline: serde_json::json!([]),
        content_hash: "sha256:ignored".to_string(),
    };
    engine
        .refresh_page_body(&missing_args)
        .await
        .expect("refresh on missing slug is silent no-op");

    // Soft-delete the page, then attempt refresh → silent Ok(()) and fields unchanged
    engine
        .soft_delete_page(slug, Some(source_id))
        .await
        .expect("soft delete refresh-me");
    let before_attempt = engine
        .get_page(slug, &get_opts(source_id, true))
        .await
        .expect("get soft-deleted refresh-me")
        .expect("deleted page still in store");

    let post_delete_args = RefreshPageBodyArgs {
        slug: slug.to_string(),
        source_id: source_id.to_string(),
        compiled_truth: "should not apply".to_string(),
        timeline: serde_json::json!([{"event": "ignored"}]),
        content_hash: "sha256:should-not-apply".to_string(),
    };
    engine
        .refresh_page_body(&post_delete_args)
        .await
        .expect("refresh on soft-deleted page is silent no-op");

    let after_attempt = engine
        .get_page(slug, &get_opts(source_id, true))
        .await
        .expect("get soft-deleted refresh-me after refresh attempt")
        .expect("deleted page still in store");
    assert_eq!(
        after_attempt.compiled_truth, before_attempt.compiled_truth,
        "soft-deleted compiled_truth must be unchanged"
    );
    assert_eq!(
        after_attempt.timeline, before_attempt.timeline,
        "soft-deleted timeline must be unchanged"
    );
    assert_eq!(
        after_attempt.content_hash, before_attempt.content_hash,
        "soft-deleted content_hash must be unchanged"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_update_cr_state_updates_fields_and_noops_for_missing_or_deleted() {
    let engine = init_in_memory().await;
    let slug = "cr-page";
    let source_id = "src-1";

    engine
        .put_page(slug, Some(source_id), &page_input(slug))
        .await
        .expect("seed cr-page");

    engine
        .update_page_contextual_retrieval_state(slug, source_id, "title", Some("gen-v2"))
        .await
        .expect("update cr state");

    let page = engine
        .get_page(slug, &get_opts(source_id, false))
        .await
        .expect("get cr-page")
        .expect("cr-page exists");
    assert_eq!(
        page.contextual_retrieval_mode,
        Some(CRMode::Title),
        "update must decode mode into CRMode::Title"
    );
    assert_eq!(
        page.corpus_generation,
        Some("gen-v2".to_string()),
        "update must store corpus_generation"
    );

    // Missing slug → silent Ok(())
    engine
        .update_page_contextual_retrieval_state("no-such", source_id, "none", None)
        .await
        .expect("update on missing slug is silent no-op");

    // Invalid mode → Err
    let bad_result = engine
        .update_page_contextual_retrieval_state(slug, source_id, "bad-mode", None)
        .await;
    assert!(
        bad_result.is_err(),
        "invalid contextual_retrieval_mode must return Err"
    );

    // Soft-delete the page, then attempt update → silent Ok(()) and fields unchanged
    engine
        .soft_delete_page(slug, Some(source_id))
        .await
        .expect("soft delete cr-page");
    let before_attempt = engine
        .get_page(slug, &get_opts(source_id, true))
        .await
        .expect("get soft-deleted cr-page")
        .expect("deleted cr-page still in store");

    engine
        .update_page_contextual_retrieval_state(
            slug,
            source_id,
            "per_chunk_synopsis",
            Some("gen-v3"),
        )
        .await
        .expect("update on soft-deleted page is silent no-op");

    let after_attempt = engine
        .get_page(slug, &get_opts(source_id, true))
        .await
        .expect("get soft-deleted cr-page after update attempt")
        .expect("deleted cr-page still in store");
    assert_eq!(
        after_attempt.contextual_retrieval_mode, before_attempt.contextual_retrieval_mode,
        "soft-deleted contextual_retrieval_mode must be unchanged"
    );
    assert_eq!(
        after_attempt.corpus_generation, before_attempt.corpus_generation,
        "soft-deleted corpus_generation must be unchanged"
    );

    engine.disconnect().await.expect("disconnect");
}

// ── Advanced-read contract tests (C1 Task 4) ─────────────────────────────────

#[tokio::test]
async fn in_memory_get_all_slugs_returns_all_including_soft_deleted() {
    let engine = init_in_memory().await;

    engine
        .put_page("alpha", Some("src-1"), &page_input("alpha"))
        .await
        .expect("seed alpha");
    engine
        .put_page("beta", Some("src-1"), &page_input("beta"))
        .await
        .expect("seed beta");
    engine
        .put_page("gamma", Some("src-2"), &page_input("gamma"))
        .await
        .expect("seed gamma src-2");
    engine
        .soft_delete_page("beta", Some("src-1"))
        .await
        .expect("soft delete beta");

    // source_id: None → all slugs including soft-deleted
    let all = engine
        .get_all_slugs(None)
        .await
        .expect("get_all_slugs None");
    let mut all_sorted: Vec<String> = all.into_iter().collect();
    all_sorted.sort();
    assert_eq!(
        all_sorted,
        vec!["alpha", "beta", "gamma"],
        "all slugs including soft-deleted"
    );

    // source_id: Some("src-1") → only src-1 slugs (including soft-deleted beta)
    let src1 = engine
        .get_all_slugs(Some("src-1"))
        .await
        .expect("get_all_slugs src-1");
    let mut src1_sorted: Vec<String> = src1.into_iter().collect();
    src1_sorted.sort();
    assert_eq!(
        src1_sorted,
        vec!["alpha", "beta"],
        "src-1 slugs include soft-deleted"
    );

    // source_id: Some("src-2") → only src-2 slugs
    let src2 = engine
        .get_all_slugs(Some("src-2"))
        .await
        .expect("get_all_slugs src-2");
    assert_eq!(src2, HashSet::from(["gamma".to_string()]), "src-2 slugs");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_list_all_page_refs_excludes_deleted_and_orders() {
    let engine = init_in_memory().await;

    engine
        .put_page("slug-b", Some("src-2"), &page_input("slug-b"))
        .await
        .expect("seed slug-b src-2");
    engine
        .put_page("slug-a", Some("src-1"), &page_input("slug-a"))
        .await
        .expect("seed slug-a src-1");
    engine
        .put_page("slug-c", Some("src-1"), &page_input("slug-c"))
        .await
        .expect("seed slug-c src-1");
    engine
        .put_page("slug-d", Some("src-2"), &page_input("slug-d"))
        .await
        .expect("seed slug-d src-2");
    // soft-delete slug-c — should be excluded
    engine
        .soft_delete_page("slug-c", Some("src-1"))
        .await
        .expect("soft delete slug-c");

    let refs = engine
        .list_all_page_refs()
        .await
        .expect("list_all_page_refs");
    assert_eq!(
        refs,
        vec![
            PageRef {
                slug: "slug-a".into(),
                source_id: "src-1".into()
            },
            PageRef {
                slug: "slug-b".into(),
                source_id: "src-2".into()
            },
            PageRef {
                slug: "slug-d".into(),
                source_id: "src-2".into()
            },
        ],
        "excludes soft-deleted, ordered by (source_id, slug)"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_find_orphan_pages_returns_live_pages_with_domain() {
    let engine = init_in_memory().await;

    // page with domain in frontmatter
    let with_domain = PageInput {
        page_type: "note".into(),
        title: "Titled Page".into(),
        compiled_truth: "truth".into(),
        frontmatter: Some(json!({"domain": "test-domain"})),
        ..PageInput::default()
    };
    engine
        .put_page("alpha", Some("src-1"), &with_domain)
        .await
        .expect("seed alpha with domain");

    // page with empty title → COALESCE falls back to slug
    let no_title = PageInput {
        page_type: "note".into(),
        title: String::new(),
        compiled_truth: "truth".into(),
        ..PageInput::default()
    };
    engine
        .put_page("beta", Some("src-1"), &no_title)
        .await
        .expect("seed beta no title");

    // page with title but no domain
    engine
        .put_page("gamma", Some("src-1"), &page_input("gamma"))
        .await
        .expect("seed gamma");

    // soft-deleted page — should be excluded
    engine
        .put_page("deleted", Some("src-1"), &page_input("deleted"))
        .await
        .expect("seed deleted");
    engine
        .soft_delete_page("deleted", Some("src-1"))
        .await
        .expect("soft delete");

    let orphans = engine.find_orphan_pages().await.expect("find_orphan_pages");
    // InMemory has no links table, so all live pages are orphans
    // Ordered by slug ASC
    assert_eq!(
        orphans,
        vec![
            OrphanPage {
                slug: "alpha".into(),
                title: "Titled Page".into(),
                domain: Some("test-domain".into()),
            },
            OrphanPage {
                slug: "beta".into(),
                title: "beta".into(),
                domain: None,
            },
            OrphanPage {
                slug: "gamma".into(),
                title: "gamma".into(),
                domain: None,
            },
        ],
        "live pages with COALESCE(title, slug) and domain extraction, ordered by slug"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_get_page_timestamps_returns_coalesce_and_omits_missing() {
    let engine = init_in_memory().await;

    engine
        .put_page("exists", Some("src-1"), &page_input("exists"))
        .await
        .expect("seed exists");
    engine
        .put_page("deleted", Some("src-1"), &page_input("deleted"))
        .await
        .expect("seed deleted");
    engine
        .soft_delete_page("deleted", Some("src-1"))
        .await
        .expect("soft delete deleted");

    let page = engine
        .get_page("exists", &get_opts("src-1", false))
        .await
        .expect("get page")
        .expect("page exists");
    let expected_ts = if page.updated_at.is_empty() {
        page.created_at.clone()
    } else {
        page.updated_at.clone()
    };

    let result: HashMap<String, String> = engine
        .get_page_timestamps(&[
            "exists".to_string(),
            "missing".to_string(),
            "deleted".to_string(),
        ])
        .await
        .expect("get_page_timestamps");

    // Key is slug, not source_id::slug
    assert_eq!(
        result.get("exists"),
        Some(&expected_ts),
        "key is slug, value is COALESCE(updated_at, created_at)"
    );
    assert!(!result.contains_key("missing"), "missing slug omitted");
    assert!(!result.contains_key("deleted"), "soft-deleted slug omitted");

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_get_effective_dates_uses_coalesce_updated_created() {
    let engine = init_in_memory().await;

    engine
        .put_page("page-a", Some("src-1"), &page_input("page-a"))
        .await
        .expect("seed page-a");
    engine
        .put_page("page-b", Some("src-2"), &page_input("page-b"))
        .await
        .expect("seed page-b");

    let page_a = engine
        .get_page("page-a", &get_opts("src-1", false))
        .await
        .expect("get page-a")
        .expect("exists");
    let expected_ts_a = if page_a.updated_at.is_empty() {
        page_a.created_at.clone()
    } else {
        page_a.updated_at.clone()
    };

    let refs = vec![
        PageRef {
            slug: "page-a".into(),
            source_id: "src-1".into(),
        },
        PageRef {
            slug: "page-b".into(),
            source_id: "src-2".into(),
        },
        PageRef {
            slug: "no-such".into(),
            source_id: "src-1".into(),
        },
    ];

    let result: HashMap<String, String> = engine
        .get_effective_dates(&refs)
        .await
        .expect("get_effective_dates");

    // Key format: "{source_id}::{slug}"
    assert_eq!(
        result.get("src-1::page-a"),
        Some(&expected_ts_a),
        "key is source_id::slug, value is COALESCE(updated_at, created_at)"
    );
    assert!(result.contains_key("src-2::page-b"), "page-b present");
    assert!(
        !result.contains_key("src-1::no-such"),
        "missing ref omitted"
    );

    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn in_memory_get_salience_scores_default_emotional_weight_is_zero() {
    let engine = init_in_memory().await;

    engine
        .put_page("page-x", Some("src-1"), &page_input("page-x"))
        .await
        .expect("seed page-x");

    let refs = vec![
        PageRef {
            slug: "page-x".into(),
            source_id: "src-1".into(),
        },
        PageRef {
            slug: "no-such".into(),
            source_id: "src-1".into(),
        },
    ];

    let result: HashMap<String, f64> = engine
        .get_salience_scores(&refs)
        .await
        .expect("get_salience_scores");

    // emotional_weight defaults to None → 0.0 * 5.0 = 0.0
    // Key format: "{source_id}::{slug}"
    assert_eq!(
        result.get("src-1::page-x"),
        Some(&0.0),
        "default score is 0.0"
    );
    assert!(
        !result.contains_key("src-1::no-such"),
        "missing ref omitted"
    );

    engine.disconnect().await.expect("disconnect");
}
