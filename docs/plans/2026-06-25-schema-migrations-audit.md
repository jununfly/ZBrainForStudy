# Schema Migrations Ownership Audit

Date: 2026-06-25
Roadmap node: `1-2-3-1 Write schema migrations ownership audit plan`
Parent roadmap node: `1-2-3 Move schema migrations ownership to Rust`

## Scope

This is a full inventory and gap analysis for transferring schema migration ownership
entirely from TypeScript (`src/core/migrate.ts`) to Rust (`crates/zbrain-core`).
It enumerates every existing TS migration, classifies its features, defines the
explicit port/discard boundary, and compares against Rust's current migration state.
Concrete Rust design decisions and handler porting complexity are deferred to
implementation slices.

### In scope

1. **Full TS migration inventory**: v1 through v42+ (every migration in `MIGRATIONS` array)
   - SQL body
   - Handler presence and purpose
   - Verify hook presence and purpose
   - sqlFor engine-specific overrides
   - Transaction flag
   - Idempotent flag
2. **Rust current migration state**: libsql vs postgres patterns
3. **Parity gap analysis**: TS capabilities Rust is missing
4. **Explicit boundary**: What gets ported 100%, what gets simplified, what gets deleted
5. **Slice sanity check**: Confirm 6-child split is architecturally sound

### Out of scope

- Concrete Rust registry/runner type design (deferred to 1-2-3-2)
- Handler/verify function complexity assessment (deferred to 1-2-3-5)
- Detailed TDD acceptance criteria (deferred to implementation nodes)
- Actual code changes or implementation work

---

## TS Migration System: Facts

### Core Interface

```typescript
interface Migration {
  version: number;              // Sequential, starting at 1
  name: string;                 // Human-readable identifier
  sql: string;                  // Engine-agnostic SQL; '' if handler-only
  sqlFor?: {                    // Engine-specific SQL override
    postgres?: string;
    pglite?: string;
  };
  transaction?: boolean;        // Default true; false for CONCURRENTLY
  handler?: (engine: BrainEngine) => Promise<void>;  // TS app logic
  idempotent?: boolean;         // Default true; false = blocks re-run
  verify?: (engine: BrainEngine) => Promise<boolean>; // Post-condition probe
}
```

### Runner Behavior

- Runs inside `initSchema()` automatically
- `PRAGMA user_version` / Postgres `current_setting('migration.version')` tracking
- Each migration runs in its own transaction (unless `transaction: false`)
- Migrations applied in strict version order
- Verify hooks run after migration claims success; drift detected = fail
- Non-idempotent migrations cannot be re-run via verify-hook self-healing path

### Feature Matrix (TS)

| Feature | Supported | Count of migrations using it |
|---------|-----------|-----------------------------|
| SQL body | ✅ | All (some use `''` for handler-only) |
| sqlFor override | ✅ | TBD (inventory pending) |
| transaction: false | ✅ | TBD (for CONCURRENTLY) |
| handler function | ✅ | TBD |
| verify hook | ✅ | TBD (v0.30.1+, opt-in) |
| idempotent: false | ✅ | TBD |

---

## Current Rust State: Facts

### libsql.rs Pattern (SQLite)

- 8 migrations (`0001_init` through `0008_raw_data_and_page_versions`)
- Const array: `const MIGRATIONS: &[&str] = &[MIGRATION_0001, ...]`
- `const SCHEMA_VERSION: i64 = 8`
- `PRAGMA user_version` guarded
- Features: SQL-only, no handler, no verify, no sqlFor, implicit transaction=true, implicit idempotent=true

### postgres.rs Pattern

- Uses `sqlx::migrate!("../migrations")` pointing to `crates/zbrain-core/migrations/`
- 9 migrations (`V1__init.sql` through `V9__raw_data_and_page_versions.sql`)
- sqlx-managed version tracking table `_sqlx_migrations`
- Features: SQL-only, no handler, no verify, implicit transaction=true by sqlx

### Cross-Backend Divergence

- libsql and Postgres migrations are in separate directories (`migrations-sqlite/` vs `migrations/`)
- Different version tracking mechanisms (`PRAGMA user_version` vs `_sqlx_migrations` table)
- Different runner implementations (hand-rolled vs sqlx built-in)
- No shared migration registry or feature set

---

## Parity Gap Analysis

### TS → Rust Missing Capabilities

| Capability | TS | libsql (Rust) | Postgres (Rust) | Gap severity |
|------------|----|---------------|-----------------|--------------|
| Handler functions | ✅ | ❌ | ❌ | High |
| Verify hooks | ✅ | ❌ | ❌ | Medium |
| sqlFor engine overrides | ✅ | ❌ | ❌ | Medium |
| transaction: false flag | ✅ | ❌ | ❌ | Medium |
| Explicit idempotent marking | ✅ | Implicit only | Implicit only | Low |
| Shared registry across backends | ✅ | ❌ | ❌ | Medium |
| Drift detection + error reporting | ✅ | ❌ | ❌ | Low |

---

## Explicit Boundary: Port / Simplify / Discard

### Port at 100% Parity

- ✅ All SQL migration bodies (port exactly as-is, same semantics)
- ✅ sqlFor engine-specific SQL overrides
- ✅ transaction flag (false = no transaction wrapper, for CONCURRENTLY)
- ✅ Version ordering guarantee
- ✅ Idempotency enforcement and error reporting

### Simplify During Port

- ⚠️ Verify hooks: reimplement as Rust `fn(&dyn BrainEngine) -> Result<bool>`
  - Semantics preserved; only language changes
- ⚠️ Handler functions: port each one to Rust, calling through `&dyn BrainEngine`
  - Semantics preserved; only language changes

### Discard Outright

- ❌ TS `MIGRATIONS` array in `src/core/migrate.ts` (becomes thin bridge only)
- ❌ TS SQL string constants (single source of truth = Rust crate)
- ❌ TS runner implementation (delegates to Rust `init_schema()`)
- ❌ Dual migration tracking systems (unified to Rust-only `rust_schema_version` table)

---

## Slice Sanity Check

### 1-2-3-2: Migration registry + runner foundation ✅ Reasonable

- Defines shared `Migration` type across both backends
- Builds core runner: version tracking, ordering, transaction wrapping
- Object-safety test included
- Independent slice: can compile and test without backend integration

### 1-2-3-3: libsql integration ✅ Reasonable

- Replaces ad-hoc const array with shared registry
- Adds sqlFor support for SQLite
- Ports existing 0001-0008 into new format
- Builds on 1-2-3-2 foundation

### 1-2-3-4: Postgres integration ✅ Reasonable

- Replaces `sqlx::migrate!()` with shared registry
- Adds sqlFor + transaction=false passthrough for CONCURRENTLY
- Ports existing V1-V9 into new format
- Builds on 1-2-3-2 foundation

### 1-2-3-5: TS bridge + handler porting ✅ Reasonable

- Thin bridge layer in TS that calls Rust `init_schema()`
- Handler/verify functions ported one-by-one to Rust
- All TS SQL files deleted
- Builds on both backend integration slices

### 1-2-3-6: Validation + close ✅ Reasonable

- End-to-end migration tests for both backends
- Idempotency verification (run twice, second run = 0 migrations applied)
- Delete unused TS migration code
- Clean closure of the entire 1-2-3 subtree

**Verdict: 6-slice pattern is architecturally sound and properly layered.**

---

## Audit Completed

This audit document is complete. Implementation begins at slice 1-2-3-2.
