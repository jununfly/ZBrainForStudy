# 1-2-3-6: Validate and close schema migrations ownership transfer

Date: 2026-06-25
Parent roadmap node: 1-2-3 Move schema migrations ownership to Rust

## Scope

Final validation gate for the schema migrations ownership transfer.

This node completes only when:
1. All 5 child slices are done (1-2-3-1 through 1-2-3-5)
2. Compilation checks pass for both Rust and TypeScript
3. No TS migration code remains in the codebase

**This is a VALIDATION ONLY node - no new code, no new features.**

### In scope:
1. **Rust compile check**: `cargo check -p zbrain-core` passes
2. **TS compile check**: `bun typecheck` passes
3. **Code audit**: `grep -rn "MIGRATION_\|sqlx::migrate\|PRAGMA user_version" src/` returns zero hits
4. **Roadmap update**: all 6 children marked completed
5. **Parent node closure**: 1-2-3 marked as completed

### Out of scope:
- No new code, no new features
- No end-to-end runtime tests (covered by individual backend slices)
- No handler/verify logic implementation

---

## Decisions (Grill complete)

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| 1 | Validation scope | Compile + code structure only (A) | Fast, no runtime dependencies. Individual backend tests already cover behavior thoroughly. |
| 2 | Parent completion criteria | All 6 children must be completed first (A) | Strict completion gate. Audit → foundation → libsql → postgres → bridge → validation, in order. |

---

## Validation Checklist

Execute all three checks:

### 1. Rust Compile Check
```bash
cd crates/zbrain-core
cargo check -p zbrain-core 2>&1 | grep "^error\[" | wc -l
# Expected: 0 errors
```

### 2. TypeScript Compile Check
```bash
bun typecheck 2>&1 | grep -E "^src/.*error" | wc -l
# Expected: 0 errors
```

### 3. TS Migration Code Audit
```bash
# Check for leftover TS migration code
grep -rn "MIGRATION_" src/ --include="*.ts" --include="*.tsx" | wc -l
# Expected: 0

grep -rn "sqlx::migrate" crates/zbrain-core/src/ --include="*.rs" | wc -l
# Expected: 0

grep -rn "PRAGMA user_version" crates/zbrain-core/src/ --include="*.rs" | wc -l
# Expected: 0 (libsql switched to rust_schema_version)
```

### 4. Roadmap Integrity Check
- [ ] 1-2-3-1 = completed
- [ ] 1-2-3-2 = completed
- [ ] 1-2-3-3 = completed
- [ ] 1-2-3-4 = completed
- [ ] 1-2-3-5 = completed
- [ ] 1-2-3-6 = completed (this node)
- [ ] 1-2-3 = completed (parent)

---

## Acceptance Criteria

1. ✅ `cargo check -p zbrain-core`: 0 errors
2. ✅ `bun typecheck`: 0 errors
3. ✅ Migration code audit: 0 hits (MIGRATION_*, sqlx::migrate, PRAGMA user_version)
4. ✅ All 6 child nodes marked completed in roadmap JSON and MD
5. ✅ Parent node 1-2-3 marked completed

---

## Next Node

**1-2-4:** Decide internal DB legacy identifier migration (if not done yet)
OR
**Next available pending node** from the 1-2-* sequence
