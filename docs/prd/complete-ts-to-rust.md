# PRD: Complete TS -> Rust

## Problem Statement

ZBrain is moving from the TypeScript legacy line to the Rust rewrite line. The repository already contains a Rust workspace, but the shipped package and most runtime behavior still point at TypeScript:

- `package.json` still exposes `bin.zbrain = src/cli.ts` and `main = src/core/index.ts`.
- `src/cli.ts`, `src/commands/*.ts`, `src/core/**/*.ts`, and `src/mcp/*.ts` still own the executable CLI, operations, MCP server, storage glue, AI gateway, ingestion, search, jobs, agents, evals, and many repository checks.
- `crates/zbrain-core` already contains a real engine contract and PostgreSQL/libsql implementations for a significant Page-level slice.
- `crates/zbrain-cli`, `crates/zbrain-web`, and `crates/zbrain-mcp` still exist mostly as stubs/placeholders.
- `admin/` is a browser/admin frontend written in React + TypeScript and is not the same category as backend/runtime TypeScript.

Directly deleting the TypeScript side would be unsafe because the Rust rewrite does not yet cover every behavior. Keeping both indefinitely would also be unsafe because it would split product truth, duplicate fixes, and leave users and contributors unable to tell which runtime is canonical.

This PRD defines the complete migration from TypeScript runtime to Rust runtime: migrate behavior into Rust slice by slice, prove each replacement at an external seam, delete the corresponding TypeScript implementation/tests/scripts/docs when the Rust slice is accepted, and explicitly record any TypeScript residue that cannot be deleted.

## Goals

1. Make Rust the canonical implementation for ZBrain runtime behavior.
2. Preserve behavior while migrating; TypeScript is reference material until a Rust slice proves parity or intentionally redesigns the seam.
3. Continuously shrink the TypeScript legacy line instead of accumulating dead code.
4. Keep browser/frontend TypeScript separate from runtime TypeScript and allow it to remain when it is the correct frontend technology.
5. Move package, CLI, MCP, storage, operations, schema/migrations, tests, and docs to ZBrain/Rust-first ownership.
6. Ensure every retained TypeScript surface has a named owner and reason.
7. Keep roadmap coverage complete, even if execution is split into many plans/slices.

## Non-Goals

- No big-bang deletion of the entire TypeScript implementation before Rust parity exists.
- No GBrain compatibility alias/fallback resurrection. The project has no online users and the public language is now ZBrain.
- No mechanical rewrite of browser UI TypeScript just for language purity.
- No opportunistic product redesign hidden inside migration slices. Redesigns are allowed only when explicitly recorded as decisions.
- No treating skipped PostgreSQL tests, unrun Bun tests, or placeholder Rust crates as proof of parity.

## Codebase Facts

### Current Rust Workspace

The root `Cargo.toml` defines the Rust rewrite line as a multi-crate workspace:

```text
crates/zbrain-core
crates/zbrain-cli
crates/zbrain-web
crates/zbrain-mcp
```

`crates/zbrain-core` currently exposes:

```rust
pub mod engine;
pub mod error;
pub mod libsql;
pub mod postgres;
pub mod time;
pub mod types;
```

The current `BrainEngine` contract already covers a substantial Page/storage slice:

- engine lifecycle: `connect`, `disconnect`, `init_schema`
- Page CRUD: `get_page`, `put_page`, `delete_page`, `list_pages`, `resolve_slugs`
- duplicate detection and soft-delete lifecycle
- tag CRUD
- page body refresh and contextual retrieval state
- all-slug/page-ref/orphan/timestamp/effective-date/salience read methods
- in-memory, PostgreSQL, and libsql-backed contract coverage for the implemented subset

`crates/zbrain-core/tests/*.rs` already include backend and contract tests for Page shape, lifecycle, Page CRUD, list filters, tag CRUD, soft delete, purge/restore, page refs, effective dates, salience scores, libsql init schema behavior, and PostgreSQL fixture paths.

### Current Rust Gaps

The Rust rewrite line is not complete:

