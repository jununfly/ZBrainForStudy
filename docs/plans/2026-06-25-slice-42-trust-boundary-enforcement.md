# Slice #42: Trust boundary enforcement core logic

**Node:** 1-4-2-1 (Security enforcement layer)
**Issue:** #42
**Security Level:** ⚠️ HIGH (Security-critical)
**Depends On:** #40, #41

---

## Grill Decisions (Confirmed)

| Q | Answer | Rationale |
|---|--------|-----------|
| Q1: `localOnly` enforcement | **A) Dispatch layer unified check** | Single source of truth, no operation can bypass |
| Q2: D18 `image_path` constraint | **A) Independent guard function** | Clear, reusable, unit testable |
| Q3: Subagent prefix enforcement | **A) Independent guard function** | 1:1 TS parity, behavior verified independently |

---

## Scope

### 1. Security Guard Functions (3 Core Enforcement Points)

#### Guard 1: `enforce_local_only(operation_name: &str, local_only: bool, ctx: &OperationContext) -> OperationResult<()>`
**Behavior:**
- If `local_only = true` AND `ctx.remote = true` → reject with `permission_denied`
- Error message must match TS exactly: `"Operation '{name}' is only available locally (MCP/remote callers cannot use it)"`
- Returns `Ok(())` otherwise

#### Guard 2: `enforce_d18_image_path_constraint(ctx: &OperationContext, image_path: Option<&str>) -> OperationResult<()>`
**D18 Security Constraint (operations.ts:3703-3711):**
- Remote callers (`ctx.remote = true`) CANNOT pass `image_path`
- If remote AND image_path is `Some(_)` → reject with `permission_denied`
- Error message must match TS exactly: `"image_path is not permitted for remote callers (D18). Use image_url or image_data instead."`
- Local callers bypass this check (user owns the filesystem)

#### Guard 3: `enforce_subagent_put_page_prefix(ctx: &OperationContext, slug: &str) -> OperationResult<()>`
**Subagent Trust Boundary (v0.23 dream cycle):**
- If `ctx.via_subagent = true` OR `ctx.subagent_id.is_some()` → check slug against `allowed_slug_prefixes`
- If `allowed_slug_prefixes` is `None` or empty → fall back to legacy subagent namespace check (`wiki/agents/{id}/%`)
- Prefix matching: `prefix/*` matches any slug starting with `prefix/`
- Exact match: bare prefix (no trailing `/*`) matches that exact slug only
- **Fail-closed:** If no prefixes match AND legacy namespace also doesn't match → reject
- Error message: `"Subagent cannot write to page '{slug}'. Allowed prefixes: {prefixes.join(', ')}"`
- Returns `Ok(())` if local caller OR not subagent context OR match found

### 2. Upload Validator Strict/Loose Mode Tests (Already Implemented in #41, Needs Security Tests)
**Test cases for path traversal protection:**
- `../` traversal attempts (strict mode blocks, loose mode passes)
- Nested `../../secret/file`
- URL-encoded `%2e%2e%2f`
- Unicode equivalent attacks
- Symlink escape attacks
- Windows path traversal (`..\`)

### 3. Security Test Suite (Unit Tests)

#### Guard 1 Tests:
- ✅ Local-only op rejected when `remote = true`
- ✅ Local-only op allowed when `remote = false`
- ✅ Non-local-only op always allowed
- ✅ Error message byte-for-byte match

#### Guard 2 Tests (D18):
- ✅ Remote caller with `image_path = Some("...")` → rejected
- ✅ Remote caller with `image_path = None` → allowed
- ✅ Local caller with `image_path = Some("...")` → allowed
- ✅ Error message byte-for-byte match

#### Guard 3 Tests (Subagent Prefix):
- ✅ Not subagent context → always allowed
- ✅ Subagent context, slug matches prefix → allowed
- ✅ Subagent context, slug does NOT match any prefix → rejected
- ✅ Empty prefix list falls back to legacy namespace
- ✅ Exact prefix match (no `/*`) works correctly
- ✅ Wildcard prefix match (`prefix/*`) works correctly
- ✅ Error message includes allowed prefix list

#### Path Validation Security Tests:
- ✅ Strict mode blocks parent-directory symlinks (B5)
- ✅ Strict mode blocks all forms of `../`
- ✅ Loose mode allows parent directory access (user-owned filesystem)
- ✅ Always rejects final-component symlinks (both modes)

---

## Acceptance Criteria

- [ ] All 3 guard functions implemented
- [ ] Every error message matches TS byte-for-byte
- [ ] 100% test coverage for all security branches
- [ ] All documented attack vectors covered in tests
- [ ] Guards are independently unit testable (no engine dependency)
- [ ] Security tests tagged with `#[cfg(test)]` and clearly documented

---

## Implementation Plan

| Phase | Action | File |
|-------|--------|------|
| 1 | Implement `enforce_local_only` guard | `crates/zbrain-core/src/operation/trust_boundary.rs` |
| 2 | Implement `enforce_d18_image_path_constraint` guard | Same |
| 3 | Implement `enforce_subagent_put_page_prefix` guard | Same |
| 4 | Add `matches_slug_prefix` helper (extracted from existing validator) | Same |
| 5 | Add unit tests for all 3 guards | Same file, mod tests |
| 6 | Add path traversal security tests for upload validator | `crates/zbrain-core/src/operation.rs` |
| 7 | Add module export to `lib.rs` | `crates/zbrain-core/src/lib.rs` |

---

## Security Notes

**Defense in depth principle:**
- All guards are standalone and composable
- Dispatch layer calls `enforce_local_only` for ALL ops
- Individual operation handlers call D18 / subagent guards as needed
- No single point of failure in the trust boundary

**Error message discipline:**
- NEVER leak internal paths or debug info in error messages
- Always return the EXACT same error string as TS (attackers fingerprint error messages)
- `OperationError.suggestion` can have guidance for legitimate users

---

## Estimates

- **Lines of code:** ~150 guards + ~300 tests = ~450
- **Test cases:** 25+ security-focused
- **Risk:** Low (pure functions, no side effects)
