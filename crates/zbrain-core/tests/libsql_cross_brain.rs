//! 1-3-3-4 — `query_across_brains` D18 4-rule contract against real libsql temp DBs.
//!
//! Local engine + mount engines are real `LibsqlEngine` instances over separate
//! temp files (mirrors `libsql_undo_wave.rs`). A `StubMountResolver` yields
//! pre-seeded mount engines so the test isolates the routing logic in
//! `query_across_brains` from mount discovery.
//!
//! Cases: local-first wins over a seeded mount; mount-fallback when local is
//! empty; unpublished mount profile is skipped; subagent callers (mounts not
//! readable) never consult mounts.

use async_trait::async_trait;
use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::calibration::{query_across_brains, MountResolver, MountableBrainEngine};
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::error::Result as ZbResult;
use zbrain_core::libsql::LibsqlEngine;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}


async fn temp_engine() -> (NamedTempFile, LibsqlEngine) {
    let temp = NamedTempFile::new().expect("alloc temp db file");
    let path = temp.path().to_string_lossy().to_string();
    let config = EngineConfig {
        database_path: Some(path),
        database_url: None,
    };
    let engine = LibsqlEngine::new();
    engine.connect(&config).await.unwrap();
    engine.init_schema().await.unwrap();
    (temp, engine)
}

async fn raw_conn(path: &std::path::Path) -> libsql::Connection {
    Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

async fn seed_source(conn: &libsql::Connection, id: &str) {
    conn.execute(
        "INSERT INTO sources (id, name) VALUES (?1, ?1)",
        libsql::params![id],
    )
    .await
    .unwrap();
}

/// Seed a `calibration_profiles` row. `published` is explicit because the
/// cross-brain contract filters on it.
async fn seed_profile(conn: &libsql::Connection, source_id: &str, wave: &str, published: bool) {
    conn.execute(
        "INSERT INTO calibration_profiles \
         (source_id, holder, wave_version, total_resolved, domain_scorecards, \
          pattern_statements, voice_gate_passed, voice_gate_attempts, \
          active_bias_tags, model_id, published) \
         VALUES (?1, 'garry', ?2, 10, '{}', '[]', 1, 1, '[]', 'test-model', ?3)",
        libsql::params![source_id, wave, published as i64],
    )
    .await
    .unwrap();
}

/// Stub resolver: returns fresh engines over the given temp db paths, seeded by
/// the caller. Reconnecting to the same file each call reads the persisted
/// rows, so a single `resolve_mounts` per query is enough for these tests.
struct StubMountResolver {
    entries: Vec<(String, String)>,
}

#[async_trait]
impl MountResolver for StubMountResolver {
    async fn resolve_mounts(&self) -> ZbResult<Vec<(String, Box<dyn MountableBrainEngine>)>> {
        let mut out: Vec<(String, Box<dyn MountableBrainEngine>)> = Vec::new();
        for (id, path) in &self.entries {
            let engine = LibsqlEngine::new();
            engine
                .connect(&EngineConfig {
                    database_path: Some(path.clone()),
                    database_url: None,
                })
                .await?;
            out.push((id.clone(), Box::new(engine)));
        }
        Ok(out)
    }
}

// 1. Local-first: a local published profile wins even if a mount also has one.
#[tokio::test]
async fn cross_brain_local_first_wins_over_mount() {
    let _guard = libsql_test_guard();
    let (local_temp, local) = temp_engine().await;
    let (mount_temp, _mount) = temp_engine().await;
    let local_conn = raw_conn(local_temp.path()).await;
    let mount_conn = raw_conn(mount_temp.path()).await;
    seed_source(&local_conn, "wiki").await;
    seed_source(&mount_conn, "wiki").await;
    // Local + mount both have a profile; distinct waves disambiguate attribution.
    seed_profile(&local_conn, "wiki", "local-wave", true).await;
    seed_profile(&mount_conn, "wiki", "mount-wave", true).await;

    let resolver = StubMountResolver {
        entries: vec![(
            "mount-b".to_string(),
            mount_temp.path().to_string_lossy().to_string(),
        )],
    };
    let result = query_across_brains(
        &local,
        "host".to_string(),
        "garry",
        true,
        &resolver,
        None,
        None,
    )
    .await
    .unwrap()
    .expect("local profile present");

    assert!(!result.from_mount, "local-first → not from mount");
    assert_eq!(result.source_brain_id, "host", "attributed to local brain id");
    assert_eq!(
        result.profile.wave_version, "local-wave",
        "local row returned, not mount"
    );
}

// 2. Mount-fallback: no local profile → first published mount profile wins.
#[tokio::test]
async fn cross_brain_mount_fallback_when_local_empty() {
    let _guard = libsql_test_guard();
    let (local_temp, local) = temp_engine().await;
    let (mount_temp, _mount) = temp_engine().await;
    let _local_conn = raw_conn(local_temp.path()).await; // local has NO profile
    let mount_conn = raw_conn(mount_temp.path()).await;
    seed_source(&mount_conn, "wiki").await;
    seed_profile(&mount_conn, "wiki", "mount-wave", true).await;

    let resolver = StubMountResolver {
        entries: vec![(
            "mount-b".to_string(),
            mount_temp.path().to_string_lossy().to_string(),
        )],
    };
    let result = query_across_brains(
        &local,
        "host".to_string(),
        "garry",
        true,
        &resolver,
        None,
        None,
    )
    .await
    .unwrap()
    .expect("mount profile present");

    assert!(result.from_mount, "came from mount");
    assert_eq!(result.source_brain_id, "mount-b", "attributed to mount id");
    assert_eq!(result.profile.wave_version, "mount-wave");
}

// 3. published=false is skipped: an unpublished mount profile yields None.
#[tokio::test]
async fn cross_brain_skips_unpublished_mount() {
    let _guard = libsql_test_guard();
    let (local_temp, local) = temp_engine().await;
    let (mount_temp, _mount) = temp_engine().await;
    let _local_conn = raw_conn(local_temp.path()).await;
    let mount_conn = raw_conn(mount_temp.path()).await;
    seed_source(&mount_conn, "wiki").await;
    seed_profile(&mount_conn, "wiki", "mount-wave", false).await; // unpublished

    let resolver = StubMountResolver {
        entries: vec![(
            "mount-b".to_string(),
            mount_temp.path().to_string_lossy().to_string(),
        )],
    };
    let result = query_across_brains(
        &local,
        "host".to_string(),
        "garry",
        true,
        &resolver,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        result.is_none(),
        "unpublished mount profile is skipped → no result"
    );
}

// 4. Subagent gate: when mounts may not be read, mount fallback never runs.
#[tokio::test]
async fn cross_brain_subagent_cannot_read_mounts() {
    let _guard = libsql_test_guard();
    let (local_temp, local) = temp_engine().await;
    let (mount_temp, _mount) = temp_engine().await;
    let _local_conn = raw_conn(local_temp.path()).await;
    let mount_conn = raw_conn(mount_temp.path()).await;
    seed_source(&mount_conn, "wiki").await;
    seed_profile(&mount_conn, "wiki", "mount-wave", true).await;

    let resolver = StubMountResolver {
        entries: vec![(
            "mount-b".to_string(),
            mount_temp.path().to_string_lossy().to_string(),
        )],
    };
    // can_read_mounts = false (the op computes this from ctx; here we pass it
    // directly to mirror a subagent caller with no allowed_slug_prefixes).
    let result = query_across_brains(
        &local,
        "host".to_string(),
        "garry",
        false,
        &resolver,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        result.is_none(),
        "mounts not consulted when can_read_mounts=false"
    );
}
