# 1-3: Config bootstrap and package entrypoint cutover - Audit & Implementation Plan

Date: 2026-06-25
Parent roadmap node: 1-3 Config bootstrap and package entrypoint cutover

## Scope

This is the first migration epic with direct user-visible impact. It moves the config discovery/loading system, CLI commands (init/doctor/config/schema), and the npm package bin entrypoint from TypeScript to Rust.

**In scope (all 4 child nodes):**
1. **1-3-1**: Port config discovery, loading, and writing to Rust
2. **1-3-2**: Port init, doctor, config storage, and schema commands to Rust
3. **1-3-3**: Cut package bin and install flow to the Rust binary
4. **1-3-4**: Delete replaced TypeScript bootstrap command surface

**Out of scope:**
- Agent/operation execution runtime (moves to 1-4)
- MCP server (moves to 1-5)
- Web backend (moves to 1-6)
- Search/ingestion (moves to 1-7)
- Facts/timeline/knowledge graph (moves to 1-8)

---

## Grill Decisions

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| Q1 | Overall strategy | A (audit first, then implement) | Proven successful pattern from 1-2-1 / 1-2-2 / 1-2-3. Full visibility before writing code minimizes rework. |
| Q2 | Audit depth | A (full coverage audit) | Single document covering all 4 nodes. User-visible changes require extra diligence; full upfront planning reduces breakage risk. |
| Q3 | Slicing in audit | A (audit + full TDD slicing) | Complete acceptance criteria and slicing plan included in this document. Audit → implementation is a single handoff; no intermediate grill session needed. |

---

## Current State Audit

### TypeScript Config System (`src/core/config.ts`)

**File size:** ~800 lines
**Key functions:**
- `loadConfig()` - Discover and load zbrain.yml from current dir or ~/.zbrain/
- `loadConfigWithEngine()` - Load config using engine's config storage backend
- `writeConfig()` - Write config back to disk
- `validateConfig()` - Schema validation for config keys

**Config discovery hierarchy:**
1. Current working directory: `./zbrain.yml`
2. User home directory: `~/.zbrain/config`
3. Environment variable overrides: `ZBRAIN_*`
4. CLI flag overrides: `--config`

**Config storage backends:**
- File-based (YAML on disk) - used by CLI
- DB-based (libsql/postgres `zbrain_config` table) - used by BrainEngine

---

### TypeScript CLI Commands

**Entry point:** `src/cli.ts` (~3500 lines)
**Command structure:**

| Command | File | Lines | Description |
|---------|------|-------|-------------|
| `init` | `src/commands/init.ts` | ~200 | Initialize new zbrain project |
| `doctor` | `src/commands/doctor.ts` | ~300 | Validate installation and connectivity |
| `config` | `src/commands/config.ts` | ~200 | Show/get/set/unset config keys |
| `schema` | `src/commands/schema.ts` | ~150 | Print database schema SQL |
| + 15 more | various | ~2500 | agents, run, import, search, ingest, etc. |

**Critical observation:** Only `init`/`doctor`/`config`/`schema` are in scope for 1-3. All other commands are deferred to later roadmap nodes.

---

### Package Bin Entrypoint (`package.json`)

```json
{
  "bin": {
    "zbrain": "./dist/cli.js"
  }
}
```

**Install flow:**
1. `npm install -g zbrain` or `bun add -g zbrain`
2. bin link created pointing to `dist/cli.js`
3. `zbrain` command in shell executes TypeScript CLI via Node/bun

---

### Rust Current State (`crates/zbrain-core`)

**BrainEngine Config methods already exist:**
```rust
async fn get_config(&self, key: &str) -> Result<Option<String>>;
async fn set_config(&self, key: &str, value: &str) -> Result<()>;
async fn list_config_keys(&self, prefix: Option<&str>) -> Result<Vec<String>>;
async fn unset_config(&self, key: &str) -> Result<u64>;
```

✅ **Good news:** The DB-backed config storage methods are already implemented in Rust for both libsql and postgres backends!

**Missing in Rust:**
- File-based YAML config discovery and parsing
- CLI argument parsing (clap not set up yet)
- Rust binary crate entrypoint (`main.rs`)
- `init` command logic (template creation, directory setup)
- `doctor` command logic (connectivity checks, validation)
- `schema` command logic (print SQL schema)

---

