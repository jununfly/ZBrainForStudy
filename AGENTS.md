# Agents working on ZBrain

ZBrain is the Rust-first line of this repository. The old TypeScript GBrain line is legacy code that is being replaced slice by slice. Use ZBrain as the product name in repository-facing language, public command examples, package names, executable names, config files, environment variables, dotfiles, and docs.

This is your install + operating protocol. Claude Code reads `./CLAUDE.md` automatically. Everyone else (Codex, Cursor, OpenClaw, Aider, Continue, or an LLM fetching via URL) starts here.

## Current migration stance

- Canonical product language: **ZBrain**.
- Canonical executable/config examples: `zbrain`, `zbrain.yml`, `ZBRAIN_*`, `.zbrain*`, `~/.zbrain`.
- Domain terms `brain` and `source` are still valid and should not be renamed.
- TypeScript code (`src/`, `admin/`) is legacy but not deleted mechanically. Delete TS surfaces only after a Rust replacement slice lands and the corresponding behavior is verified.
- Do not add GBrain aliases or compatibility fallbacks in the first cleanup phase. There are no online users yet.
- If a TS remnant is not safe to delete during Rust migration, stop and record an explicit decision (see [`docs/plans/KNOWN-GAPS.md`](docs/plans/KNOWN-GAPS.md)).

## Build + run (current ZBrain line)

The product is a Cargo workspace. There is no `bun run build` for the engine/CLI anymore — the CLI and core are Rust.

```bash
# Build the CLI
cargo build --release -p zbrain-cli

# Run tests / lints across the whole workspace (required before shipping)
cargo test    --workspace --all-targets
cargo clippy  --workspace --all-targets -- -D warnings

# Run the CLI directly, or via the cross-platform Node wrapper
./target/release/zbrain --help
node bin/zbrain-rs.js --help     # locates the built binary; falls back to `cargo build`
```

The `bin/zbrain-rs.js` wrapper is a transparent argv pass-through that finds the compiled binary in `target/` per-platform and forwards exit codes. It does **not** parse flags — global-flag semantics live in the Rust `clap` layer (`crates/zbrain-cli/src/lib.rs`).

## Read this order

1. `./AGENTS.md` (this file) — install + operating protocol.
2. [`./ZJ-CONTEXT.md`](./ZJ-CONTEXT.md) — canonical domain language for the Rust rewrite and brand migration.
3. [`./RUST_REWRITE.md`](./RUST_REWRITE.md) — migration status + slice map (supersedes the stale 8-slice table; current work is tracked in `docs/plans/`).
4. [`./docs/plans/`](./docs/plans/) — roadmap (Part1–Part13), residual-TS inventory, and known gaps.
5. [`./CLAUDE.md`](./CLAUDE.md) — architecture reference, key files, trust boundaries, test layout.
6. [`./docs/architecture/brains-and-sources.md`](./docs/architecture/brains-and-sources.md) — the two-axis mental model: brain = which DB, source = which repo in the DB. Every query routes on both axes.
7. [`./CONTRIBUTING.md`](./CONTRIBUTING.md) — contributor guide, test discipline, eval-capture mode.
8. [`./docs/CROSS_OS_AGENT_GUIDE.md`](./docs/CROSS_OS_AGENT_GUIDE.md) — cross-OS agent dev do's and don'ts (Windows/macOS/WSL git & cargo pitfalls). Read before any git/cargo op from a non-Linux shell.

## Trust boundary (critical)

ZBrain distinguishes **trusted local CLI callers** from **untrusted agent-facing callers** via `OperationContext.remote` (set `false` for local CLI, `true` for MCP/HTTP transports). Security-sensitive operations tighten filesystem confinement or require `admin` scope when `remote = true`. The operation contract and the `remote` flag live in `crates/zbrain-core/src/operation.rs` — consult it when writing or reviewing an operation. The CLI entrypoint sets the flag in `crates/zbrain-cli/src/lib.rs`; the MCP/HTTP servers set it before dispatch.

## Common tasks

- **Configure / engines:** [`docs/ENGINES.md`](./docs/ENGINES.md), [`docs/architecture/brains-and-sources.md`](./docs/architecture/brains-and-sources.md).
- **Debug:** `zbrain doctor`, `zbrain doctor --fix`.
- **Migrate / upgrade:** `zbrain apply-migrations`.
- **Eval retrieval changes:** capture is off by default. To benchmark a retrieval change against real captured queries, set `ZBRAIN_CONTRIBUTOR_MODE=1`, then `zbrain eval export --since 7d > base.ndjson` and `zbrain eval replay --against base.ndjson`. Full guide: [`docs/eval-bench.md`](./docs/eval-bench.md).
- **Track temporal entity updates:** `zbrain find-trajectory <entity-slug>` for chronological history and signal rollups.
- **Everything else:** [`./llms.txt`](./llms.txt) is the full documentation map for single-fetch ingestion.

## Before shipping

Easiest path — run the workspace gate locally:

```bash
cargo test    --workspace --all-targets
cargo clippy  --workspace --all-targets -- -D warnings
```

Ship via the project's release flow, not by hand.

## Privacy

Never commit real names of people, companies, or funds into public artifacts. Public docs must use generic placeholders (`alice-example`, `acme-example`, `fund-a`).

## Forks

If you are a fork, regenerate `llms.txt` + `llms-full.txt` with your own URL base before publishing (the generator lives under `tools/` and is being ported to Rust alongside the rest of the CLI).
