# 1-8: Global flag gap audit & subsystem hand-off

**Roadmap node**: `1-8` (part2 config-bootstrap) — renamed from
`Rust CLI global flag parity (--quiet/--progress-json/--progress-interval/--timeout/--explain)`
to `global flag gap audit & subsystem hand-off (progress reporter / MCP timeout /
search attribution)`.

## Why this node was reframed (Q1)

Node 1-8 was split out of 1-5's grill to track the 5 TS global flags that the
Rust `Cli` struct does not yet expose. The naive plan was "migrate the 5 flags
to Rust clap `global = true`". Investigation found that **none of the 5 flags
has a working consumer in Rust** — the entire subsystems behind them are
unported. Adding the flags now would produce dead no-op flags, exactly the
`--offline` anti-pattern removed in node 1-3. So this node does **not add any
flag**; it is a one-shot audit + hand-off that records the gap authoritatively
and leaves near-site code anchors for whoever ports the subsystems later.

TS source of the flags: `src/core/cli-options.ts` `parseGlobalFlags`.

## Authoritative flag → subsystem → anchor table (Q5)

| TS flag | TS semantics | Blocked on subsystem | Rust landing site (code anchor) |
|---------|--------------|----------------------|---------------------------------|
| `--quiet` | Suppress human progress output | **Progress reporter** (TS `src/core/progress.ts`: human/json/quiet three-state + interval throttle). Rust has only a `CliOpts` data struct that is never populated or read. | `crates/zbrain-core/src/operation.rs` — `CliOpts` struct — `FUTURE(progress-reporter)` |
| `--progress-json` | Emit newline-delimited JSON progress events | Same as above | Same anchor |
| `--progress-interval=<ms>` | Min progress refresh interval | Same as above | Same anchor |
| `--timeout=<Ns\|Nms\|Nm>` | Per-call timeout for thin-client-routed MCP calls | **MCP per-call timeout wiring.** Routing skeleton exists (`McpClient::call_tool` + `RemoteMcpError::Timeout` variant) but `http_client = Client::new()` has no timeout and nothing consumes a timeout value. | `crates/zbrain-cli/src/mcp_client.rs` — `call_tool` — `FUTURE(mcp-timeout)` |
| `--explain` | Per-stage scoring attribution view for `search`/`query` (base_score + boost multipliers + reranker rank delta) | **Search rerank + per-stage attribution.** Rust `query` scoring is a hardcoded keyword-hit weighting (title +0.4 / content +0.4 / frontmatter +0.2) in `zbrain-core` engine with no rerank/boost/attribution stages. `doctor` already flags `reranker_health` as `UNMIGRATED_TS`. | `crates/zbrain-cli/src/lib.rs` — `QueryArgs` — `FUTURE(search-attribution)` |

Top-level wrapper anchor: `bin/zbrain-rs.js` header `FUTURE(global-flag-parity)`
points at this table and forbids wrapper-side flag parsing.

## Decisions

- **Q1** (A): Do not add placeholder flags. Reframe 1-8 as audit + hand-off.
  Dead no-op flags are a fresh `--offline`-style lie.
- **Q2** (A, later superseded by Q3): originally planned to split into 3
  sub-nodes by subsystem (progress reporter / MCP timeout / search attribution).
- **Q3** (B): Do **not** create pending sub-nodes in the part2 roadmap. Because
  parent/child status auto-sync means any pending child of 1-8 would block 1-8,
  and in turn block `1`, preventing part2 from ever closing. Instead 1-8 is a
  one-shot node completed by this audit + code anchors. The actual subsystem
  migrations belong to a future part3 / feature-migration roadmap, where they
  will be given real nodes. Same principle as node 1-4 (don't create pending
  nodes for work outside the current roadmap's scope).
- **Q4** (A): Place `FUTURE(...)` anchors near each subsystem's real landing
  site in Rust (not centralized in the wrapper), so a future agent editing
  e.g. the progress reporter sees "these flags are waiting on you" right there.
- **Q5** (C): No behavior tests — there is no behavior, and asserting a comment
  exists is a brittle test (unlike nodes 1-3/1-4 which anchor `UNMIGRATED_TS_*`
  array **constants**). Verification = `cargo build` (anchors don't break
  compilation) + grep that the 4 `FUTURE(...)` tags are present with consistent
  spelling + this audit's table as the single authoritative index.

## Verification checklist

- [ ] `cargo build -p zbrain-cli` succeeds (comment anchors are valid).
- [ ] `grep -rn "FUTURE(progress-reporter)\|FUTURE(mcp-timeout)\|FUTURE(search-attribution)\|FUTURE(global-flag-parity)"` returns exactly the 4 anchors above.
- [ ] This table matches the anchor locations.

## Deliberately NOT done

- No flags added to the Rust `Cli` struct (still only `--config` / `--debug`).
- No pending sub-nodes in the part2 roadmap (deferred to a future roadmap).
- No behavior/runtime change whatsoever — audit + comments only.
