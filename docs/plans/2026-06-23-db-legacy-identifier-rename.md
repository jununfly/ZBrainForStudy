# DB Legacy Identifier Rename Plan

> Scope: planning only. Do not modify schema/code in this step. The implementation should be a separate TDD/tracer-bullet slice.

## Goal

Remove the remaining internal DB schema identifiers that still carry the old GBrain brand:

- `gbrain_cycle_locks` -> `zbrain_cycle_locks`
- `gbrain_tool_use_id` -> `zbrain_tool_use_id`

This follows the already-confirmed ZBrain migration rule: there are no online users, so first-stage brand migration can be breaking and should not keep GBrain compatibility aliases, fallback reads, or duplicate internal names.

## Non-goals

- Do not rename domain words such as `brain` or `source` when they are not brand references.
- Do not implement a compatibility layer for `gbrain_*` names.
- Do not bundle unrelated TS -> Rust storage parity work into this slice.
- Do not treat this as completing Rust migration ownership of all schema migrations; this is one targeted schema-name cleanup.

## Current known touchpoints

### Schema DDL

Update fresh schema definitions so new databases never create the old names:

- `src/schema.sql`
- `src/core/schema-embedded.ts`
- `src/core/pglite-schema.ts`

Expected changes:

- `CREATE TABLE IF NOT EXISTS gbrain_cycle_locks` becomes `CREATE TABLE IF NOT EXISTS zbrain_cycle_locks`
- `gbrain_tool_use_id UUID` becomes `zbrain_tool_use_id UUID`
- any index/constraint/comment names that include `gbrain` should be renamed if present

### Migration layer

Add a new migration after the current latest migration. The migration should rename existing DB objects in-place:

- table rename: `gbrain_cycle_locks` -> `zbrain_cycle_locks`
- column rename on `subagent_tool_executions`: `gbrain_tool_use_id` -> `zbrain_tool_use_id`

Known files to inspect/update:

- `src/core/migrate.ts`
- `src/commands/migrations/v0_18_1.ts`

Implementation detail to verify before coding:

- SQLite/libsql and PostgreSQL syntax differs for conditional/idempotent renames.
- If the migration runner guarantees each migration runs once, prefer the simplest valid per-dialect rename statements.
- If existing project migrations are defensive/idempotent, follow the existing convention instead of inventing a new pattern.

### Runtime SQL

Update all raw SQL call sites that read/write these schema identifiers:

- `src/core/db-lock.ts`
- `src/core/cycle.ts`
- `src/commands/doctor.ts`
- `src/core/minions/handlers/subagent.ts`

Expected runtime changes:

- all lock operations target `zbrain_cycle_locks`
- subagent crash-replay stable key column becomes `zbrain_tool_use_id`
- SQL aliases and selected field names should be consistent; do not keep a public API field named `gbrain_tool_use_id` unless a test proves an external contract requires it

### Tests and fixtures

Update hard-coded schema references in test code and scripts:

- `tests/unit/**/*.test.ts`
- `tests/unit/core/cycle.serial.test.ts`
- `tests/unit/e2e/**/*.test.ts`
- `tests/heavy/sync_lock_regression.sh`

Expected test changes:

- assertions query `zbrain_cycle_locks`
- selected/inserted columns use `zbrain_tool_use_id`
- any migration tests should prove old DBs are upgraded and fresh DBs are created with only ZBrain names

### Eval and docs references

Update only current/canonical docs and active eval harnesses:

- `src/eval/longmemeval/harness.ts`
- `docs/eval-bench.md`
- `docs/prd/complete-ts-to-rust.md`
- `docs/architecture/system-of-record.md`
- `TODOS.md`
- `CLAUDE.md`
- `skills/migrations/v0.17.0.md`

Generated or aggregate files such as `llms-full.txt` should be regenerated if the project has a documented generation command; otherwise update only if it is maintained manually in this repository.

### Brand guard

Update the brand guard so the two old identifiers are no longer allowed exceptions:

- `tests/unit/zbrain-brand-guard.test.ts`

Expected end state:

- remove `/gbrain_cycle_locks/` allowlist entry
- remove `/gbrain_tool_use_id/` allowlist entry
- keep unrelated intentional allowlist entries only if still valid

## Recommended implementation sequence

### Step 1: Add failing migration/brand tests

Create or update tests that prove the desired behavior before production edits:

1. Fresh schema creation contains `zbrain_cycle_locks` and `zbrain_tool_use_id`.
2. Fresh schema creation does not contain `gbrain_cycle_locks` or `gbrain_tool_use_id`.
3. Migrating an existing DB with old objects results in the new table/column names.
4. Brand guard fails if either legacy identifier remains outside intentionally historical migration text.

Keep each test focused; avoid a horizontal batch of imagined tests if implementation discoveries change the plan.

### Step 2: Update fresh schema definitions

Change the three schema DDL sources together:

- `src/schema.sql`
- `src/core/schema-embedded.ts`
- `src/core/pglite-schema.ts`

Then rerun the fresh-schema tests.

### Step 3: Add the rename migration

Add the next migration using existing project migration conventions.

Migration acceptance criteria:

- existing data in `gbrain_cycle_locks` is preserved under `zbrain_cycle_locks`
- existing `subagent_tool_executions.gbrain_tool_use_id` values are preserved under `zbrain_tool_use_id`
- migration is applied for each supported backend path that currently owns these schema objects
- migration does not create duplicate old+new objects

### Step 4: Update runtime SQL

Update runtime consumers after schema/migration tests establish the new names:

- lock acquisition, extension, release, inspection, stale lock listing
- cycle lock cleanup or doctor checks
- subagent tool execution replay lookup and write paths

### Step 5: Update tests, eval, docs, and brand guard

Replace hard-coded test references and active documentation references.

The brand guard should become the final safety net: after this step, no non-historical GBrain identifier should remain.

### Step 6: Run validation

Preferred validation commands, adjusted to the repository's actual package scripts:

```bash
bun test tests/unit/zbrain-brand-guard.test.ts
bun test tests/unit/core/cycle.serial.test.ts
bun test tests/unit/e2e
bun test
```

Known local caveat: this environment previously lacked `bun`, so if unavailable, record the blocker instead of claiming test pass.

## Risk checklist

- **Dialect risk:** `ALTER TABLE ... RENAME COLUMN` support and conditional rename syntax may differ between SQLite/libsql and PostgreSQL.
- **Replay-key risk:** `zbrain_tool_use_id` is used for crash-replay stability; renaming must preserve values exactly.
- **Fresh-vs-upgrade drift:** fresh schema DDL and migration-upgraded schema must converge.
- **Generated-doc drift:** avoid manually editing generated aggregate docs if regeneration is available.
- **Brand guard false positives:** historical migration notes may still mention old identifiers; decide whether those are acceptable historical references or should be rewritten as migration comments with explicit context.

## Done criteria

- New databases use only `zbrain_cycle_locks` and `zbrain_tool_use_id`.
- Upgraded databases preserve old data under the new names.
- Runtime SQL no longer references the old names.
- Active tests/eval/docs no longer depend on the old names.
- Brand guard no longer allowlists `gbrain_cycle_locks` or `gbrain_tool_use_id` as live exceptions.
- Validation commands have been run, or an explicit environment blocker is recorded.

## Next roadmap action

After this plan is accepted, add or activate a separate implementation node under `1-2 Core storage parity closure`, for example:

- `1-2-5 Implement DB legacy identifier rename migration`

That node should own the code changes and TDD execution. Node `1-2-4` should remain the decision/planning slice.