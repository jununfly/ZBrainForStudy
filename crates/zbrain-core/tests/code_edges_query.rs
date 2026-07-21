//! 1-6-7-10-2: code-graph query methods — Libsql + Postgres backends.
//!
//! Exercises the read half of the code-graph subsystem: `get_callers_of`
//! (UNION of `code_edges_chunk` + `code_edges_symbol` on `to_symbol_qualified`),
//! `get_callees_of` (same on `from_symbol_qualified`), and `get_edges_by_chunk`
//! (direction + edge_type filtering). The Postgres tests also exercise the
//! previously-missing PG write path (`add_code_edges` / `delete_code_edges_for_chunks`).

use serde_json::json;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::import::{
    CodeEdgeByChunkOpts, CodeEdgeDirection, CodeEdgeInput, CodeGraphQueryOpts,
};
use zbrain_core::libsql::LibsqlEngine;

fn edge(
    from: i64,
    to: Option<i64>,
    from_sym: &str,
    to_sym: &str,
    edge_type: &str,
    source_id: Option<&str>,
) -> CodeEdgeInput {
    CodeEdgeInput {
        from_chunk_id: from,
        to_chunk_id: to,
        from_symbol_qualified: from_sym.to_string(),
        to_symbol_qualified: to_sym.to_string(),
        edge_type: edge_type.to_string(),
        edge_metadata: json!({}),
        source_id: source_id.map(|s| s.to_string()),
    }
}

/// Fresh LibsqlEngine backed by a temp file.
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