## Gap Analysis

| Area | TS Status | Rust Status | Gap Size |
|------|-----------|-------------|----------|
| Config file discovery | ✅ Complete | ❌ Missing | Medium |
| YAML parsing/serializing | ✅ Complete | ❌ Missing | Small (use serde_yaml) |
| DB config storage | ✅ Complete | ✅ Complete | None! |
| CLI arg parsing | ✅ Complete (manual) | ❌ Missing | Medium (add clap) |
| `init` command | ✅ Complete | ❌ Missing | Medium |
| `doctor` command | ✅ Complete | ❌ Missing | Medium |
| `config` command | ✅ Complete | ⚠️ Partial (backend ready, CLI missing) | Small |
| `schema` command | ✅ Complete | ❌ Missing | Very Small |
| Package bin entry | TS only | ❌ No Rust bin yet | Medium |
| Install flow | npm/bun only | ❌ No cargo install or binary distribution | Large |

---

## TDD Implementation Slicing Plan

**Overall order:** Compile-only foundation → Config file system → CLI framework → Commands → Bin cutover → Delete TS

---

### Issue #32: 1-3-1-1 Rust binary crate setup + clap CLI framework

**Scope:**
- Create `crates/zbrain-cli/` binary crate (separate from zbrain-core lib)
- Add `clap` dependency with derive feature
- Basic CLI skeleton with global flags (`--config`, `--debug`)
- Empty command stubs: init, doctor, config, schema
- `main.rs` entrypoint with error handling and exit codes

**Acceptance Criteria:**
- [ ] `cargo build -p zbrain-cli` succeeds
- [ ] `cargo run -p zbrain-cli -- --help` prints usage
- [ ] `cargo run -p zbrain-cli -- --version` prints version
- [ ] All 4 command stubs compile without `unimplemented!()` panics
- [ ] `main.rs` uses `#[tokio::main]` for async runtime

**Dependencies:** None (first slice)

---

### Issue #33: 1-3-1-2 Config file discovery + YAML parsing

**Scope:**
- Config discovery logic: cwd → ~/.zbrain/config → env vars → CLI override
- YAML parsing with `serde` + `serde_yaml`
- Config struct definition matching TS schema
- `load_config_file()` / `write_config_file()` functions
- Redaction for sensitive keys (passwords, tokens, API keys)

**Acceptance Criteria:**
- [ ] `cargo test -p zbrain-cli config` passes
- [ ] Discovery finds config in current working directory
- [ ] Discovery falls back to `~/.zbrain/config` when cwd has none
- [ ] YAML round-trip: load → modify → write produces valid YAML
- [ ] Sensitive keys (key, secret, token, password, pwd, passwd, auth) are redacted on display
- [ ] `ZBRAIN_*` environment variable overrides work correctly

**Dependencies:** #32

---

### Issue #34: 1-3-2-1 `zbrain config` command implementation

**Scope:**
- `zbrain config show` - display all config values (redacted)
- `zbrain config get <key>` - get single config value
- `zbrain config set <key> <value>` - set config value
- `zbrain config unset <key>` - unset single key
- `zbrain config unset --pattern <prefix>` - bulk unset by prefix

**Acceptance Criteria:**
- [ ] All 5 subcommands compile and run
- [ ] `show` redacts sensitive keys correctly
- [ ] `get/set/unset` work with file-based config
- [ ] `--pattern` bulk unset works with prefix matching
- [ ] Error messages match TS CLI behavior (same semantics)

**Dependencies:** #32, #33

---

### Issue #35: 1-3-2-2 `zbrain init` command implementation

**Scope:**
- Interactive project initialization
- Create `zbrain.yml` config file with defaults
- Create `~/.zbrain/` directory if missing
- Initialize embedded database (libsql)
- Run migrations (via `engine.init_schema()`)
- Success message with next steps

**Acceptance Criteria:**
- [ ] `zbrain init` creates valid config file
- [ ] `~/.zbrain/` directory created if missing
- [ ] Database initialized with migrations run
- [ ] Re-running `init` on existing project detects and handles gracefully
- [ ] `--force` flag to overwrite existing config

**Dependencies:** #32, #33

---

### Issue #36: 1-3-2-3 `zbrain doctor` command implementation

