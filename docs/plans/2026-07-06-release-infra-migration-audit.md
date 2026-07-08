# Release infra migration — hand-off audit (roadmap 1-1-3)

Date: 2026-07-06
Branch: rust-rewrite

This slice migrated the release/build infrastructure from the retired TS
`bun --compile` model to the Rust binary, up to the boundary of what can be
verified on a Windows dev machine. Everything past that boundary is documented
here as an explicit hand-off, NOT silently assumed to work.

## What was done (locally verified)

### 1. `.cargo/config.toml` — removed the hardcoded `[build] target`
- **Before:** `target = "x86_64-pc-windows-msvc"` pinned EVERY `cargo`
  invocation to one triple. This (a) blocked cross-compile — `cargo build
  --target=<other>` double-applied the pin — and (b) relocated the host binary
  to `target/x86_64-pc-windows-msvc/{debug,release}/` instead of the natural
  `target/{debug,release}/` the wrapper searches (Part2 rc=127 footgun).
- **After:** no pin; host default toolchain is used. Verified: `rustc -vV`
  host = `x86_64-pc-windows-msvc`, `rustup` default = msvc, no `rust-toolchain`
  file — so removing the pin keeps producing an MSVC binary locally.
- **Verified locally:** `cargo build -p zbrain-cli` succeeds and lands at
  `target/debug/zbrain.exe`; `node bin/zbrain-rs.js --version` resolves it
  → `zbrain 0.0.1`.

### 2. `bin/zbrain-rs.js` — wrapper now finds debug AND release binaries
- Extracted a pure, exported `getBinaryCandidates(platform, arch, root)`.
- Candidate ordering: triple-specific before bare `target/`; RELEASE before
  DEBUG within each location (prefer the shipped/optimized binary, fall back to
  a plain `cargo build` for dev). Closes the rc=127 footgun where a bare
  `cargo build` (debug) was invisible to the wrapper.
- **Verified locally:** `bin/zbrain-rs.test.mjs` — 7/7 pass (4 existing
  exit-code cases + 3 new candidate-list cases: release-before-debug ordering,
  plain `target/debug` coverage, unknown-platform fallback to both profiles).

### 3. `.github/workflows/release.yml` — Rust cross-compile matrix
- **Before:** compiled TS via `bun build --compile` to 2 targets, artifacts
  named `gbrain-*` (stale branding), uploaded TS binaries. The Rust binary was
  never in the release pipeline.
- **After:** 4-triple matrix, each running
  `cargo build --release --target <triple> -p zbrain-cli`, artifacts named by
  triple (no `gbrain-*`):
  - `aarch64-apple-darwin` (macos-latest)
  - `x86_64-apple-darwin` (macos-latest)
  - `x86_64-unknown-linux-gnu` (ubuntu-latest)
  - `x86_64-pc-windows-msvc` (windows-latest)
- **Verified locally:** YAML parses (js-yaml). Runner execution is NOT verified
  (see blind spots).

### 4. `package.json` — retired the TS `build:all`
- `build:all` now runs `bun run build:rust` (= `cargo build --release -p
  zbrain-cli`) instead of the TS `bun --compile` cross-compile.
- `prepublish:clawhub` (→ `build:all`) therefore produces a working
  HOST-platform Rust binary locally. `publish:clawhub` is unchanged.

## Blind spots — CI / other-OS verified later (HAND-OFF)

A mac/linux/CI run MUST confirm each of these; none is verifiable on this
Windows dev box (only `x86_64-pc-windows-{msvc,gnu}` targets installed):

1. **Actual darwin/linux cross-compilation.** `release.yml` builds each triple
   on its NATIVE runner (macos-latest / ubuntu-latest / windows-latest), so no
   true cross-linker is needed — but the matrix has never been executed. First
   `v*` tag push is the real test. Confirm each job produces a runnable binary.

2. **`clawhub package publish . --family bundle-plugin`.** Never run here.
   Confirm the bundle a real publish produces actually contains a usable binary
   for the target platform (see delivery-gap below).

3. **DELIVERY GAP — the wrapper only searches `target/`.** `bin/zbrain-rs.js`
   resolves binaries from `target/{,<triple>/}{release,debug}/`. But:
   - `release.yml` uploads binaries as GitHub release ARTIFACTS, not into
     `target/`. Nothing yet downloads a release artifact and places it where the
     wrapper looks.
   - `publish:clawhub` publishes `.`; `target/` is gitignored and not obviously
     included in the bundle.
   - **Today this only works because the wrapper's fallback runs `cargo build
     --release` at runtime** (requires the end user to have a Rust toolchain).
   - **Follow-up needed:** wire release artifacts (or a postinstall download)
     into a path the wrapper searches, so end users without cargo get a
     pre-built binary. Until then, cargo-at-runtime is the only delivery path.

4. **Version drift (pre-existing, out of scope).** `zbrain --version` →
   `0.0.1` (Cargo.toml) vs `package.json` 0.41.x. Version sync is a separate
   concern, noted in the Part2 final-validation report; not touched here.

## Files touched
- `.cargo/config.toml` (rewritten — removed pin, documented why)
- `bin/zbrain-rs.js` (extracted `getBinaryCandidates`, added debug coverage)
- `bin/zbrain-rs.test.mjs` (+3 candidate-list tests)
- `.github/workflows/release.yml` (rewritten — Rust 4-triple matrix)
- `package.json` (`build:all` → `bun run build:rust`)