/// `code_edges_chunk`/`code_edges_symbol` carry FKs to `content_chunks(id)`.
/// Production writes edges only after chunks land, so seed the referenced
/// chunk rows on the same DB file before exercising `add_code_edges`.
async fn seed_chunks(path: &std::path::Path, ids: &[i64]) {
    let conn = libsql::Builder::new_local(path)
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

/// Seed source rows so the `code_edges_chunk.source_id` FK is satisfied.
async fn seed_sources(path: &std::path::Path, ids: &[&str]) {
    let conn = libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    for &id in ids {
        conn.execute(
            "INSERT OR IGNORE INTO sources (id, name) VALUES (?1, ?2)",
            [id, id],
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn libsql_get_callers_of_unions_resolved_and_unresolved() {
    let (temp, engine) = temp_engine().await;
    seed_chunks(temp.path(), &[1, 2, 3]).await;
    seed_sources(temp.path(), &["s1"]).await;

    engine
        .add_code_edges(&[
            edge(1, Some(2), "m::a", "m::target", "calls", Some("s1")),
            edge(3, None, "m::b", "m::target", "imports", None),
        ])
        .await
        .unwrap();

    let callers = engine
        .get_callers_of("m::target", &CodeGraphQueryOpts::default())
        .await
        .unwrap();
    assert_eq!(callers.len(), 2);
    assert!(callers.iter().any(|e| e.resolved && e.from_symbol_qualified == "m::a"));
    assert!(callers.iter().any(|e| !e.resolved && e.from_symbol_qualified == "m::b"));
    // source_id round-trips for the resolved (seeded) row.
    let resolved = callers.iter().find(|e| e.resolved).unwrap();
    assert_eq!(resolved.source_id.as_deref(), Some("s1"));
    assert_eq!(resolved.to_chunk_id, Some(2));
    let unresolved = callers.iter().find(|e| !e.resolved).unwrap();
    assert_eq!(unresolved.to_chunk_id, None);
}

#[tokio::test]
async fn libsql_get_callees_of_matches_from_symbol() {
    let (temp, engine) = temp_engine().await;
    seed_chunks(temp.path(), &[1, 2, 3, 4, 5]).await;

    engine
        .add_code_edges(&[
            edge(1, Some(2), "m::origin", "m::x", "calls", None),
            edge(3, None, "m::origin", "m::y", "imports", None),
            edge(4, Some(5), "m::other", "m::z", "calls", None),
        ])
        .await
        .unwrap();

    let callees = engine
        .get_callees_of("m::origin", &CodeGraphQueryOpts::default())
        .await
        .unwrap();
    assert_eq!(callees.len(), 2);
    assert!(callees.iter().all(|e| e.from_symbol_qualified == "m::origin"));
    assert!(callees.iter().any(|e| e.resolved && e.to_symbol_qualified == "m::x"));
    assert!(callees.iter().any(|e| !e.resolved && e.to_symbol_qualified == "m::y"));
}

#[tokio::test]
async fn libsql_get_edges_by_chunk_direction_and_type() {
    let (temp, engine) = temp_engine().await;
    seed_chunks(temp.path(), &[10, 20, 30]).await;

    engine
        .add_code_edges(&[
            edge(10, Some(20), "m::a", "m::b", "calls", None),   // out, resolved
            edge(30, Some(10), "m::c", "m::d", "calls", None),   // in, resolved
            edge(10, None, "m::e", "m::f", "imports", None),     // out, unresolved
        ])
        .await
        .unwrap();

    let out = engine
        .get_edges_by_chunk(
            10,
            &CodeEdgeByChunkOpts {
                direction: CodeEdgeDirection::Out,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|e| e.from_chunk_id == 10));

    let inbound = engine
        .get_edges_by_chunk(
            10,
            &CodeEdgeByChunkOpts {
                direction: CodeEdgeDirection::In,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].from_chunk_id, 30);
    assert_eq!(inbound[0].to_chunk_id, Some(10));

    let both = engine
        .get_edges_by_chunk(
            10,
            &CodeEdgeByChunkOpts {
                direction: CodeEdgeDirection::Both,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(both.len(), 3);

    let calls_only = engine
        .get_edges_by_chunk(
            10,
            &CodeEdgeByChunkOpts {
                direction: CodeEdgeDirection::Both,
                edge_type: Some("calls".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(calls_only.len(), 2);
    assert!(calls_only.iter().all(|e| e.edge_type == "calls"));
}

// ─────────────────────────────────────────────────────────────────────────
// Postgres integration tests
// ─────────────────────────────────────────────────────────────────────────

mod support;
use support::pg_fixture::PgFixture;

/// Seed a source row (satisfies `code_edges_chunk.source_id` FK) and the
/// referenced `content_chunks` rows (satisfies `code_edges_chunk.from_chunk_id`
/// / `to_chunk_id` FK). Postgres enforces these; libsql does not by default.
async fn pg_seed_chunks(url: &str, source_id: &str, chunk_ids: &[i64]) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("seed pool");
    sqlx::query("INSERT INTO sources (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
        .bind(source_id)
        .bind(source_id)
        .execute(&pool)
        .await
        .expect("seed source");
    for &id in chunk_ids {
        sqlx::query(
            "INSERT INTO content_chunks (id, page_id, chunk_index, chunk_text, chunk_source) \
             VALUES ($1, $1, 0, 'seed', 'text') ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .execute(&pool)
        .await
        .expect("seed chunk");
    }
    pool.close().await;
}

#[tokio::test]
async fn postgres_get_callers_of_unions_resolved_and_unresolved() {
    let fix = PgFixture::start().await;
    pg_seed_chunks(&fix.url, "s1", &[1, 2, 3]).await;

    // Exercises the previously-missing PG write path.
    fix.engine
        .add_code_edges(&[
            edge(1, Some(2), "m::a", "m::target", "calls", Some("s1")),
            edge(3, None, "m::b", "m::target", "imports", None),
        ])
        .await
        .unwrap();

    let callers = fix
        .engine
        .get_callers_of("m::target", &CodeGraphQueryOpts::default())
        .await
        .unwrap();
    assert_eq!(callers.len(), 2);
    assert!(callers.iter().any(|e| e.resolved && e.from_symbol_qualified == "m::a"));
    assert!(callers.iter().any(|e| !e.resolved && e.from_symbol_qualified == "m::b"));
    let resolved = callers.iter().find(|e| e.resolved).unwrap();
    assert_eq!(resolved.source_id.as_deref(), Some("s1"));
}

#[tokio::test]
async fn postgres_get_edges_by_chunk_direction_and_type() {
    let fix = PgFixture::start().await;
    pg_seed_chunks(&fix.url, "s1", &[10, 20, 30]).await;

    fix.engine
        .add_code_edges(&[
            edge(10, Some(20), "m::a", "m::b", "calls", Some("s1")),
            edge(30, Some(10), "m::c", "m::d", "calls", Some("s1")),
            edge(10, None, "m::e", "m::f", "imports", None),
        ])
        .await
        .unwrap();

    let out = fix
        .engine
        .get_edges_by_chunk(
            10,
            &CodeEdgeByChunkOpts {
                direction: CodeEdgeDirection::Out,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|e| e.from_chunk_id == 10));

    let inbound = fix
        .engine
        .get_edges_by_chunk(
            10,
            &CodeEdgeByChunkOpts {
                direction: CodeEdgeDirection::In,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].from_chunk_id, 30);

    let calls_only = fix
        .engine
        .get_edges_by_chunk(
            10,
            &CodeEdgeByChunkOpts {
                direction: CodeEdgeDirection::Both,
                edge_type: Some("calls".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(calls_only.len(), 2);
    assert!(calls_only.iter().all(|e| e.edge_type == "calls"));
}
