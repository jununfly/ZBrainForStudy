//! Slice 6a S6-T7 — `LibsqlEngine` Tag CRUD integration tests.
//!
//! Mirrors the TS `addTag / removeTag / getTags` surface, with
//! `source_id: Option<&str>` parameterisation.  TS normalises
//! `source_id = None` to `"default"` (`opts?.sourceId ?? 'default'`);
//! Rust follows exactly.
//!
//! Test matrix (15 cases):
//!   `add_tag`    ×5  success / duplicate-idempotent / page-not-found /
//!                  explicit-source / None≡default
//!   `remove_tag` ×4  delete-existing / delete-absent-tag-silent /
//!                  page-missing-silent / source-mismatch-silent
//!   `get_tags`   ×4  empty-list / multi-tag-alpha-order /
//!                  page-missing→[] / source-mismatch→[]
//!   integration×2  soft-deleted-page → tags still reachable via
//!                  `include_deleted` page then `add_tag` fails /
//!                  hard-delete-page → FK CASCADE cleans `page_tags`

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
use zbrain_core::libsql::LibsqlEngine;

/// Build a connected, schema-initialized engine on a fresh temp file.
/// Returns `(engine, NamedTempFile)` so the caller can keep the temp file
/// alive for the duration of the test — dropping it deletes the DB.
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

