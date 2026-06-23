// scripts/e2e-test-map.ts
//
// Path-glob -> E2E test files map. Used by scripts/select-e2e.ts.
//
// CONTRACT: This map can ONLY narrow from "all". When a changed src/ path
// matches no glob here, the selector falls back to "run all E2E" (fail-closed).
// You can safely add narrowing entries; you cannot break correctness by missing
// one. Tune as misses surface (i.e., when ci:local:diff ran more than necessary
// and you'd like to narrow that surface area).
//
// Glob syntax is the minimal subset implemented in select-e2e.ts:
//   - "**" matches any sequence of path segments (including zero)
//   - "*" matches any characters within a single path segment
//   - everything else is literal
// No brace expansion, no ?, no [ ].

export const E2E_TEST_MAP: Record<string, string[]> = {
  // Source-aware ranking, hybrid search, intent classification.
  "src/core/search/**": [
    "tests/unit/e2e/search-quality.test.ts",
    "tests/unit/e2e/search-exclude.test.ts",
    "tests/unit/e2e/search-swamp.test.ts",
  ],
  // Tree-sitter chunkers feed code-indexing E2E.
  "src/core/chunkers/**": ["tests/unit/e2e/code-indexing.test.ts"],
  // OpenClaw context-engine plugin: engine + entry feed the plugin-shape E2E
  // (mocked SDK) AND the real-loader Tier 2 E2E that spawns openclaw and
  // actually installs the plugin into an isolated --profile.
  "src/core/context-engine.ts": [
    "tests/unit/e2e/openclaw-context-engine-plugin.test.ts",
    "tests/unit/e2e/openclaw-plugin-load-real.test.ts",
  ],
  "src/openclaw-context-engine.ts": [
    "tests/unit/e2e/openclaw-context-engine-plugin.test.ts",
    "tests/unit/e2e/openclaw-plugin-load-real.test.ts",
  ],
  // dream.ts is a thin alias over runCycle in cycle.ts.
  "src/core/cycle.ts": ["tests/unit/e2e/cycle.test.ts", "tests/unit/e2e/dream.test.ts"],
  // Multi-source sync writes share the per-source bookmark anchor.
  "src/core/sync.ts": ["tests/unit/e2e/sync.test.ts", "tests/unit/e2e/multi-source.test.ts"],
  // v0.32.8 multi-source bug class regression suite — fires on any cycle
  // phase, extract, integrity, embed, or migrate-engine change.
  "src/core/cycle/extract-takes.ts": ["tests/unit/e2e/multi-source-bug-class.test.ts"],
  "src/core/cycle/patterns.ts": ["tests/unit/e2e/multi-source-bug-class.test.ts"],
  "src/core/cycle/synthesize.ts": ["tests/unit/e2e/multi-source-bug-class.test.ts"],
  "src/commands/embed.ts": ["tests/unit/e2e/multi-source-bug-class.test.ts"],
  "src/commands/extract.ts": ["tests/unit/e2e/multi-source-bug-class.test.ts"],
  "src/commands/migrate-engine.ts": ["tests/unit/e2e/multi-source-bug-class.test.ts"],
  // Any minions queue/worker/handler change exercises all minion E2E.
  "src/core/minions/**": [
    "tests/unit/e2e/minions-concurrency.test.ts",
    "tests/unit/e2e/minions-resilience.test.ts",
    "tests/unit/e2e/minions-shell.test.ts",
    "tests/unit/e2e/minions-shell-pglite.test.ts",
    "tests/unit/e2e/worker-abort-recovery.test.ts",
  ],
  // postgres.js bind paths + JSONB shapes + parity vs PGLite.
  "src/core/postgres-engine.ts": [
    "tests/unit/e2e/postgres-bootstrap.test.ts",
    "tests/unit/e2e/postgres-jsonb.test.ts",
    "tests/unit/e2e/jsonb-roundtrip.test.ts",
    "tests/unit/e2e/engine-parity.test.ts",
    "tests/unit/e2e/schema-drift.test.ts",
  ],
  // PGLite bootstrap path + parity guard.
  "src/core/pglite-engine.ts": [
    "tests/unit/e2e/postgres-bootstrap.test.ts",
    "tests/unit/e2e/engine-parity.test.ts",
    "tests/unit/e2e/schema-drift.test.ts",
  ],
  // Schema source of truth: any change must pass the cross-engine drift gate.
  "src/schema.sql": ["tests/unit/e2e/schema-drift.test.ts"],
  "src/core/pglite-schema.ts": ["tests/unit/e2e/schema-drift.test.ts"],
  "src/core/migrate.ts": ["tests/unit/e2e/schema-drift.test.ts", "tests/unit/e2e/migrate-chain.test.ts"],
  // MCP stdio + HTTP transports share dispatch.
  "src/mcp/**": ["tests/unit/e2e/mcp.test.ts", "tests/unit/e2e/http-transport.test.ts"],
  // Integrity batch-load fast path.
  "src/commands/integrity.ts": ["tests/unit/e2e/integrity-batch.test.ts"],
  // Upgrade chains migration ledger; touches both runners.
  "src/commands/upgrade.ts": [
    "tests/unit/e2e/upgrade.test.ts",
    "tests/unit/e2e/migrate-chain.test.ts",
    "tests/unit/e2e/migration-flow.test.ts",
  ],
  "src/commands/doctor.ts": ["tests/unit/e2e/doctor-progress.test.ts"],
  // Knowledge graph layer feeds graph-quality.
  "src/core/link-extraction.ts": ["tests/unit/e2e/graph-quality.test.ts"],
  // v0.38 ingestion substrate. POST /ingest lives inside serve-http.ts
  // (per the plan-eng-review E1 decision); the daemon + built-in sources
  // + ingest_capture Minion handler all feed the in-process roundtrip
  // E2E AND the HTTP contract E2E for the webhook route.
  "src/commands/serve-http.ts": [
    "tests/unit/e2e/serve-http-ingest-webhook.test.ts",
    "tests/unit/e2e/serve-http-oauth.test.ts",
  ],
  "src/core/ingestion/**": [
    "tests/unit/e2e/ingestion-roundtrip.test.ts",
    "tests/unit/e2e/serve-http-ingest-webhook.test.ts",
  ],
  "src/core/minions/handlers/ingest-capture.ts": [
    "tests/unit/e2e/ingestion-roundtrip.test.ts",
    "tests/unit/e2e/serve-http-ingest-webhook.test.ts",
  ],
};
