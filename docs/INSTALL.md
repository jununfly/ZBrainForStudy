# Install

Three install paths. Pick one. Mix later if needed.

## 1. Run with an agent platform (recommended)

Already running [OpenClaw](https://github.com/garrytan/openclaw) or [Hermes](https://github.com/garrytan/hermes)?

```bash
bun install -g github:jununfly/zbrain
zbrain init --pglite                  # 2 seconds; no server
zbrain skillpack scaffold --all       # 43 skills scaffolded into your agent workspace
zbrain doctor                         # green checks all the way down
```

Your agent now reads `skills/RESOLVER.md` once per request, routes intent to the right skill, executes. New entity mentions create new pages. Daily cron runs enrichment overnight.

Scaffolded skills are first-class files in your agent repo — edit freely. To pull upstream zbrain improvements later, `zbrain skillpack reference <name>` diffs your local copy vs the bundle. The legacy `skillpack install` managed-block model was retired in v0.36.0.0; if you're upgrading from an older release, run `zbrain skillpack migrate-fence` once to strip the legacy fence and keep your existing skill rows.

To upgrade later: `zbrain upgrade` runs schema migrations + post-upgrade prompts (chunker bumps, the v0.36.2.0 ZeroEntropy switch). Always TTY-only; non-TTY upgrades skip prompts with informational stderr lines.

## 2. CLI standalone

No agent platform, just shell + MCP-aware editor.

```bash
bun install -g github:jununfly/zbrain
zbrain init --pglite
```

> **If `bun install -g` hits a postinstall error** (Bun blocks postinstall hooks in some environments), the CLI prints a recovery hint pointing at [#218](https://github.com/jununfly/zbrain/issues/218). Run `zbrain doctor` to diagnose, then `zbrain apply-migrations --yes` manually. The deterministic fallback is `git clone https://github.com/jununfly/zbrain.git ~/zbrain && cd ~/zbrain && bun install && bun link`.

The init flow detects your repo size and suggests Supabase for brains > 1000 markdown files. To switch later:

```bash
zbrain migrate --to supabase     # PGLite → Postgres
zbrain migrate --to pglite       # Postgres → PGLite (rare)
```

For shared / large / multi-machine deployments (a team or company brain with multiple users hitting one server over HTTP MCP with OAuth scoping per user), follow the dedicated walkthrough: **[Tutorial: set up ZBrain as your company brain](tutorials/company-brain.md)**.

API keys live in `~/.zbrain/config.json` (file plane) or env vars (`OPENAI_API_KEY`, `ZEROENTROPY_API_KEY`, `VOYAGE_API_KEY`, `ANTHROPIC_API_KEY`). Set via CLI:

```bash
zbrain config set zeroentropy_api_key sk-...
zbrain config set anthropic_api_key sk-ant-...
```

Common follow-ups:

```bash
zbrain import ~/my-knowledge      # bulk-import a markdown folder
zbrain sync --watch               # live-sync a git repo (autopilot mode)
zbrain autopilot --install        # background daemon for nightly enrichment
```

## 3. MCP server (any MCP client)

```bash
zbrain serve                      # stdio MCP (Claude Desktop / Code / Cursor)
zbrain serve --http               # HTTP MCP with OAuth 2.1 + admin dashboard
```

Per-client setup guides live in [`docs/mcp/`](mcp/):

- [`docs/mcp/CLAUDE_CODE.md`](mcp/CLAUDE_CODE.md)
- [`docs/mcp/CLAUDE_DESKTOP.md`](mcp/CLAUDE_DESKTOP.md)
- [`docs/mcp/CHATGPT.md`](mcp/CHATGPT.md)
- [`docs/mcp/PERPLEXITY.md`](mcp/PERPLEXITY.md)
- [`docs/mcp/DEPLOY.md`](mcp/DEPLOY.md) — production deploy patterns

The HTTP server ships with an admin SPA at `/admin`, an SSE activity feed at `/admin/events`, DCR-style client registration, scope-gated `read`/`write`/`admin` access, and rate limiting.

## Thin-client mode

Connect to someone else's brain without running a local engine:

```bash
zbrain init --mcp-only            # configures remote MCP, skips local DB
```

Useful for: team mounts, brain-as-a-service deployments, dev machines without disk space. Most local commands refuse with a paste-ready hint. See [`docs/architecture/topologies.md`](architecture/topologies.md).

## Verifying the install

```bash
zbrain doctor --json              # full health check
zbrain models                     # which AI models are configured for what
zbrain models doctor              # 1-token probe per configured model
```

If anything's yellow, `zbrain doctor` names the fix command in the message. Most issues are missing API keys or stale schema (`zbrain upgrade --force-schema`).
