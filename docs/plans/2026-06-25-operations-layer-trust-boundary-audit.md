# 1-4: Operations Layer and Trust Boundary Migration - Audit & Implementation Plan

Date: 2026-06-25
Parent roadmap node: 1-4 Operations layer and trust boundary migration

## Scope

This is the largest TypeScript → Rust migration epic by code volume. It moves the entire operation definition system (schemas, handlers, context, and the complete trust boundary) from TypeScript to Rust. This is the critical security boundary between local CLI and remote MCP callers.

**In scope (all 3 child nodes):**
1. **1-4-1**: Port operation definitions, schemas, and context types to Rust
2. **1-4-2**: Port local and remote trust boundary enforcement to Rust
3. **1-4-3**: Port operation execution and error handling infrastructure

**Out of scope (deferred to later nodes):**
- MCP server implementation (moves to 1-5)
- Web backend API (moves to 1-6)
- Search/ingestion operations (moves to 1-7)
- Facts/timeline/knowledge graph operations (moves to 1-8)
- AI gateway providers and routing (moves to 1-9)
- Jobs/agents/autopilot execution (moves to 1-10)

---

## Grill Decisions

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| Q1 | Overall strategy | A (audit first, then implement) | Proven successful pattern from 1-2 / 1-3. Full visibility before writing code minimizes rework, especially critical for security-sensitive trust boundary code. |
| Q2 | Audit depth | A (full coverage audit) | Single document covering all 3 nodes. Trust boundary code requires extra diligence; full upfront planning prevents security regressions. |
| Q3 | Slicing in audit | A (audit + full TDD slicing) | Complete acceptance criteria and slicing plan included in this document. Audit → implementation is a single handoff; no intermediate grill session needed. |
| Q4 | Rust Operation architecture | A (trait-based Operation system) | Best fits Rust idioms: type safety, zero-cost abstraction, each operation can have its own state and type parameters. |
| Q5 | Trust boundary strategy | A (full 1:1 parity) | Security critical path — behavior must exactly match TS implementation: `localOnly` checks, subagent path prefix restrictions, `image_path` remote caller ban (D18). |

---

## Current State Audit

### TypeScript Operations System (`src/core/operations.ts`)

**File size:** 4,243 lines (largest single file in the codebase)
**Key components:**

1. **Types infrastructure**
   - `ErrorCode` open union (10+ defined codes + extensibility)
   - `OperationError` class with structured error output
   - `UploadValidator` for path security (strict vs loose modes)
   - `OperationContext` type (jobId, subagentId, clientId, permissions)

2. **Operation definition structure**
   ```typescript
   interface Operation<Params, Result> {
     name: string;
     description: string;
     cliHints?: { name: string, positional?: string[], stdin?: string };
     params: ZodSchema<Params>;
     localOnly?: boolean;
     handler: (ctx: OperationContext, params: Params, engine: BrainEngine) => Promise<Result>;
   }
   ```

3. **Approximately 50+ operations defined**
   - Core CRUD: get_page, put_page, delete_page, restore_page, list_pages
   - Search: search, query, takes_search
   - Takes system: takes_list, takes_scorecard, takes_calibration
   - Tagging: add_tag, remove_tag, get_tags
   - Link graph: add_link, remove_link, get_links, get_backlinks, traverse_graph
   - Timeline: add_timeline_entry, get_timeline
   - Health: get_stats, get_health, get_brain_identity, run_doctor, get_versions
   - Versioning: revert_version
   - Sync: sync_brain
   - Raw data: put_raw_data, get_raw_data, resolve_slugs, get_chunks
   - Ingestion: log_ingest, get_ingest_log
   - Files: file_list, file_upload, file_url
   - Jobs: submit_job, submit_agent, get_job, list_jobs, cancel_job, retry_job, get_job_progress, pause_job, resume_job, replay_job, send_job_message
   - Quality: find_orphans, get_calibration_profile, get_recent_salience, find_anomalies, find_experts, find_contradictions, find_trajectory
   - Transcripts: get_recent_transcripts
   - Auth: whoami
   - Sources: sources_add, sources_list, sources_remove, sources_status
   - Facts: extract_facts, recall, forget_fact
   - Think: think