- `crates/zbrain-cli/src/main.rs` only prints a banner.
- `crates/zbrain-mcp/src/lib.rs` is a placeholder.
- `crates/zbrain-web/src/lib.rs` is a placeholder.
- Rust core does not yet own the full TypeScript operations layer, MCP operation dispatch, AI gateway, search/retrieval, chunking, ingestion, source management, facts/takes/timeline, config, jobs, agents/minions/autopilot, eval harnesses, package install/update behavior, or admin backend API.

### Current TypeScript Runtime Surface

The current TypeScript runtime surface includes, at minimum:

- `src/cli.ts`: actual package CLI entrypoint.
- `src/commands/*.ts`: 100+ command modules including init/config/doctor/storage/schema/import/capture/extract/pages/search/reindex/embed/sources/sync/takes/agents/jobs/serve/MCP/evals/dev checks.
- `src/core/**/*.ts`: engine abstractions, PGLite/Postgres implementations, operations, AI gateway, pricing/capability routing, embeddings, search, chunkers, ingestion, facts/takes/timeline, sync, config, migrations, minions/agent/autopilot, schemapack/skillpack, eval/bench support.
- `src/mcp/*.ts`: tool definitions, dispatch, HTTP transport, server startup, rate limiting, parameter summaries/validation, operation context construction.
- `scripts/*.ts` and shell checks: build, postinstall, schema generation, verification, repo guardrails, test orchestration.
- `tests/unit/**/*.ts`, `tests/heavy/**/*.ts`, and eval fixtures: reference coverage for behavior that may not yet exist in Rust.

### Current Frontend Surface

`admin/` is a React/Vite/TypeScript browser frontend:

```text
admin/src/App.tsx
admin/src/api.ts
admin/src/pages/*.tsx
admin/src/lib/scope-constants.ts
admin/vite.config.ts
```

This PRD treats browser/frontend TS/TSX as an explicit retention candidate. The Rust migration target is runtime/backend/CLI/MCP/storage/core behavior. A future admin UI may continue using TypeScript as long as the backend API is Rust-owned and the retention decision is explicit.

## Migration Principles

1. **Rust is canonical.** New runtime behavior should land in Rust unless it is explicitly frontend-only.
2. **TypeScript is a shrinking reference layer.** TS can remain while it provides behavior not yet replaced, but it must not become a coequal long-term runtime.
3. **Replacement implies deletion.** When a Rust slice successfully replaces a TypeScript slice, delete the corresponding TS implementation, obsolete TS tests, obsolete scripts, and obsolete docs.
4. **Unclear leftovers require a decision.** If a TS file cannot be deleted after apparent replacement, record why: frontend retained, reference retained temporarily, dev-only retained temporarily, or boundary redesigned.
5. **Test at external seams.** Prefer CLI behavior tests, operation contract tests, MCP dispatch tests, engine contract tests, HTTP API tests, and repository guard tests over private implementation tests.
6. **Keep slices independently reviewable.** Each slice should have one behavior boundary, its own tests, its own deletion list, and a clear rollback path.
7. **No compatibility drift.** Do not reintroduce `gbrain`, `GBRAIN_*`, `.gbrain`, `~/.gbrain`, or `gbrain.yml` fallback behavior.
8. **Do not confuse domain words with brand words.** `brain` and `source` remain domain terms.

## TypeScript Retention Policy

Every TypeScript file after this PRD starts must fall into exactly one category:

### Category A: Must Migrate to Rust

These are product/runtime surfaces and must eventually be Rust-owned:

- CLI entrypoint and command dispatch.
- Core engine and storage abstractions.
- PostgreSQL/libsql schema and migrations.
- Operations and trust-boundary enforcement.
- MCP tool definitions, parameter validation, dispatch, transport, and rate limiting.
- HTTP/admin backend API.
- Search, retrieval, embeddings, chunking, ingestion, source management, sync, facts, takes, timeline.
- Config/bootstrap/doctor/install/update behavior.
- Jobs, agents, minions, autopilot, and remote execution behavior.
- Evals/benchmarks that validate runtime behavior.

### Category B: May Remain TypeScript by Explicit Decision

These surfaces may stay TypeScript if named and bounded:

- Browser/admin frontend under `admin/`.
- Browser-only client SDK or UI integration code.
- Build tooling that only compiles frontend assets.
- Temporary parity harnesses used to compare TS reference behavior against Rust during a slice.

