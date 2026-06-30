# 1-4: Operations layer and trust boundary migration

Date: 2026-06-30
Parent roadmap node: 1-4 Operations layer and trust boundary migration

## Scope

Incrementally migrate operations from TypeScript to Rust. Build trust boundary enforcement in Rust.

### Architecture Summary

Current State:
- **TypeScript**: ~50 operations (full list in `src/core/operations.ts`)
- **Rust**: 2 operations implemented (`get_page`, `think`)
- **Trust Boundary**: Fully implemented in TS (`localOnly` flag, thin client routing)

### In scope for 1-4:
1. **1-4-1**: Port operation definitions, schemas, and context
2. **1-4-2**: Port local and remote trust boundary enforcement
3. **1-4-3**: Move shared CLI MCP dispatch to Rust operations

### Out of scope:
- MCP server full migration (deferred to 1-5)
- Individual operation behavior changes
- New operation feature additions

---

## Decisions

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| 1 | Migration order? | High-impact ops first | Prioritize most-used: `query`, `search`, `add_source`, `put_page`, then batch the rest |
| 2 | Dual-running strategy? | Registry-based dispatch | Rust registry handles implemented ops; fall back to... (TBD) |
| 3 | Thin client routing? | Replicate in Rust CLI | Same routing logic as TS: check `isThinClient` → route to remote MCP |
| 4 | Trust boundary checks? | `OperationContext` method | `ctx.is_allowed(operation)` pattern, enforced at dispatch |

---

## Implementation Plan (3 slices)

### 🟢 Slice 1-4-1: Port operation definitions, schemas, and context

**Status**: Already partially done (get_page + think work)

**Remaining work**:
- [x] `Operation` trait with `name()`, `handler()`, `params()` schema
- [x] `OperationRegistry` with JSON dispatch
- [x] `OperationContext` with engine, auth, trust level
- [ ] Serialize/deserialize for all standard operation params
- [ ] Error type parity with TS `OperationError` (codes, suggestions)

**Completed operations so far**:
- ✅ `get_page` (Slice 1-2-1)
- ✅ `think` (Slice 1-2-3 Issue #51)

**Next 5 high-priority operations**:
1. `query` - semantic search (core user interaction)
2. `search` - hybrid search + keyword expansion
3. `put_page` - page creation/update
4. `add_source` - source management
5. `list_pages` - browsing interface

### Slice 1-4-2: Port local and remote trust boundary enforcement

Implement the thin client routing and `localOnly` checks in Rust:

1. **Thin client detection**
   - Load config and check `remote_mcp` settings
   - Match TS behavior exactly: `isThinClient(config)`
   - Refuse `localOnly` ops with hint message
   - Route through `callRemoteTool` equivalent

2. **Trust boundary enforcement**
   - Every operation declares `localOnly: bool`
   - Registry checks before dispatch
   - Clear error messaging matching TS

3. **Auth context propagation**
   - API keys and provider credentials
   - Remote MCP signature headers

### Slice 1-4-3: Move shared CLI MCP dispatch to Rust operations

1. **NAPI bridge for remaining operations**
   - Rust CLI calls into TS for unported operations
   - Gradual transition without breaking workflows
   - OR: Just use `zbrain-ts` fallback for unported

2. **MCP tool definition generation**
   - Rust ops expose JSON schema for params
   - Auto-generate MCP tool definitions
   - Match TS `--tools-json` output exactly

3. **Remote call layer**
   - Rust implementation of `callRemoteTool`
   - Error handling and retries matching TS
   - Signature verification for remote MCP

---

## Acceptance Criteria

1. ✅ Operation registry supports JSON dispatch with schema validation
2. ✅ Top 10 operations implemented in Rust (query, search, put_page, etc.)
3. ✅ Thin client routing works identically in Rust and TS
4. ✅ `localOnly` operations are refused on thin clients with matching error messages
5. ✅ `zbrain --tools-json` produces same output in Rust and TS
6. ✅ NAPI fallback layer works for unported operations

---

## Next Node

**1-5**: MCP server migration (full transport layer, rate limiting, audit hooks)