4. **Trust boundary enforcement points**
   - `localOnly: boolean` flag - if true, MCP dispatch layer blocks the operation
   - Subagent permission checks (put_page prefix restriction)
   - `image_path` D18 security constraint (remote callers cannot pass local file paths)
   - Upload validator path security (symlink + path traversal protection)
   - Thin client routing with specific refuse hints per command

5. **Error handling pattern**
   - Structured errors with `code`, `message`, `suggestion`, `docs`
   - `toJSON()` serialization for MCP transport
   - Open union ErrorCode pattern for forward compatibility

---

### TypeScript Trust Boundary Implementation

**Key security constraints (audit-verified):**

| Constraint | Location | Purpose |
|------------|----------|---------|
| `localOnly: true` flag | per-operation | Marks operations that should never be exposed over MCP |
| Subagent `put_page` prefix check | line ~611-619 | Subagents can only write under their assigned prefix path |
| `image_path` remote caller ban | line ~3708 | D18 security constraint: remote MCP callers cannot pass local file paths |
| Upload path validator | line ~94-173 | Two modes: strict (remote=true) for untrusted callers, loose for local CLI |
| Thin client refuse hints | src/cli.ts ~752-792 | Per-command actionable error messages when blocked |

**Operations marked `localOnly: true`:**
```
purge_deleted_pages, think, list_pages, put_raw_data, sync_brain,
file_upload, file_list, file_url, get_chunks, resolve_slugs,
get_raw_data, whoami, sources_*
```

---

### Rust Current State

**Existing infrastructure in `crates/zbrain-core/src/`:**

✅ **BrainEngine trait already defined** - all DB-backed methods exist for both backends
✅ **Migration system complete** - schema and migration registry working
✅ **Error type system in place** - `Result<T>` with structured errors
✅ **Config system implemented** - `crates/zbrain-cli/src/config.rs` complete
✅ **CLI framework ready** - clap derive with subcommands

**Missing in Rust:**
- Operation trait and type definitions
- Params schema validation (zod equivalent in Rust: validator or serde + custom checks)
- Operation context type with jobId/subagentId/clientId
- Trust boundary enforcement logic (localOnly checks, subagent permission)
- Operation registry and dispatch system
- Structured error serialization matching TS format
- Upload validator path security implementation

---

## Gap Analysis

| Area | TS Status | Rust Status | Gap Size |
|------|-----------|-------------|----------|
| Operation trait/type system | ✅ Complete | ❌ Missing | Medium |
| Params schema validation | ✅ Complete (zod) | ❌ Missing | Medium (use validator crate) |
| Operation context type | ✅ Complete | ❌ Missing | Small |
| Trust boundary enforcement | ✅ Complete (security critical) | ❌ Missing | Large (requires security audit) |
| Operation registry/dispatch | ✅ Complete | ❌ Missing | Medium |
| Structured error handling | ✅ Complete | ⚠️ Partial (needs serialization) | Small |
| Upload validator security | ✅ Complete | ❌ Missing | Medium |
| 50+ operation implementations | ✅ Complete | ❌ Not started | Very Large |
| CLI command dispatch bridge | ✅ Complete | ⚠️ Partial (stubs exist) | Medium |

---

## Security Critical Path Identification

This node contains the **primary security boundary** of the entire system. **Every line must be audited.**

### Highest risk areas:
1. **`localOnly` enforcement** - bug = remote code execution vector
2. **Subagent path prefix check** - bug = privilege escalation
3. **`image_path` D18 constraint** - bug = arbitrary file read
4. **Upload path validator** - bug = path traversal / file overwrite
5. **Thin client refuse logic** - bug = local-only operations exposed remotely

### Security verification strategy:
- TDD approach: write failing security tests **before** implementation
- Exact 1:1 behavior parity with TS tests
- Fuzz testing for path validation logic
- Property-based tests for `localOnly` flag enforcement

