# ZBrain

> **Search gives you raw pages. ZBrain gives you the answer.** A brain layer for AI agents that does retrieval, synthesis, and graph traversal in one box — built in Rust.

ZBrain is the knowledge layer for an AI agent: it ingests your notes, meetings, emails, and ideas; builds a self-wiring knowledge graph; and answers questions with synthesized, cited prose instead of a list of pages to read yourself.

> **Status — Rust rewrite in progress.** This repository is mid-migration from a TypeScript codebase to Rust. The CLI, engines, and core operations now run in Rust (`crates/`); the legacy TypeScript under `src/` and `admin/` is being deleted slice by slice as each Rust replacement lands. See [RUST_REWRITE.md](RUST_REWRITE.md) and [`docs/plans/`](docs/plans/) for the current slice status.

## What ZBrain does

- **Synthesis layer.** `zbrain think` returns an actual answer — well-cited prose across people, companies, deals, and ideas — with an explicit note on what the brain doesn't know yet (gap analysis). Not "10 chunks that mention your query."
- **Self-wiring knowledge graph.** Every page write extracts entity references and creates typed edges (`attended`, `works_at`, `invested_in`, `founded`, `advises`, …) with zero LLM calls. Ask "who works at Acme AI?" and get answers vector search alone can't reach.
- **Two ways to query.** `zbrain query` for fast raw retrieval (hybrid vector + keyword scoring); `zbrain think` for the synthesized answer. Pair either with `find-trajectory` to trace how an entity changed over time.

## Install (build from source)

Prerequisites: Rust 1.88+ (MSRV) and a cargo toolchain.

```bash
# Build the CLI
cargo build --release -p zbrain-cli

# Run it directly, or via the cross-platform wrapper that locates the binary
./target/release/zbrain --help
node bin/zbrain-rs.js --help        # falls back to `cargo build` if no binary is found
```

Initialize a brain and verify the install:

```bash
zbrain init          # libsql embedded SQLite by default (zero-config, no server)
zbrain doctor        # verify health + connectivity
```

For shared / large / multi-machine deployments, ZBrain also runs on **Postgres** (via `sqlx`). See [docs/INSTALL.md](docs/INSTALL.md).

## Quickstart

```bash
zbrain capture "the thought I want to remember"
zbrain capture --file ./notes/today.md
zbrain query "what themes show up across my notes?"
zbrain think "who's working on AI agents at portfolio companies?"
```

## Architecture

**Rust workspace.** The product is a Cargo workspace:

| Crate | Role |
|-------|------|
| `zbrain-core` | Engine trait, types, operations, migrations, schema packs |
| `zbrain-cli` | `clap` command-line interface (bin: `zbrain`) |
| `zbrain-mcp` | Model Context Protocol server |
| `zbrain-web` | HTTP API + admin surface |
| `zbrain-worker` | Background job / maintenance worker |
| `zbrain-chunking` | Content chunking (tree-sitter semantic chunking) |
| `zbrain-svg` | SVG / diagram rendering helpers |

**Two engines, one contract.** `BrainEngine` (in `zbrain-core`) defines the operation set both engines implement:

- **libsql** — embedded SQLite, zero-config default for personal brains.
- **Postgres** — `sqlx`-backed, for shared / large / multi-machine deployments.

The brain repo (your markdown) is the system of record; ZBrain syncs it into the engine's store for retrieval. See [`docs/architecture/`](docs/architecture/) for system design:

- [brains-and-sources](docs/architecture/brains-and-sources.md) — the two-axis mental model (brain = which DB, source = which repo)
- [RETRIEVAL](docs/architecture/RETRIEVAL.md) — hybrid search + graph theory
- [schema-packs](docs/architecture/schema-packs.md) — agent-authored page shapes
- [topologies](docs/architecture/topologies.md) — deploy topologies
- [system-of-record](docs/architecture/system-of-record.md) — git-as-source-of-truth

## CLI surface

The `zbrain` CLI covers ingestion, query, graph, schema, jobs, and serving:

```bash
zbrain init | doctor | config            # setup + health
zbrain capture | put-page | sync          # ingest
zbrain query | think | get-page           # retrieve + synthesize
zbrain graph-query | find-trajectory | find-contradictions | recall
zbrain schema | schema-sql | skillpack | skillify
zbrain serve (HTTP) | serve-mcp (stdio)   # connect to an AI client
zbrain jobs | agent | autopilot | remote
zbrain apply-migrations | reindex | dream
```

Run `zbrain --help` (or `zbrain <subcommand> --help`) for the full, current list.

## Docs

- [AGENTS.md](AGENTS.md) — operating + contribution protocol for agents
- [CLAUDE.md](CLAUDE.md) — deep operating context (architecture, key files, trust boundaries)
- [CONTRIBUTING.md](CONTRIBUTING.md) — contributor guide + test discipline
- [SECURITY.md](SECURITY.md) — threat model + hardening
- [docs/INSTALL.md](docs/INSTALL.md) — every install path, end to end
- [RUST_REWRITE.md](RUST_REWRITE.md) — migration status + slice map
- [docs/plans/](docs/plans/) — roadmap (Part1–Part13) and known gaps
- [ZJ-CONTEXT.md](ZJ-CONTEXT.md) — canonical domain language for the rewrite
- [llms.txt](llms.txt) — documentation map for agents

## License

MIT.
