# Agents working on ZBrain

ZBrain is the Rust-first line of this repository. The old TypeScript GBrain line is legacy code that is being replaced slice by slice. Use ZBrain as the product name in repository-facing language, public command examples, package names, executable names, config files, environment variables, dotfiles, and docs.

This is your install + operating protocol. Claude Code reads `./CLAUDE.md` automatically. Everyone else (Codex, Cursor, OpenClaw, Aider, Continue, or an LLM fetching via URL) starts here.

## Current migration stance

- Canonical product language: **ZBrain**.
- Canonical executable/config examples: `zbrain`, `zbrain.yml`, `ZBRAIN_*`, `.zbrain*`, `~/.zbrain`.
- Domain terms `brain` and `source` are still valid and should not be renamed.
- TypeScript code is legacy but not deleted mechanically. Delete TS surfaces only after a Rust replacement slice lands and the corresponding behavior is verified.
- Do not add GBrain aliases or compatibility fallbacks in the first cleanup phase. There are no online users yet.
- If a TS remnant is not safe to delete during Rust migration, stop and record an explicit decision.

## Install (current ZBrain line)

1. Install dependencies with Bun:
   ```bash
   curl -fsSL https://bun.sh/install | bash
   export PATH="$HOME/.bun/bin:$PATH"
   bun install
   ```
2. Build the CLI:
   ```bash
   bun run build
   ```
3. Init the brain:
   ```bash
   zbrain init
   ```
   ZBrain defaults to PGLite for zero-config local use. For 1000+ files or multi-machine sync, prefer Postgres + pgvector.
4. **STOP — ask the user about search mode.** `zbrain init` may print a 9-cell cost matrix (mode x downstream model) preceded by `[AGENT]` markers. Relay the matrix to the operator and confirm their choice before continuing. Cost spread between corners is large; silent acceptance is the wrong default. See [`./INSTALL_FOR_AGENTS.md`](./INSTALL_FOR_AGENTS.md) for the full ask-the-user protocol.
5. Read [`./INSTALL_FOR_AGENTS.md`](./INSTALL_FOR_AGENTS.md) for the full install flow: API keys, identity, cron, and verification.

## Read this order

1. `./AGENTS.md` (this file) — install + operating protocol.
2. `./ZJ-CONTEXT.md` — canonical domain language for the Rust rewrite and brand migration.
3. [`./docs/zj-adr/ZJ-0001-zbrain-rust-rewrite-brand.md`](./docs/zj-adr/ZJ-0001-zbrain-rust-rewrite-brand.md) — repository-level decision record for ZBrain naming and compatibility stance.
4. [`./CLAUDE.md`](./CLAUDE.md) — architecture reference, key files, trust boundaries, test layout.
5. [`./docs/architecture/brains-and-sources.md`](./docs/architecture/brains-and-sources.md) — the two-axis mental model: brain = which DB, source = which repo in the DB. Every query routes on both axes.
6. [`./skills/conventions/brain-routing.md`](./skills/conventions/brain-routing.md) — agent-facing decision table for brain/source switching and cross-brain federation.
7. [`./skills/RESOLVER.md`](./skills/RESOLVER.md) — skill dispatcher. Read before any task.

## Trust boundary (critical)

ZBrain distinguishes **trusted local CLI callers** (`OperationContext.remote = false`, set by `src/cli.ts`) from **untrusted agent-facing callers** (`remote = true`, set by `src/mcp/server.ts`). Security-sensitive operations like `file_upload` tighten filesystem confinement when `remote = true` and default to strict behavior when unset. If you are writing or reviewing an operation, consult `src/core/operations.ts` for the contract.

## Common tasks

- **Configure:** [`docs/ENGINES.md`](./docs/ENGINES.md), [`docs/guides/live-sync.md`](./docs/guides/live-sync.md), [`docs/mcp/DEPLOY.md`](./docs/mcp/DEPLOY.md).
- **Debug:** [`docs/ZBRAIN_VERIFY.md`](./docs/ZBRAIN_VERIFY.md), [`docs/guides/minions-fix.md`](./docs/guides/minions-fix.md), `zbrain doctor --fix`.
- **Migrate / upgrade:** `zbrain upgrade`, [`docs/UPGRADING_DOWNSTREAM_AGENTS.md`](./docs/UPGRADING_DOWNSTREAM_AGENTS.md), [`skills/migrations/`](./skills/migrations/), `zbrain apply-migrations --yes`.
- **Eval retrieval changes:** capture is off by default. To benchmark a retrieval change against real captured queries, set `ZBRAIN_CONTRIBUTOR_MODE=1`, then `zbrain eval export --since 7d > base.ndjson` and `zbrain eval replay --against base.ndjson`. For public benchmark coverage, `zbrain eval longmemeval <dataset.jsonl>` runs against an isolated in-memory PGLite per question; your `~/.zbrain` is never opened. Full guide: [`docs/eval-bench.md`](./docs/eval-bench.md).
- **Drive the brain to a target health score:** `zbrain doctor --remediation-plan --json` previews fixes; `zbrain doctor --remediate --yes --target-score 90 --max-usd 5` walks a dependency-ordered plan and refuses to spend past the cost cap.
- **Track temporal entity updates:** use `zbrain eval trajectory <entity-slug>` or `zbrain founder scorecard <entity-slug>` for chronological history and signal rollups.
- **Everything else:** [`./llms.txt`](./llms.txt) is the full documentation map. [`./llms-full.txt`](./llms-full.txt) is the same map with core docs inlined for single-fetch ingestion.

## Before shipping

Easiest path: `bun run ci:local` runs the full CI gate inside Docker. Use `bun run ci:local:diff` for the diff-aware subset during focused iteration.

Manual path: `bun test` plus the E2E lifecycle described in `./CLAUDE.md`.

Ship via the `/ship` skill, not by hand.

## Privacy

Never commit real names of people, companies, or funds into public artifacts. Public docs must use generic placeholders (`alice-example`, `acme-example`, `fund-a`).

## Forks

If you are a fork, regenerate `llms.txt` + `llms-full.txt` with your own URL base before publishing: `LLMS_REPO_BASE=https://raw.githubusercontent.com/your-org/your-fork/main bun run build:llms`.