### Category C: Delete After Replacement

These surfaces should be deleted once the corresponding Rust behavior is accepted:

- TS command modules replaced by Rust CLI commands.
- TS core modules replaced by Rust core crates.
- TS MCP modules replaced by `zbrain-mcp`.
- TS backend/Express server code replaced by `zbrain-web`.
- TS tests that only covered deleted TS implementation details.
- Shell/TS scripts that exist only to drive TS runtime behavior.
- Docs/examples that mention TS-only command paths or Bun-only runtime assumptions after Rust owns the path.

### Category D: Requires Decision Before Keeping

These cannot linger silently:

- Mixed frontend/backend files.
- Dev tooling that imports runtime TS modules.
- Evals whose only implementation is TS but whose behavior should be Rust-validated.
- Package exports that expose TS internals as public API.
- Database schema identifiers with old names. Previously known examples `gbrain_cycle_locks` and `gbrain_tool_use_id` have a dedicated DB migration slice to rename them to `zbrain_cycle_locks` and `zbrain_tool_use_id`.

## Target Rust Architecture

### `zbrain-core`

Owns the domain/runtime contracts:

- structured errors and result envelope
- page/source/type models
- engine trait and backend implementations
- migrations and schema contract
- operations layer and trust boundary model
- ingestion/search/retrieval/chunking contracts
- config model and validation
- jobs/agent domain contracts where they are runtime behavior rather than CLI presentation

### `zbrain-cli`

Owns command-line UX:

- argument parser and subcommand tree
- config/bootstrap flow
- user-facing command output
- local command execution
- Rust-backed command handlers calling `zbrain-core`, `zbrain-web`, or `zbrain-mcp` as needed
- final package binary target for `zbrain`

### `zbrain-mcp`

Owns agent-facing MCP behavior:

- tool definitions generated from Rust operation contracts
- parameter validation and summaries
- trust-boundary constrained operation dispatch
- stdio and HTTP transports as needed
- rate limiting and audit hooks
- parity with current `src/mcp/*.ts` behavior before TS MCP deletion

### `zbrain-web`

Owns backend HTTP/admin behavior:

- Axum-based API server
- admin API endpoints
- auth/session/token behavior
- request logs/jobs/calibration/agent endpoints
- static serving or embedding boundary for built frontend assets
- no long-term Express backend dependency

### `admin/`

May remain React + TypeScript frontend:

- browser UI remains TypeScript by explicit decision
- backend API contracts must be Rust-owned
- frontend build scripts may remain if they do not import TS runtime internals

## Migration Roadmap

The migration roadmap has been split into smaller plan files: completed work is archived in `docs/plans/ZBRAIN_TS_TO_RUST_PART1_COMPLETED.md`, the active Config/Bootstrap/Package Entrypoint work is tracked in `docs/plans/ZBRAIN_TS_TO_RUST_PART2_CONFIG_BOOTSTRAP.md`, and remaining unfinished domains should be split into later parts before execution. The following roadmap remains the domain-level PRD inventory.

### Phase 0: Roadmap and Inventory

- Restore/create canonical roadmap JSON and rendered Markdown view.
- Classify all TypeScript surfaces into Category A/B/C/D.
- Record current Rust coverage and TS reference surfaces.
- Define deletion gates and verification commands.

### Phase 1: Core Storage Parity Closure

- Finish Page contract parity across InMemory/PostgreSQL/libsql.
- Add missing advanced Page writes if still TS-only.
- Normalize migration ownership under Rust.
- Implement and validate the DB schema identifier rename strategy: `gbrain_cycle_locks` -> `zbrain_cycle_locks`, `gbrain_tool_use_id` -> `zbrain_tool_use_id`.
- Delete TS storage methods and tests only when Rust owns equivalent behavior.

### Phase 2: Config, Bootstrap, and Package Entrypoint

- Port config discovery/loading/writing to Rust.
- Port `init`, `doctor`, `config`, `storage`, `schema`, and migration commands.
- Move package `bin.zbrain` to the Rust binary after command bootstrap parity exists.
- Replace Bun-only runtime assumptions with Rust package/install flow.
- Delete TS bootstrap/config/storage command modules and obsolete tests.