---

## TDD Implementation Slicing Plan

**Overall order:** Type foundation → Trust boundary infrastructure → Core operation implementations → Dispatch integration

---

### Issue #40: 1-4-1-1 Rust Operation trait and type foundation

**Scope:**
- Define `Operation` trait in `crates/zbrain-core/src/operation.rs`
- Define `OperationContext` struct with jobId, subagentId, clientId, permissions
- Define `ErrorCode` enum (open union pattern, match TS exactly)
- Define `OperationError` struct with `code`, `message`, `suggestion`, `docs` fields
- `to_json()` serialization matching TS format exactly

**Acceptance Criteria:**
- [ ] `cargo build -p zbrain-core` succeeds
- [ ] `Operation` trait has: `name()`, `description()`, `params_schema()`, `local_only()`, `handler()`
- [ ] `ErrorCode` variants match TS definitions 1:1
- [ ] `OperationError` JSON output byte-for-byte matches TS `toJSON()`
- [ ] Unit tests verify error serialization parity

**Dependencies:** None (foundational slice)
**Estimated size:** ~200-300 lines

---

### Issue #41: 1-4-1-2 Params schema validation system

**Scope:**
- Add `validator` crate dependency (or equivalent schemars + custom validation)
- Define `Params` derive macro or trait-based validation system
- Implement validation error format matching TS zod errors
- Common validators: required fields, string patterns, number ranges, nested object validation

**Acceptance Criteria:**
- [ ] Validation errors match TS zod error structure
- [ ] All common validation patterns covered (required, string, number, nested objects)
- [ ] Invalid params return `invalid_params` error code with structured details
- [ ] Unit tests cover validation success and failure cases

**Dependencies:** #40
**Estimated size:** ~150-250 lines

---

### Issue #42: 1-4-2-1 Trust boundary enforcement core logic

**Scope:**
- Implement `localOnly` flag check in dispatch
- Implement subagent path prefix validation for put_page
- Implement `image_path` D18 constraint for remote callers
- Upload path validator (strict vs loose modes, symlink protection)
- Comprehensive security test suite

**Acceptance Criteria:**
- [ ] `localOnly = true` operations blocked when context is remote
- [ ] Subagent put_page correctly enforces prefix constraint
- [ ] `image_path` rejected when caller is not local CLI
- [ ] Upload validator blocks path traversal attempts (`../`)
- [ ] Upload validator blocks symlinks in strict mode
- [ ] Fuzz tests pass for path validation
- [ ] Security tests cover ALL edge cases from TS implementation

**Dependencies:** #40, #41
**Estimated size:** ~300-400 lines (security critical, requires extensive testing)

---

### Issue #43: 1-4-1-3 Operation registry and dispatch system

**Scope:**
- Implement `OperationRegistry` struct to store all operations
- `register(operation)` method for registration
- `lookup(name)` method for retrieving operation by name
- `dispatch(name, params, context, engine)` method that validates, checks permissions, and executes
- Macro for easy operation registration (similar to `operations` array in TS)

