//! 1-6-7-10-1: code-graph edge storage (write side) — Libsql backend.
//!
//! Verifies that `add_code_edges` routes resolved vs unresolved edges into the
//! correct table, honors the UNIQUE dedup keys (via `INSERT OR IGNORE`), and
//! that `delete_code_edges_for_chunks` removes touching rows. Mirrors the TS
//! `addCodeEdges` / `deleteCodeEdgesForChunks` split across `code_edges_chunk`
//! and `code_edges_symbol`.

use libsql::Builder;
use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::import::CodeEdgeInput;
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


fn resolved_edge(from: i64, to: i64, edge_type: &str) -> CodeEdgeInput {
    CodeEdgeInput {
        from_chunk_id: from,
        to_chunk_id: Some(to),
        from_symbol_qualified: format!("m::f{from}"),
        to_symbol_qualified: format!("m::t{to}"),
        edge_type: edge_type.to_string(),
        edge_metadata: json!({}),
        source_id: None,
    }
}

fn unresolved_edge(from: i64, to_symbol: &str) -> CodeEdgeInput {
    CodeEdgeInput {
        from_chunk_id: from,
        to_chunk_id: None,
        from_symbol_qualified: format!("m::f{from}"),
        to_symbol_qualified: to_symbol.to_string(),
        edge_type: "imports".to_string(),
        edge_metadata: json!({}),
        source_id: None,
    }
}

/// Fresh LibsqlEngine backed by a temp file (engine + guard so the file is
/// not deleted before the engine is done with it).
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

/// Raw row count for a given code-edges table.
async fn count_rows(path: &std::path::Path, table: &str) -> i64 {
    let conn = Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    let mut rows = conn
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// `code_edges_chunk`/`code_edges_symbol` carry FKs to `content_chunks(id)`.
/// Production writes edges only after chunks land, so seed the referenced
/// chunk rows on the same DB file before exercising `add_code_edges`.
async fn seed_chunks(path: &std::path::Path, ids: &[i64]) {
    let conn = Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    for &id in ids {
        conn.execute(
            "INSERT OR IGNORE INTO content_chunks (id, page_id, chunk_index, chunk_text, chunk_source) \
             VALUES (?1, ?1, 0, 'seed', 'text')",
            [id],
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn add_code_edges_routes_resolved_and_unresolved() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    seed_chunks(temp.path(), &[1, 2, 3]).await;

    engine
        .add_code_edges(&[resolved_edge(1, 2, "calls"), unresolved_edge(3, "ext::qux")])
        .await
        .unwrap();

    assert_eq!(count_rows(temp.path(), "code_edges_chunk").await, 1);
    assert_eq!(count_rows(temp.path(), "code_edges_symbol").await, 1);

    // Spot-check the resolved row's payload landed correctly.
    let conn = Builder::new_local(temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    let mut rows = conn
        .query(
            "SELECT from_chunk_id, to_chunk_id, edge_type, from_symbol_qualified, to_symbol_qualified \
             FROM code_edges_chunk LIMIT 1",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
    assert_eq!(row.get::<i64>(1).unwrap(), 2);
    assert_eq!(row.get::<String>(2).unwrap(), "calls");
    assert_eq!(row.get::<String>(3).unwrap(), "m::f1");
    assert_eq!(row.get::<String>(4).unwrap(), "m::t2");
}

#[tokio::test]
async fn add_code_edges_dedup_via_unique_key() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    seed_chunks(temp.path(), &[1, 2]).await;

    // Same resolved edge twice → only one row (INSERT OR IGNORE on
    // (from_chunk_id, to_chunk_id, edge_type)).
    engine
        .add_code_edges(&[resolved_edge(1, 2, "calls")])
        .await
        .unwrap();
    engine
        .add_code_edges(&[resolved_edge(1, 2, "calls")])
        .await
        .unwrap();
    assert_eq!(count_rows(temp.path(), "code_edges_chunk").await, 1);

    // Different edge_type is a distinct key → stored.
    engine
        .add_code_edges(&[resolved_edge(1, 2, "imports")])
        .await
        .unwrap();
    assert_eq!(count_rows(temp.path(), "code_edges_chunk").await, 2);
}

#[tokio::test]
async fn delete_code_edges_for_chunks_removes_touching_edges() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    seed_chunks(temp.path(), &[10, 20, 30, 40, 50]).await;

    engine
        .add_code_edges(&[
            resolved_edge(10, 20, "calls"), // from=10
            resolved_edge(30, 10, "calls"), // to=10
            unresolved_edge(10, "ext::a"),  // from=10 (symbol table)
            resolved_edge(40, 50, "calls"), // untouched
        ])
        .await
        .unwrap();

    assert_eq!(count_rows(temp.path(), "code_edges_chunk").await, 3);
    assert_eq!(count_rows(temp.path(), "code_edges_symbol").await, 1);

    // Deleting chunk 10 removes the 3 touching edges (from or to, including the
    // unresolved one), leaving only 40→50.
    engine.delete_code_edges_for_chunks(&[10]).await.unwrap();

    assert_eq!(count_rows(temp.path(), "code_edges_chunk").await, 1);
    assert_eq!(count_rows(temp.path(), "code_edges_symbol").await, 0);

    let conn = Builder::new_local(temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    let mut rows = conn
        .query("SELECT from_chunk_id, to_chunk_id FROM code_edges_chunk LIMIT 1", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 40);
    assert_eq!(row.get::<i64>(1).unwrap(), 50);
}
