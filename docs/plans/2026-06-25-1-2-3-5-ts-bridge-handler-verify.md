# 1-2-3-5: Build TS bridge + port handler/verify functions to Rust

Date: 2026-06-25
Parent roadmap node: 1-2-3 Move schema migrations ownership to Rust

## Scope

Complete the migration ownership transfer:

1. **TS Bridge**: `src/core/migrate.ts` becomes a thin single-line delegation to `engine.initSchema()`
2. **Trait Extension**: Add `handler()` and `verify()` methods to the `Migration` trait
3. **Backend Implementation**: Implement handler/verify in both `LibsqlMigration` and `PostgresMigration`
4. **Full TS Cleanup**: Delete ALL embedded SQL files + MIGRATIONS array from TypeScript

**Hard cutover**: No dual runner period, no fallback to TS migrations. Rust is single source of truth.

### In scope:
1. Extend `Migration` trait with `handler(&self, &dyn BrainEngine) -> Result<()>` and `verify(&self, &dyn BrainEngine) -> Result<bool>`
2. Implement handler/verify stubs in `LibsqlMigration` and `PostgresMigration`
3. Rewrite `src/core/migrate.ts:runMigrations()` as:
   ```typescript
   export async function runMigrations(engine: BrainEngine): Promise<void> {
       await engine.initSchema();
   }
   ```
4. Delete all embedded SQL `const MIGRATION_*` strings from TS
5. Delete the entire `MIGRATIONS: Migration[]` array from TS
6. Delete unused TS migration types/interfaces if nothing else uses them

### Out of scope:
- Actual handler logic implementations (beyond stubs returning Ok(()) / Ok(true))
- Actual verify logic implementations (beyond stubs returning Ok(true))
- Integration tests for TS bridge (compile-level verification only per Q4)

---

## Decisions (Grill complete)

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| 1 | TS bridge calling pattern | `engine.init_schema()` through BrainEngine trait (A) | Reuse existing abstraction. TS runMigrations already receives engine: BrainEngine. Single line delegation. |
| 2 | TS side cleanup scope | Full delete: MIGRATIONS array + all .sql files (A) | No production users = no history to preserve. Cleanest cutover, no technical debt. |
| 3 | Handler/Verify porting strategy | Both become distinct Migration trait methods (A) | Type-safe, compiler-enforced across all backends. No optional implementations = no behavior gaps. |
| 4 | Testing strategy | Compile-level verification only (B) | Bridge is 1 line of delegation. Rust migration behavior is already fully tested by backend-specific tests. |

---

## Implementation Plan (2 vertical slices)

### Slice 1: Migration trait extension + backend implementations

**Files:**
- `crates/zbrain-core/src/migration.rs` - extend `Migration` trait with handler and verify methods
- `crates/zbrain-core/src/libsql.rs` - implement handler/verify for `LibsqlMigration` (stubs: `Ok(())` / `Ok(true)`)
- `crates/zbrain-core/src/postgres.rs` - implement handler/verify for `PostgresMigration` (stubs: `Ok(())` / `Ok(true)`)

**Note:** Initially all implementations are stubs returning success. Actual logic porting can be done slice-by-slice later if needed.

### Slice 2: TS bridge rewrite + full cleanup

**File:** `src/core/migrate.ts`

- Rewrite `runMigrations()` to single-line `engine.initSchema()` call
- Delete `MIGRATIONS` array and all `MIGRATION_*` SQL string constants
- Delete `Migration` interface/type if no other references
- Delete `isMigrationIdempotent`, `MigrationDriftError` and other migration-only utilities if unused
- Leave only `runMigrations` + any exports that external callers depend on

---

## Acceptance Criteria

1. ✅ `cargo check -p zbrain-core` passes after trait extension
2. ✅ `bun typecheck` passes after TS rewrite (no broken imports/references)
3. ✅ No SQL string constants remain in `src/core/migrate.ts`
4. ✅ No `MIGRATIONS` array remains in TypeScript codebase
5. ✅ `Migration` trait has both `handler` and `verify` methods
6. ✅ Both `LibsqlMigration` and `PostgresMigration` implement all methods

---

## Next Node

**1-2-3-6:** Validate and close schema migrations ownership transfer
