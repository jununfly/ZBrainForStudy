# 1-5: bin wrapper transparent pass-through correctness

**Roadmap node**: `1-5` (part2 config-bootstrap) — renamed from
`package/bin entrypoint strict TS flag parity` to
`bin wrapper transparent pass-through correctness (argv + exit-code/signal)`.

## Why the node was reframed (Q1)

The original label implied the node wrapper (`bin/zbrain-rs.js`) should
replicate TS's global-flag parsing for parity. Investigation of the real
history showed otherwise:

- The TS entrypoint was `src/cli.ts`, run **directly by `bun`**. It parsed its
  own global flags (`parseGlobalFlags` in `src/core/cli-options.ts`:
  `--quiet` / `--progress-json` / `--progress-interval` / `--timeout` /
  `--explain`) and owned its own exit code — because it *was* the process.
- There was **no node wrapper doing flag parsing** in the TS era. `bin/zbrain-rs.js`
  is a post-cutover artifact (commit `38752ec`) whose sole job is to locate and
  exec the Rust binary.

Making the wrapper parse global flags would create a **double-parse** conflict
with the Rust clap layer — an anti-pattern. So the node's true scope is
**transparent pass-through correctness**, and the label was corrected to stop
misleading future agents (same technique as node 1-4's rename).

## Real defects found (and fixed)

The wrapper had two genuine pass-through bugs:

1. **argv mangling via `shell: true`** (old line 83). On Windows, spawning the
   binary through cmd.exe re-parses argv and corrupts arguments containing
   spaces / quotes / `^ & |`. Fix (Q3): drop the `shell` option on the exec
   `spawnSync`; the absolute `.exe` path goes straight to CreateProcess and argv
   is forwarded verbatim. The `cargo build` spawn keeps `shell: true` (cargo is
   resolved from PATH and does not carry user argv).

2. **exit-code masking via `result.status || 0`** (old line 86). A
   signal-killed child has `status === null`; `null || 0` reported a crash as
   **success (0)**. Fix (Q4): extract a pure `resolveExitCode({status, signal,
   error})`:
   - numeric `status` → propagate verbatim
   - `signal` set → `128 + signalNumber` (Unix convention; unknown signal → 1)
   - `error` set (spawn failed, ENOENT/EACCES) → 1
   - otherwise → 0

## Global-flag gap tracing (Q2)

The 5 TS global flags are **not** yet on the Rust `Cli` struct
(`crates/zbrain-cli/src/lib.rs` currently has only `--config` / `--debug`).
Per the "semantic deviation → sub-node" convention this is tracked two ways:

- **roadmap node `1-8`** (sibling of 1-5, pending): `Rust CLI global flag
  parity (--quiet/--progress-json/--progress-interval/--timeout/--explain)`.
- **code anchor**: `FUTURE(global-flag-parity)` comment in the header of
  `bin/zbrain-rs.js`, pointing at node 1-8 and forbidding wrapper-side parsing.

## Tests (Q5)

JS wrapper, not part of the bun/cargo suites. `resolveExitCode` is exported as
a pure function and tested with zero-dependency `node:test` in
`bin/zbrain-rs.test.mjs` (4 branches: status-0 / non-zero status / signal-kill /
spawn-error). Runnable via `npm run test:wrapper`
(`node --test bin/zbrain-rs.test.mjs`). The `main()` execution block is guarded
by an `import.meta.url === pathToFileURL(argv[1])` entrypoint check so importing
the module for tests does not spawn anything.

## Deliberately NOT touched

- Rust clap global flags (deferred to 1-8).
- `cargo build` spawn's `shell: true` (no user argv, needs PATH resolution).
- `scripts/postinstall.ts` Rust/TS fallback logic (belongs to 1-6 cleanup).

## Release-level bug found & fixed during wrap-up (Q6)

While staging the commit, discovered that `.gitignore` line 2 was a blanket
`bin/` ignore. That meant `bin/zbrain-rs.js` — the `package.json` `bin.zbrain`
entrypoint after the cutover — **was never version-controlled**. A fresh clone
or `npm install` would have no CLI entry script at all.

Fix: replaced the blanket `bin/` rule with precise ignores for only the bun
`--compile` self-contained artifacts:

```
bin/zbrain
bin/zbrain.exe
bin/zbrain-darwin-arm64
bin/zbrain-linux-x64
bin/zbrain-*-x64
bin/zbrain-*-arm64
```

so the source wrapper (`bin/zbrain-rs.js`) and its test (`bin/zbrain-rs.test.mjs`)
are now tracked, while compiled binaries remain ignored. Verified via
`git check-ignore`.