### Phase 3: Operations Layer and Trust Boundary

- Port operation definitions, schemas, context, and trust checks to Rust.
- Preserve local trusted vs remote constrained caller behavior.
- Move shared CLI/MCP operation dispatch to `zbrain-core`.
- Add operation contract tests.
- Delete TS operations code after MCP/CLI callers use Rust operations.

### Phase 4: MCP Server Migration

- Implement `zbrain-mcp` tool definition generation, parameter validation, dispatch, transport, rate limiting, and audit behavior.
- Prove parity with current `src/mcp/*.ts` using MCP dispatch/transport tests.
- Cut package/docs/examples to Rust MCP server.
- Delete `src/mcp/*.ts` and obsolete MCP tests after parity.

### Phase 5: Web Backend Migration

- Implement `zbrain-web` Axum backend API.
- Port auth/session/token, request logs, jobs, calibration, agents, and admin API endpoints.
- Decide static frontend serving boundary.
- Keep `admin/` TypeScript frontend if still appropriate.
- Delete Express/TS backend command/server code after API parity.

### Phase 6: Ingestion, Sources, Search, and Retrieval

- Port source management, import/capture/extract, frontmatter, file ingestion, sync, and reindex flows.
- Port embeddings/chunking/search/hybrid retrieval behavior or redesign with explicit decisions.
- Add corpus/index parity tests.
- Delete TS ingestion/search/source command/core modules after Rust replacement.

### Phase 7: Facts, Takes, Timeline, Salience, and Graph

- Port facts/takes/timeline/salience/backlinks/orphans/graph query behavior.
- Add contract and CLI behavior tests.
- Delete replaced TS modules and tests.

### Phase 8: AI Gateway, Providers, Models, and Routing

- Port provider config, model capability/pricing registry, routed gateway behavior, backoff/retry, and audit constraints.
- Preserve direct-provider guardrails.
- Add tests for routed/no-direct-provider behavior.
- Delete TS gateway/provider modules after Rust callers are canonical.

### Phase 9: Jobs, Agents, Minions, Autopilot, and Remote Execution

- Port jobs, agent logs, minions, autopilot, remote execution, and fanout flows.
- Preserve privacy/PII guardrails and trust boundaries.
- Add integration tests around job lifecycle and agent command behavior.
- Delete replaced TS agent/job modules.

### Phase 10: Evals, Benchmarks, and Developer Tooling

- Decide which evals/benchmarks remain product-critical.
- Port critical eval harnesses or redefine them as external fixtures around Rust binaries/APIs.
- Delete obsolete TS-only eval/dev tools.
- Keep only explicitly named frontend/build tooling TS where justified.

### Phase 11: Final Cutover and Repository Cleanup

- Remove TS runtime package exports.
- Remove TS CLI entrypoint and command/core/MCP/backend runtime directories once empty or explicitly retained.
- Remove Bun runtime dependency if it is no longer needed outside frontend/tooling.
- Verify no stale TS runtime assumptions in docs/examples/scripts.
- Keep brand guard and add TS runtime residue guard.
- Run final Rust workspace verification and targeted retained-TS checks.

## Slice Completion Definition

A migration slice is complete only when all of the following are true:

1. The Rust replacement is implemented in the correct crate boundary.
2. External behavior is covered by Rust tests, CLI tests, MCP tests, HTTP tests, or contract tests.
3. The corresponding TypeScript implementation is deleted, or a retention decision is recorded.
4. Obsolete TypeScript tests are deleted or rewritten to target the Rust seam.
5. Docs, examples, package exports, scripts, and roadmap state are updated.
6. Repository scans show no accidental references to deleted TS paths.
7. The slice can be reviewed independently without unrelated migration churn.

## Deletion Rule

For every Rust slice, maintain a deletion checklist:

```text
Rust replacement:
TS implementation to delete:
TS tests to delete or port:
Scripts/docs/examples to update:
Package exports/bin impact:
Retention decisions required:
Verification commands:
```

If the deletion checklist cannot be completed, the slice cannot be marked complete until the reason is recorded.

## Testing and Verification Strategy

### Per-Slice Verification

Use the seam closest to the user/caller:

- Storage behavior: Rust engine contract tests across InMemory/PostgreSQL/libsql.
- CLI behavior: run the Rust binary and assert command output/exit behavior.
- Operations: contract tests for schemas, trust boundary, context construction, and dispatch.
- MCP: tool definition, parameter validation, dispatch, transport, rate limiting, and audit tests.
- Web backend: HTTP API tests around Axum routes and auth/session behavior.
- Search/ingestion: fixture corpus tests and deterministic retrieval assertions.
- AI gateway: routed provider tests and no-direct-provider guard tests.

### Final Verification

The final cutover should include:

```bash
cargo fmt --all --manifest-path Cargo.toml -- --check
cargo build --manifest-path Cargo.toml --workspace
cargo test --manifest-path Cargo.toml --workspace
cargo clippy --manifest-path Cargo.toml --workspace --all-targets -- -D warnings
bun run typecheck
bun test tests/unit/zbrain-brand-guard.test.ts
```

`bun run typecheck` and TS tests may still exist for retained frontend/tooling TypeScript. They must not imply that TypeScript runtime is still canonical.

### Repository Guards

Add or preserve guards for:

- no public GBrain brand regression
- no package export pointing at deleted TS runtime modules
- no docs/examples referencing deleted TS entrypoints
- no TS runtime residue outside explicit retention allowlist
- no skipped PostgreSQL tests counted as pass

## User Stories

1. As a ZBrain maintainer, I want Rust to become the canonical runtime, so that the repository no longer splits attention between two product lines.
2. As a ZBrain maintainer, I want TypeScript code to remain only until its behavior has a Rust replacement, so that migration does not delete reference behavior too early.
3. As a ZBrain maintainer, I want each migrated slice to delete corresponding TypeScript code/tests/scripts/docs, so that the legacy line shrinks continuously.
4. As a maintainer, I want browser/frontend TS to be explicitly retained when appropriate, so that UI work is not confused with backend runtime migration.
5. As a CLI user, I want the final `zbrain` binary to be Rust-backed, so that install, startup, and command behavior do not depend on TS runtime internals.
6. As an agent caller, I want MCP behavior and trust boundaries to survive the migration, so that remote callers remain constrained.
7. As a contributor, I want migration slices to be independently reviewable, so that each PR has a clear scope and deletion checklist.
8. As a documentation reader, I want docs to describe the Rust-first architecture, so that I can tell which components are canonical and which are legacy/reference.

## Implementation Decisions

- ZBrain is the canonical product language. No GBrain compatibility aliases/fallbacks are preserved.
- Rust is the canonical runtime target.
- TypeScript runtime code is not deleted until replaced by a tested Rust slice.
- Rust replacement and TypeScript deletion are part of the same slice definition.
- Browser/admin frontend TypeScript may remain by explicit decision.
- `brain` and `source` remain domain terms.
- Historical GBrain changelog content remains reset; ZBrain release history starts from the first ZBrain release.
- Roadmap JSON is the source of truth for execution status. Markdown roadmap views are rendered from JSON.
- DB schema identifiers with old names require dedicated migration decisions and are not public brand compatibility.
- The DB identifier decision is made: rename `gbrain_cycle_locks` -> `zbrain_cycle_locks` and `gbrain_tool_use_id` -> `zbrain_tool_use_id` with a dedicated migration slice.

## Open Decisions

1. Which package manager/runtime responsibilities remain with Bun after Rust owns CLI/backend runtime?
3. Which eval harnesses are product-critical enough to port versus archive/delete?
4. Should Rust expose a public library API equivalent to current `package.json exports`, or should the package become primarily binary/API-server oriented?
5. What is the exact static asset boundary between `zbrain-web` and `admin/`?

## Out of Scope

- Preserving GBrain compatibility for hypothetical online users.
- Rewriting the React admin frontend into Rust/WASM solely for language uniformity.
- Replacing every developer convenience script before runtime migration closes.
- Treating this PRD as a license to redesign unrelated product behavior without roadmap decisions.

## Further Notes

This PRD intentionally treats TypeScript as a shrinking reference layer, not as a coequal long-term product line. The operating rule is simple: when Rust successfully replaces a slice, delete the corresponding TypeScript slice. When deletion is not obvious, make the decision explicit instead of letting legacy code survive by inertia.