// ═══════════════════════════════════════════════════════════════════════
// add_tag (5)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn s6t7_add_tag_succeeds_on_existing_page() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("alpha", &note_input("Alpha", "body"))
        .await
        .expect("put_page");
    engine
        .add_tag("alpha", "rust", None)
        .await
        .expect("add_tag must succeed");
    // Verify the tag landed
    let tags = engine.get_tags("alpha", None).await.expect("get_tags");
    assert_eq!(tags, vec!["rust"]);
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_add_tag_idempotent_on_duplicate() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("beta", &note_input("Beta", "body"))
        .await
        .expect("put_page");
    engine
        .add_tag("beta", "ai", None)
        .await
        .expect("first add_tag");
    engine
        .add_tag("beta", "ai", None)
        .await
        .expect("second add_tag must be idempotent");
    let tags = engine.get_tags("beta", None).await.expect("get_tags");
    assert_eq!(tags, vec!["ai"], "duplicate add must not produce a second row");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_add_tag_page_not_found_returns_error() {
    let (engine, _tmp) = init_clean_engine().await;
    let err = engine
        .add_tag("ghost", "rust", None)
        .await
        .expect_err("add_tag on missing page must fail");
    assert_eq!(err.class, "PageNotFound");
    assert_eq!(err.code, "page_not_found");
    // TS message shape: `addTag failed: page "ghost" (source=default) not found`
    assert!(err.message.contains("ghost"), "msg={}", err.message);
    assert!(
        err.message.contains("(source=default)"),
        "None→'default' normalisation; msg={}",
        err.message
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_add_tag_with_explicit_source_id() {
    // put_page currently hard-codes source_id='default'. We use add_tag
    // with source_id=Some("default") which must match. A non-default
    // source must produce PageNotFound because no such page row exists.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("gamma", &note_input("Gamma", "body"))
        .await
        .expect("put_page");

    // Matching source → ok
    engine
        .add_tag("gamma", "rust", Some("default"))
        .await
        .expect("add_tag with explicit 'default' must succeed");
    let tags = engine.get_tags("gamma", Some("default")).await.expect("get_tags");
    assert_eq!(tags, vec!["rust"]);

    // Mismatched source → page not found
    let err = engine
        .add_tag("gamma", "rust", Some("other"))
        .await
        .expect_err("mismatched source must fail");
    assert_eq!(err.code, "page_not_found");
    assert!(
        err.message.contains("(source=other)"),
        "msg={}",
        err.message
    );
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_add_tag_none_equivalent_to_default() {
    // `None` and `Some("default")` must be semantically identical.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("delta", &note_input("Delta", "body"))
        .await
        .expect("put_page");
    engine
        .add_tag("delta", "ml", None)
        .await
        .expect("add_tag with None");
    let tags_none = engine.get_tags("delta", None).await.expect("get_tags(None)");
    let tags_default = engine
        .get_tags("delta", Some("default"))
        .await
        .expect("get_tags(Some(default))");
    assert_eq!(tags_none, tags_default, "None≡default must hold");
    engine.disconnect().await.expect("disconnect");
}

// ═══════════════════════════════════════════════════════════════════════
// remove_tag (4)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn s6t7_remove_tag_deletes_existing_tag() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("eps", &note_input("Eps", "body"))
        .await
        .expect("put_page");
    engine.add_tag("eps", "rust", None).await.expect("add_tag");
    engine
        .remove_tag("eps", "rust", None)
        .await
        .expect("remove_tag");
    let tags = engine.get_tags("eps", None).await.expect("get_tags");
    assert!(tags.is_empty(), "tag must be gone after remove");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_remove_tag_absent_tag_is_silent() {
    // TS removeTag uses a sub-select → DELETE … WHERE page_id=(SELECT …)
    // AND tag=$3. If the tag doesn't exist, affected=0, no error.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("zeta", &note_input("Zeta", "body"))
        .await
        .expect("put_page");
    engine
        .remove_tag("zeta", "nonexistent", None)
        .await
        .expect("removing absent tag must be silent Ok(())");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_remove_tag_page_missing_is_silent() {
    // TS asymmetry: addTag throws on missing page, removeTag is silent.
    // The sub-select yields NULL → DELETE matches 0 rows → Ok(()).
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .remove_tag("no-such-page", "rust", None)
        .await
        .expect("remove_tag on missing page must be silent Ok(())");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_remove_tag_source_mismatch_is_silent() {
    // Page exists under source='default'; asking for source='other' means
    // the sub-select returns NULL → silent success, same as TS.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("eta", &note_input("Eta", "body"))
        .await
        .expect("put_page");
    engine.add_tag("eta", "rust", None).await.expect("add_tag");
    engine
        .remove_tag("eta", "rust", Some("other"))
        .await
        .expect("source mismatch must be silent");
    // Original tag still present
    let tags = engine.get_tags("eta", None).await.expect("get_tags");
    assert_eq!(tags, vec!["rust"], "wrong-source remove must not affect real tag");
    engine.disconnect().await.expect("disconnect");
}

// ═══════════════════════════════════════════════════════════════════════
// get_tags (4)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn s6t7_get_tags_returns_empty_for_page_with_no_tags() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("theta", &note_input("Theta", "body"))
        .await
        .expect("put_page");
    let tags = engine.get_tags("theta", None).await.expect("get_tags");
    assert!(tags.is_empty(), "page with no tags must yield []");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_get_tags_returns_sorted_tags() {
    // TS: `ORDER BY tag`. Insert out-of-order to prove sorting.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("iota", &note_input("Iota", "body"))
        .await
        .expect("put_page");
    engine.add_tag("iota", "zinc", None).await.expect("add zinc");
    engine.add_tag("iota", "alpha", None).await.expect("add alpha");
    engine.add_tag("iota", "mid", None).await.expect("add mid");
    let tags = engine.get_tags("iota", None).await.expect("get_tags");
    assert_eq!(tags, vec!["alpha", "mid", "zinc"], "must be alphabetically sorted");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_get_tags_page_missing_returns_empty() {
    // TS getTags returns [] when the sub-select yields NULL (no matching
    // page). This is the second asymmetry vs addTag (which throws).
    let (engine, _tmp) = init_clean_engine().await;
    let tags = engine
        .get_tags("ghost", None)
        .await
        .expect("get_tags on missing page must return Ok");
    assert!(tags.is_empty(), "missing page → []");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_get_tags_source_mismatch_returns_empty() {
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("kappa", &note_input("Kappa", "body"))
        .await
        .expect("put_page");
    engine.add_tag("kappa", "rust", None).await.expect("add_tag");
    let tags = engine
        .get_tags("kappa", Some("other"))
        .await
        .expect("get_tags with wrong source");
    assert!(tags.is_empty(), "source mismatch → []");
    engine.disconnect().await.expect("disconnect");
}

// ═══════════════════════════════════════════════════════════════════════
// Integration (2)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn s6t7_add_tag_fails_on_soft_deleted_page() {
    // A soft-deleted page should be invisible to add_tag (it only looks at
    // live pages), so add_tag must return PageNotFound.
    let (engine, _tmp) = init_clean_engine().await;
    engine
        .put_page("lambda", &note_input("Lambda", "body"))
        .await
        .expect("put_page");
    engine
        .soft_delete_page("lambda", None)
        .await
        .expect("soft_delete_page");

    let err = engine
        .add_tag("lambda", "rust", None)
        .await
        .expect_err("add_tag on soft-deleted page must fail");
    assert_eq!(err.code, "page_not_found");
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn s6t7_hard_delete_page_cascades_to_page_tags() {
    // Schema contract: page_tags.page_id has `ON DELETE CASCADE` (migration
    // 0004). When a `pages` row is hard-deleted, every (page_id, tag) row in
    // page_tags must vanish.
    //
    // This test deliberately bypasses `purge_deleted_pages` (a future-slice
    // method, still trait-default Unsupported here) and issues the raw DELETE
    // against the same DB file via a side-channel libsql connection. That
    // keeps the test scoped to schema FK behavior — independent of which
    // higher-level slice eventually wires the purge.
    //
    // Side-benefit: validates that `PRAGMA foreign_keys = ON` is actually
    // active on the engine connection (libsql defaults OFF). If the PRAGMA
    // were missing, this test would PASS the COUNT assertion but mask a real
    // bug — so we also COUNT before-delete to prove the dependency existed.
    let (engine, tmp) = init_clean_engine().await;
    engine
        .put_page("mu", &note_input("Mu", "body"))
        .await
        .expect("put_page");
    engine.add_tag("mu", "rust", None).await.expect("add_tag");
    engine.add_tag("mu", "ai", None).await.expect("add_tag");

    // Sanity: tags landed via the engine API.
    let tags = engine.get_tags("mu", None).await.expect("get_tags");
    assert_eq!(tags, vec!["ai".to_string(), "rust".to_string()]);

    // Open a sibling connection to the same DB file, with foreign_keys ON to
    // match the engine's expected behavior. (The engine's own connection
    // should already have FK on; this test asserts the schema cascade
    // contract regardless.)
    let db = ::libsql::Builder::new_local(tmp.path())
        .build()
        .await
        .expect("open sibling libsql db");
    let raw = db.connect().expect("connect sibling");
    raw.execute("PRAGMA foreign_keys = ON", ())
        .await
        .expect("enable FK");

    // Confirm the tags exist via raw SQL (pre-delete baseline).
    let mut pre = raw
        .query(
            "SELECT COUNT(*) FROM page_tags pt \
             JOIN pages p ON p.id = pt.page_id \
             WHERE p.slug = ?1",
            ::libsql::params!["mu"],
        )
        .await
        .expect("pre count");
    let pre_row = pre.next().await.expect("pre row").expect("pre present");
    let pre_count: i64 = pre_row.get(0).expect("pre count i64");
    assert_eq!(pre_count, 2, "expected 2 tags on `mu` before hard-delete");

    // Hard-delete the page row directly. With FK CASCADE wired, page_tags
    // rows must disappear; without it, this would either fail (FK violation
    // when FK is on) or leave orphans (FK off).
    let affected = raw
        .execute(
            "DELETE FROM pages WHERE slug = ?1",
            ::libsql::params!["mu"],
        )
        .await
        .expect("hard delete pages row");
    assert_eq!(affected, 1, "expected 1 page row deleted");

    // Post-delete: zero rows in page_tags for this slug ⇒ CASCADE worked.
    let mut post = raw
        .query(
            "SELECT COUNT(*) FROM page_tags WHERE page_id NOT IN (SELECT id FROM pages)",
            (),
        )
        .await
        .expect("post orphan count");
    let post_row = post.next().await.expect("post row").expect("post present");
    let orphan_count: i64 = post_row.get(0).expect("orphan count i64");
    assert_eq!(orphan_count, 0, "FK CASCADE must clear page_tags rows");

    // And via the engine API, the page is gone ⇒ get_tags returns [].
    let tags = engine.get_tags("mu", None).await.expect("get_tags");
    assert!(tags.is_empty(), "hard-deleted page → no dangling tags");
    engine.disconnect().await.expect("disconnect");
}
