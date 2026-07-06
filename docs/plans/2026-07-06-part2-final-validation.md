# Part2 Final Validation — Config/Bootstrap/Package Entrypoint strict parity

Roadmap node: `1-7` (Part2 slice deliverable validation)
Date: 2026-07-06
Branch: `rust-rewrite`

## Scope (Q1)

This is **slice deliverable validation for Part2 only** — it verifies the six
knives `1-1`~`1-6` landed and did not regress each other. It is **NOT** a
full-migration end-to-end acceptance: "complete TS→Rust migration" spans
Part1/2/3 by design (root charter states all other unfinished TS→Rust work is
deferred to Part3). Behaviors that depend on **unported Part3 subsystems**
(progress reporter, MCP timeout, search attribution/rerank, release chain) are
explicitly **out of scope** here — see the hand-off boundary below.

## Verification material (Q2) — actual commands + results

### 1. Rust unit/integration suite (parser parity + behavior for 1-1~1-5)

```
$ cargo test -p zbrain-cli --lib
test result: ok. 101 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 30.03s
```

Covers init parity (engine-selection / migrate-only / mcp-only / embedding /
existing-config / JSON), config parity (set schema gating + `--force`; get
exit-code + no-redaction; show redaction), doctor parity (`--json` envelope +
`UNMIGRATED_TS_DOCTOR_CHECKS` anchor + health-score/status pure fns),
schema-sql rename (`UNMIGRATED_TS_SCHEMA_PACK_VERBS` anchor), plus sources /
capture / sync command parsing. **PASS.**

### 2. bin wrapper transparent pass-through (1-5)

```
$ node --test bin/zbrain-rs.test.mjs
# tests 4 / # pass 4 / # fail 0
```

status-0 pass-through / non-zero status pass-through / signal-killed (status
null) → non-zero (not 0) / spawn-error → non-zero. **PASS.**

### 3. Cleanup guard (1-6)

```
$ bash scripts/check-no-legacy-getconnection.sh
check-no-legacy-getconnection: ok (no new singleton callers)
```

Ran green after the 3 dead allowlist entries (init.ts / doctor.ts /
serve-http.ts) were removed. **PASS.**

### 4. Top-level binary smoke (assembly-level regression guard, 1-5 lesson)

Built `zbrain` debug binary
(`target/x86_64-pc-windows-msvc/debug/zbrain.exe`), ran no-side-effect commands
only (no DB touched):

| Invocation | Expected | Result |
|---|---|---|
| `zbrain --version` | exit 0 | `zbrain 0.0.1`, rc=0 |
| `zbrain --help` | exit 0 | rc=0 |
| `zbrain init --help` | exit 0 | rc=0 |
| `zbrain config --help` | exit 0 | rc=0 |
| `zbrain doctor --help` | exit 0 | rc=0 |
| `zbrain schema-sql --help` | exit 0 | rc=0 |
| `zbrain definitely-not-a-cmd` | clap exit 2 | rc=2 |
| `zbrain schema` (bare; renamed in 1-4) | clap exit 2 | rc=2 |

clap top-level assembly intact; the 1-4 rename (`schema`→`schema-sql`) holds at
the binary level. **PASS.**

## Deliverable cross-check (1-1 ~ 1-6)

| Node | Deliverable | Verified by | Status |
|---|---|---|---|
| 1-1 | init strict TS flag parity | cargo suite (init_* cases) + `init --help` smoke | [x] |
| 1-2 | config strict TS flag parity | cargo suite (config_* cases) + `config --help` smoke | [x] |
| 1-3 | doctor strict TS flag parity (`--json`, drop dead `--offline`) | cargo suite (doctor_* + `UNMIGRATED_TS_DOCTOR_CHECKS`) + `doctor --help` smoke | [x] |
| 1-4 | schema DDL dumper → `schema-sql` + unmigrated schema-pack trace | cargo suite (`UNMIGRATED_TS_SCHEMA_PACK_VERBS`) + bare `schema` rc=2 smoke | [x] |
| 1-5 | bin wrapper transparent pass-through (argv + exit-code/signal) | `node --test` wrapper 4/4 + `.gitignore` entry-in-VCS fix | [x] |
| 1-6 | migration cleanup for TS remnants + doc links | guard script green + dead `build` script removed + stale build-command docs fixed | [x] |

All six green. Part2 slice is internally consistent — no cross-knife regression.

## Part3 hand-off boundary (deliberately NOT validated here)

Per Q1/Q4, the following are **expected gaps**, owned by
`.workbuddy/roadmaps/zbrain-ts-to-rust-part3-release-and-ts-retirement.json`.
They are documented, not fixed, and do **not** block Part2 closure:

| Deferred item | Why out of scope | Part3 node |
|---|---|---|
| Global flags `--quiet` / `--progress-json` / `--progress-interval` behavior | No Rust consumer; depends on unported progress reporter (`FUTURE(progress-reporter)` in operation.rs) | 1-2 |
| `--timeout` actually applied to MCP client | Routing skeleton only; `Client::new()` has no timeout (`FUTURE(mcp-timeout)`) | 1-2 |
| `--explain` search attribution / rerank | Rust query scoring is hardcoded keyword weighting; no rerank/attribution (`FUTURE(search-attribution)`) | 1-2 |
| Release chain (`build:all` / `prepublish:clawhub` cross-compile mac/linux + openclaw manifest serve/serve-mcp semantics + binary naming) | Local verification blind spot (can't build/verify mac+linux artifacts here) | 1-1 |
| `--version` shows `0.0.1` (Cargo.toml) vs `0.41.14.0` (package.json) | Version-sync / release concern (CLAUDE.md 5-file sync mechanism), not a TS command-parity gap | 1-1 / release |
| TS entrypoint retirement (`src/cli.ts`, postinstall TS fallback, `check-cli-executable.sh`, `src/commands/*`) | `src/cli.ts` is still the live entry for many unported features; hard-depends on release chain cutting to Rust first | 1-3 |

## Verdict

**PASS.** All three material classes green (cargo 101/101, wrapper 4/4, guard
ok) + binary smoke green + all six deliverables cross-checked + Part3 hand-off
boundary documented. Part2 (Config/Bootstrap/Package Entrypoint strict parity)
is closed. No in-scope regressions found; no in-scope fixes required.