**Scope:**
- Config file validation (exists, valid YAML)
- Database connectivity check
- Migration status verification
- Network connectivity check (for providers)
- Summary report with pass/fail status

**Acceptance Criteria:**
- [ ] All 4 check categories implemented
- [ ] Exit code 0 if all pass, non-zero if any fail
- [ ] Clear actionable error messages for each failure
- [ ] Colorized output (ANSI colors) matching TS CLI

**Dependencies:** #32, #33

---

### Issue #37: 1-3-2-4 `zbrain schema` command implementation

**Scope:**
- Print libsql schema SQL
- Print postgres schema SQL
- `--backend` flag to select which schema to output
- Default: libsql

**Acceptance Criteria:**
- [ ] `zbrain schema` outputs libsql schema
- [ ] `zbrain schema --backend postgres` outputs postgres schema
- [ ] Output matches actual SQL used by migrations exactly

**Dependencies:** #32

---

### Issue #38: 1-3-3-1 Package.json bin entrypoint cutover

**Scope:**
- Update `package.json` `"bin"` to point to Rust binary
- Two-phase approach for safety:
  1. Add `zbrain-rs` bin pointing to Rust (for testing)
  2. Switch `zbrain` alias to Rust once verified
- `postinstall` script to prebuild Rust binary or download prebuilt
- Handle cross-platform (Windows/macOS/Linux)

**Acceptance Criteria:**
- [ ] `npm install -g` installs working `zbrain` command
- [ ] `zbrain --version` prints Rust binary version
- [ ] Works on Windows, macOS, and Linux
- [ ] Fallback to building from source if prebuilt not available
- [ ] `zbrain-rs` alias available for testing/rollback

**Dependencies:** #34, #35, #36, #37

---

### Issue #39: 1-3-4-1 Delete replaced TypeScript bootstrap code

**Scope:**
- Delete TS CLI command implementations: init.ts, doctor.ts, config.ts, schema.ts
- Delete unreachable config loading code paths in TS
- Remove dead exports
- Update remaining TS code to call Rust bridge where needed
- TypeScript type checks pass (`tsc --noEmit`)

**Acceptance Criteria:**
- [ ] 4 deleted command files confirmed
- [ ] `tsc --noEmit` passes 0 errors
- [ ] No dead imports referencing deleted files
- [ ] Git diff shows net lines deleted (no new TS code added)

**Dependencies:** #38 (bin cutover complete)

---

## Overall Acceptance Gates

Node **1-3 is complete** when:
1. ✅ All 8 child issues (#32-#39) are closed
2. ✅ `npm install -g zbrain` installs Rust binary by default
3. ✅ `zbrain init`, `zbrain doctor`, `zbrain config`, `zbrain schema` all work via Rust
4. ✅ All 4 TypeScript command files are deleted from the repo
5. ✅ `cargo build -p zbrain-cli` succeeds
6. ✅ `tsc --noEmit` succeeds

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Cross-platform build failures | Medium | High | CI matrix for Windows/macOS/Linux; prebuilt binaries |
| Config file format incompatibility | Low | Medium | Rust YAML parser must match TS parser behavior; add compatibility tests |
| npm postinstall failures | Medium | High | Graceful fallback to source build; clear error messages |
| User confusion during transition | Medium | Medium | Clear release notes; `zbrain-rs` alias for parallel testing |

---

## Related Nodes

- Parent: **1-3 Config bootstrap and package entrypoint cutover** (roadmap.json)
- Follow-up: 1-4 Operations layer and trust boundary migration
- Parallel eligible: None (this is a sequential dependency bottleneck)

---

## Implementation Checklist

Generated from slicing above. Use this for status tracking during development:

| # | Task | Status |
|---|------|--------|
| 32 | Rust binary crate setup + clap CLI framework | ⏳ Pending |
| 33 | Config file discovery + YAML parsing | ⏳ Pending |
| 34 | `zbrain config` command implementation | ⏳ Pending |
| 35 | `zbrain init` command implementation | ⏳ Pending |
| 36 | `zbrain doctor` command implementation | ⏳ Pending |
| 37 | `zbrain schema` command implementation | ⏳ Pending |
| 38 | Package.json bin entrypoint cutover | ⏳ Pending |
| 39 | Delete replaced TypeScript bootstrap code | ⏳ Pending |

**Estimated total LOC:** +1500 Rust, -850 TypeScript (net +650)
