# Install

ZBrain is Rust-first. The old `bun install -g github:jununfly/zbrain` path (TypeScript line) is retired — build the CLI from this repository instead.

## 1. Build from source (recommended)

Prerequisites: Rust 1.88+ (MSRV) and a cargo toolchain.

```bash
# Clone
git clone https://github.com/jununfly/zbrain.git
cd zbrain

# Build the CLI
cargo build --release -p zbrain-cli

# Run it directly
./target/release/zbrain --help

# …or via the cross-platform Node wrapper (finds the binary; builds if missing)
node bin/zbrain-rs.js --help
```

Initialize a brain and verify the install:

```bash
zbrain init          # libsql embedded SQLite by default (zero-config, no server)
zbrain doctor        # verify health + connectivity
```

`zbrain init` creates a local brain backed by **libsql** (embedded SQLite). No server, no Docker, no external dependency. This is the default for personal brains.

## 2. Run as an agent brain (MCP)

ZBrain exposes its surface over MCP so any agent client can drive it:

```bash
zbrain serve-mcp          # stdio MCP (local subprocess; for Claude Code, Cursor, Windsurf)
zbrain serve              # HTTP API + admin SPA (for remote clients)
```

Point your agent client at the `zbrain` binary (via the `bin/zbrain-rs.js` wrapper if you prefer a single entrypoint). The operation/trust contract is described in `crates/zbrain-core/src/operation.rs`; the agent-facing protocol is in [AGENTS.md](../AGENTS.md) and [CLAUDE.md](../CLAUDE.md).

## 3. Postgres (shared / large / multi-machine)

For team or company brains — multiple users hitting one server over HTTP with per-user scoping — run the **Postgres** engine instead of libsql.

```bash
# Point the engine at a Postgres database
export ZBRAIN_DATABASE_URL="postgres://user:pass@host:5432/zbrain"
zbrain init --engine postgres
zbrain apply-migrations   # bring the schema up to the latest version
zbrain doctor
```

The Postgres engine is `sqlx`-backed (`crates/zbrain-core`). Set `ZBRAIN_DATABASE_URL` (or the equivalent config key) before `zbrain init`; the embedded libsql engine remains the zero-config default when no URL is set.

## Verify

```bash
zbrain doctor             # green checks all the way down
zbrain query "hello"      # sanity-check retrieval
```

## Where to go next

- [../README.md](../README.md) — product overview + CLI surface
- [../AGENTS.md](../AGENTS.md) — operating + contribution protocol
- [../RUST_REWRITE.md](../RUST_REWRITE.md) — migration status
- [architecture/brains-and-sources.md](architecture/brains-and-sources.md) — brain/source model
- [architecture/RETRIEVAL.md](architecture/RETRIEVAL.md) — hybrid search + graph theory