**Acceptance Criteria:**
- [ ] Operations can be registered and looked up by name
- [ ] Dispatch correctly validates params before calling handler
- [ ] Dispatch correctly enforces trust boundary checks (#42)
- [ ] Errors propagate correctly through the dispatch layer
- [ ] `operations: &[&dyn Operation]` array pattern works like TS
- [ ] Zero-cost abstraction verified (no runtime overhead vs direct calls)

**Dependencies:** #40, #41, #42
**Estimated size:** ~200-300 lines

---

### Issue #44: 1-4-1-4 First operation port (get_page) - end-to-end verification

**Scope:**
- Port `get_page` operation from TS to Rust as a reference implementation
- Demonstrate the complete pipeline: schema → validation → handler → error → serialization
- Verify 1:1 behavior parity with TS implementation
- Create template pattern for porting remaining operations

**Acceptance Criteria:**
- [ ] `get_page` Rust implementation passes all existing TS unit tests
- [ ] Behavior verified 1:1 against TS including error cases
- [ ] Template documentation written for porting other operations
- [ ] Performance benchmark shows no regression vs TS

**Dependencies:** #40, #41, #42, #43
**Estimated size:** ~100-150 lines (operation) + test verification

---

### Issue #45: 1-4-1-5 Batch 1: Core CRUD operations

**Scope:**
Port these 8 core operations:
1. `put_page` - includes subagent permission check
2. `delete_page`
3. `restore_page`
4. `list_pages`
5. `add_tag`
6. `remove_tag`
7. `get_tags`
8. `get_versions`

**Acceptance Criteria:**
- [ ] All 8 operations ported
- [ ] Each operation has unit tests matching TS test coverage
- [ ] Subagent path check verified for `put_page`
- [ ] All `localOnly` flags correctly set
- [ ] Behavior parity confirmed against TS

**Dependencies:** #44 (template established)
**Estimated size:** ~400-600 lines total

---

### Issue #46: 1-4-1-6 Batch 2: Link graph and timeline operations

**Scope:**
Port these 7 graph operations:
1. `add_link`
2. `remove_link`
3. `get_links`
4. `get_backlinks`
5. `traverse_graph`
6. `add_timeline_entry`
7. `get_timeline`

**Acceptance Criteria:**
- [ ] All 7 operations ported with tests
- [ ] Graph traversal logic matches TS exactly
- [ ] Timeline entry formatting matches
- [ ] Correct `localOnly` flags set

**Dependencies:** #45
**Estimated size:** ~350-500 lines total

---

### Issue #47: 1-4-1-7 Batch 3: Takes and quality operations

**Scope:**
Port these 8 takes/quality operations:
1. `takes_list`
2. `takes_search`
3. `takes_scorecard`
4. `takes_calibration`
5. `find_orphans`
6. `get_calibration_profile`
7. `get_recent_salience`
8. `find_anomalies`

**Acceptance Criteria:**
- [ ] All 8 operations ported with tests
- [ ] Takes fence stripping logic matches TS
- [ ] Anomaly detection algorithm behavior verified
- [ ] Correct `localOnly` flags set (most quality operations are local-only)

**Dependencies:** #46
**Estimated size:** ~400-550 lines total

---

### Issue #48: 1-4-1-8 Batch 4: Health, stats, and metadata operations

**Scope:**
Port these 8 operations:
1. `get_stats`
2. `get_health`
3. `get_brain_identity`
4. `run_doctor`
5. `revert_version`
6. `sync_brain`
7. `whoami`
8. `think`

**Acceptance Criteria:**
- [ ] All 8 operations ported with tests
- [ ] Health check output format matches
- [ ] `think` operation with correct output streaming
- [ ] `localOnly` flags correctly set for think/sync_brain/whoami

**Dependencies:** #47
**Estimated size:** ~400-500 lines total

---

### Issue #49: 1-4-1-9 Batch 5: Files, chunks, raw data operations

**Scope:**
Port these 7 file/data operations:
1. `file_list`
2. `file_upload`
3. `file_url`
4. `put_raw_data`
5. `get_raw_data`
6. `resolve_slugs`
7. `get_chunks`

**Acceptance Criteria:**
- [ ] All 7 operations ported with tests
- [ ] File upload validator from #42 integrated correctly
- [ ] `image_path` D18 constraint enforced for remote callers
- [ ] Correct `localOnly` flags set for all file operations

**Dependencies:** #48
**Estimated size:** ~350-450 lines total

---

### Issue #50: 1-4-1-10 Batch 6: Sources and ingestion operations

**Scope:**
Port these 5 sources/ingestion operations:
1. `sources_add`
2. `sources_list`
3. `sources_remove`
4. `sources_status`
5. `log_ingest`
6. `get_ingest_log`

**Acceptance Criteria:**
- [ ] All 6 operations ported with tests
- [ ] Correct `localOnly` flags set for all sources operations
- [ ] Ingest log formatting matches TS

**Dependencies:** #49
**Estimated size:** ~250-350 lines total

---

### Issue #51: 1-4-1-11 Batch 7: Facts and recall operations

**Scope:**
Port these 3 fact system operations:
1. `extract_facts`
2. `recall`
3. `forget_fact`

**Acceptance Criteria:**
- [ ] All 3 operations ported with tests
- [ ] Fact extraction output format matches
- [ ] Recall result formatting matches TS
- [ ] Error codes match for fact not found

**Dependencies:** #50
**Estimated size:** ~150-250 lines total

---

### Issue #52: 1-4-1-12 Batch 8: Advanced expert/trajectory operations

**Scope:**
Port these 4 advanced operations:
1. `find_experts`
2. `find_contradictions`
3. `find_trajectory`
4. `get_recent_transcripts`

**Acceptance Criteria:**
- [ ] All 4 operations ported with tests
- [ ] Expert scoring algorithm matches
- [ ] Contradiction detection logic verified
- [ ] Trajectory graph output matches TS format

**Dependencies:** #51
**Estimated size:** ~300-400 lines total

---

### Issue #53: 1-4-2-1 MCP server trust boundary integration

**Scope:**
- Integrate Rust operation registry with MCP server
- `localOnly` operations filtered out of MCP tool list
- Remote context correctly set for MCP-invoked operations
- Thin client refuse hints implemented in Rust
- Integration tests verify security boundary

**Acceptance Criteria:**
- [ ] `localOnly = true` operations NOT exposed in MCP tools list
- [ ] Remote context flag correctly set for MCP-invoked operations
- [ ] Subagent ID and permissions correctly passed through context
- [ ] Thin client refuse hints match TS wording and behavior exactly
- [ ] Integration test verifies remote caller cannot call local-only ops

**Dependencies:** #43 (operation registry), #42 (trust boundary core)
**Estimated size:** ~200-300 lines

---

### Issue #54: 1-4-3-1 CLI command dispatch bridge to Rust operations

**Scope:**
- Integrate Rust operation registry into `zbrain-cli`
- CLI commands route through Rust operation dispatch system
- CLI arguments map to operation params correctly
- Error output formatting matches TS CLI behavior
- Help text generation from operation descriptions

**Acceptance Criteria:**
- [ ] CLI commands route through Rust operation dispatch
- [ ] Positional and flag arguments map correctly to params
- [ ] Error output formatting and exit codes match TS CLI
- [ ] `--help` output generated from operation metadata
- [ ] Integration tests verify CLI → operation flow end-to-end

**Dependencies:** #43, #44 (first operation ported)
**Estimated size:** ~200-300 lines

---

### Issue #55: 1-4-3-2 Error handling and reporting parity

**Scope:**
- Ensure all error codes map 1:1 between TS and Rust
- Error message wording parity
- Suggestion field support for actionable errors
- Docs field support for error documentation links
- Stack trace capture and formatting (for debug mode)

**Acceptance Criteria:**
- [ ] All ErrorCode variants exist in Rust with same string representation
- [ ] Error message wording matches in all common failure paths
- [ ] `suggestion` field rendered correctly in CLI output
- [ ] `docs` field included when present
- [ ] Stack traces match format and verbosity level

**Dependencies:** #40 (error types)
**Estimated size:** ~100-200 lines

---

## Summary: All Implementation Slices

| Issue # | Slice | Estimated Lines | Dependencies |
|---------|-------|-----------------|--------------|
| #40 | Operation trait + type foundation | 200-300 | None |
| #41 | Params schema validation system | 150-250 | #40 |
| #42 | Trust boundary enforcement core | 300-400 | #40, #41 |
| #43 | Operation registry + dispatch | 200-300 | #40, #41, #42 |
| #44 | First operation port (get_page) | 100-150 | #40-#43 |
| #45 | Batch 1: Core CRUD (8 ops) | 400-600 | #44 |
| #46 | Batch 2: Link graph + timeline (7 ops) | 350-500 | #45 |
| #47 | Batch 3: Takes + quality (8 ops) | 400-550 | #46 |
| #48 | Batch 4: Health + metadata (8 ops) | 400-500 | #47 |
| #49 | Batch 5: Files + raw data (7 ops) | 350-450 | #48 |
| #50 | Batch 6: Sources + ingestion (6 ops) | 250-350 | #49 |
| #51 | Batch 7: Facts + recall (3 ops) | 150-250 | #50 |
| #52 | Batch 8: Advanced expert/trajectory (4 ops) | 300-400 | #51 |
| #53 | MCP server trust boundary integration | 200-300 | #42, #43 |
| #54 | CLI command dispatch bridge | 200-300 | #43, #44 |
| #55 | Error handling and reporting parity | 100-200 | #40 |
|---------|-------|-----------------|--------------|
| **Total** | **16 slices** | **~3,750 - 5,300 lines** | |

---

## Gap Summary Table (Consolidated)

| Component | Status in TS | Status in Rust | Priority | Risk |
|-----------|-------------|----------------|----------|------|
| Operation trait system | ✅ Complete | ❌ Missing | P0 | High (security boundary) |
| Params validation (zod) | ✅ Complete | ❌ Missing | P0 | Medium |
| Operation context | ✅ Complete | ❌ Missing | P0 | High (permissions) |
| Trust boundary enforcement | ✅ Complete | ❌ Missing | **P0 (CRITICAL)** | **Critical** |
| Operation registry/dispatch | ✅ Complete | ❌ Missing | P0 | High |
| Structured error handling | ✅ Complete | ⚠️ Partial | P1 | Medium |
| Upload path validator | ✅ Complete | ❌ Missing | **P0 (CRITICAL)** | **Critical** |
| 50+ operation implementations | ✅ Complete | ❌ Not started | P0-P2 | Medium-High |
| MCP server integration | ✅ Complete | ❌ Missing | P0 | High |
| CLI command bridge | ⚠️ Partial (stubs) | In progress | P1 | Low |

---

## Acceptance Gates (Node 1-4 Complete When...)

Node 1-4 is **fully complete** when:

### Gate 1: All operations ported
- [ ] All 52+ operations ported to Rust
- [ ] Each operation has unit tests matching TS coverage
- [ ] Behavior parity verified for every operation

### Gate 2: Trust boundary verified
- [ ] All security constraints from #42 have tests
- [ ] `localOnly` enforcement has 100% test coverage
- [ ] Subagent path check verified for all edge cases
- [ ] `image_path` D18 constraint has fuzz tests
- [ ] Upload validator has path traversal + symlink tests

### Gate 3: Integration complete
- [ ] MCP server correctly filters local-only operations
- [ ] CLI routes all commands through Rust dispatch
- [ ] Error codes, messages, and formatting 100% match

### Gate 4: TypeScript code deleted
- [ ] `src/core/operations.ts` removed from TypeScript codebase
- [ ] All operation imports updated to use Rust FFI or bridge
- [ ] No dead references to old operation types remain
- [ ] `tsc --noEmit` passes with 0 errors

---

## Key Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Security regression in trust boundary | Medium | Critical | TDD with failing security tests FIRST; exhaustive test coverage; 1:1 behavior parity requirement |
| Subtle behavior differences in complex ops | High | Medium | Property-based testing; golden file output comparisons; side-by-side runtime comparisons |
| Large slice scope causing delays | Medium | Medium | Batched approach (8 batches) keeps each slice manageable; parallelizable |
| Path validation platform differences (Windows vs Unix) | Medium | Medium | Cross-platform CI testing; explicit test cases for Windows path semantics |
| Error message wording drift | Low | Low | Golden file test for error output; automated string comparison |

---

## Non-Goals / Explicitly Out of Scope

1. **New operation features** - No new functionality during migration. Exact 1:1 parity only.
2. **Performance optimizations** - Optimizations deferred to post-migration cleanup passes.
3. **New trust boundary constraints** - Exact parity with existing TS implementation only.
4. **MCP server implementation** - Deferred to Node 1-5 (this node only provides the Rust-side enforcement hooks).
5. **Web backend API** - Deferred to Node 1-6.
