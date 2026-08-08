# CLAUDE.md

ZBrain is a personal knowledge brain and GStack mod for agent platforms. Pluggable
engines: PGLite (embedded Postgres via WASM, zero-config default) or Postgres + pgvector
+ hybrid search in a managed Supabase instance. `zbrain init` defaults to PGLite;
suggests Supabase for 1000+ files. GStack teaches agents how to code. ZBrain teaches
agents everything else: brain ops, signal detection, content ingestion, enrichment,
cron scheduling, reports, identity, and access control.

## Two organizational axes (read this first)

ZBrain knowledge is organized along two orthogonal axes. Users AND agents must
understand both, or queries misroute silently.

- **Brain** — WHICH DATABASE. Your personal brain is `host`. You can mount
  additional brains (team-published, each with their own DB and access policy)
  via `zbrain mounts add` (v0.19+). Routing: `--brain`, `ZBRAIN_BRAIN_ID`,
  `.zbrain-mount` dotfile.
- **Source** — WHICH REPO INSIDE THE DATABASE. A brain can hold many sources
  (wiki, gstack, openclaw, essays). Slugs scope per source. Routing:
  `--source`, `ZBRAIN_SOURCE`, `.zbrain-source` dotfile.

Both axes follow the same 6-tier resolution pattern. Read
`docs/architecture/brains-and-sources.md` for topology diagrams (personal, team
mount, CEO-class with multiple team brains) and
`skills/conventions/brain-routing.md` for the agent-facing decision table.

## Architecture

ZBrain is a Rust workspace (Cargo). The CLI binary `zbrain` is built from
`crates/zbrain-cli` and launched via `bin/zbrain-rs.js` (a thin Node wrapper that
runs the compiled binary, falling back to `cargo build -p zbrain-cli` when no
prebuilt binary is found). Domain logic lives in `crates/zbrain-core` behind the
`BrainEngine` trait; the CLI and the MCP server (`crates/zbrain-mcp`) are both
thin front ends that route through `run_operation` in
`crates/zbrain-core/src/operation.rs`.

Two storage engines implement `BrainEngine`:
- **libsql** (default) — embedded SQLite via the `libsql` crate.
- **Postgres** — server-backed via `sqlx` (feature `postgres`).

Trust boundary: `OperationContext.remote` in `crates/zbrain-core/src/operation.rs`
flags untrusted callers. `remote: false` is the only trusted path; anything else
is treated as remote (fail-closed). HTTP/MCP transports set it explicitly; the
stdio CLI sets it to `false`.
## Key files

The repo is a Cargo workspace (`Cargo.toml`) with these crates:

- `crates/zbrain-core` — domain logic and the `BrainEngine` trait. Key modules:
  - `src/operation.rs` — `run_operation` + `OperationContext` (trust boundary; `remote` flag).
  - `src/engine.rs` — `BrainEngine` trait + engine selection.
  - `src/libsql.rs` / `src/postgres.rs` — the two `BrainEngine` implementations.
  - `src/schema_pack/` — the 32-verb schema-pack manager.
  - `src/migration_registry.rs` plus `migrations/` and `migrations-sqlite/` — dual-dialect DB migrations.
  - `src/skillify/`, `src/skillpack/` — skill scaffolding + skillpack install/harvest.
- `crates/zbrain-cli` — the `zbrain` CLI (`main.rs` + `lib.rs`), clap subcommands, plus `bin/zbrain-rs.js` at repo root.
- `crates/zbrain-mcp` — Model Context Protocol server (stdio + HTTP).
- `crates/zbrain-web` — admin SPA + HTTP API server (`zbrain serve`).
- `crates/zbrain-chunking` — text chunking strategies.
- `crates/zbrain-svg` — SVG / diagram rendering for `zbrain publish`.
- `crates/zbrain-worker` — background job worker (supervisor).

Thin-client routing: untrusted callers (HTTP/MCP) route operations through a
remote host; `OperationContext.remote` is set by the transport. See
`crates/zbrain-core/src/operation.rs`.
### BrainBench — in a sibling repo (v0.20+)

BrainBench — the public benchmark for personal-knowledge agent stacks — lives in
[github.com/garrytan/zbrain-evals](https://github.com/garrytan/zbrain-evals). It
depends on zbrain as a consumer; zbrain never pulls in the ~5MB eval corpus or
the pdf-parse dev dep at install time.

zbrain's public API surface (the exports map in `Cargo.toml`) is what
zbrain-evals consumes: `zbrain/engine`, `zbrain/types`, `zbrain/operations`,
`zbrain/pglite-engine`, `zbrain/link-extraction`, `zbrain/import-file`,
`zbrain/transcription`, `zbrain/embedding`, `zbrain/config`, `zbrain/markdown`,
`zbrain/backoff`, `zbrain/search/hybrid`, `zbrain/search/expansion`,
`zbrain/extract`. Removing any of these is a breaking change for the
zbrain-evals consumer.

## v0.36.1.0 Hindsight calibration wave (key files cluster)

The wave that taught zbrain to know how the user tends to be wrong + use
that knowledge at every advice surface. Six-migration schema (v67-v72),
three new cycle phases, eight expansions, one admin tab. Plan persisted
at `~/.claude/plans/system-instruction-you-are-working-rippling-knuth.md`.
Convention skill at `skills/conventions/calibration.md` has the agent-
facing rules.

**v0.37.2.0 hotfix (2026-05-20, migration v80)** — `takes_resolution_consistency`
CHECK widened to accept `quality='unresolvable' AND outcome=NULL` as the 4th
valid resolution state. Column-level CHECK on `resolved_quality` renamed to
`takes_resolved_quality_values` and widened to enumerate all 4 states. Unblocks
production grading scripts that write the judge's 4th verdict type. `Take.resolved_quality`,
`TakeResolution.quality`, and `takes-fence.ts:TakeQuality` all widen to 4-state.
`TakesScorecard` gains sibling fields `unresolvable_count` + `unresolvable_rate`;
`resolved` stays 3-state (correct+incorrect+partial) so historical comparisons
hold. `finalizeScorecard` formula: `unresolvable_rate = unresolvable_count / (resolved
+ unresolvable_count)`, NULL when both 0. Spec doc preserved at
`docs/architecture/calibration-quality-gate-spec.md` (from closed PR #1191) since
the follow-up minor (forthcoming) ships the falsifiability + per-category
calibration on top. Migration renumbered v74→v79→v80 during successive master
merges — v0.37.0.0's autonomous-remediation wave claimed v68-v78, then v0.37.1.0
(brainstorm/lsd) claimed v79. Pinned by
R1-R5 in `tests/unit/takes-resolution.test.ts` and `tests/unit/migrate.test.ts`'s
v80 structural + PGLite round-trip suite (CHECK admits unresolvable+NULL, still
rejects partial+true and unresolvable+true|false, pre-v80 NULL/NULL rows survive).

- `crates/zbrain-core/src/cycle/base-phase.ts` — abstract `BaseCyclePhase` class.
  Enforces `sourceScopeOpts(ctx)` threading at the type level; closes
  the v0.34.1 source-isolation leak class structurally for every new
  phase. Inherits source-scope, budget meter, error envelope, progress
  reporter. propose_takes / grade_takes / calibration_profile all
  extend it.
- `crates/zbrain-core/src/cycle/propose-takes.ts` — LLM scans markdown prose,
  proposes gradeable claims to `take_proposals` queue. Idempotency
  cache on `(source_id, page_slug, content_hash, prompt_version)`
  composite unique index. F2 fence-dedup: existing canonical takes
  passed to extractor as context. v0.36.1.0 ships a stub prompt; tuned
  prompt arrives via the T19 synthetic corpus build.
- `crates/zbrain-core/src/cycle/grade-takes.ts` — walks unresolved takes older than
  6 months, retrieves evidence, asks judge model, caches verdict.
  Auto-resolve DISABLED by default (D17). Conservative thresholds:
  >=0.95 single OR >=0.85 ensemble 3/3 unanimous. T5 ensemble
  (`aggregateEnsemble`) reuses v0.27.x cross-modal substrate; fires on
  borderline 0.6-0.95 band. Writes to `take_grade_cache`.
- `crates/zbrain-core/src/cycle/calibration-profile.ts` — aggregates resolved takes
  into 2-4 narrative pattern statements + active bias tags. Voice-gated
  via `gateVoice()`. Cold-brain skip when <5 resolved. Writes to
  `calibration_profiles` with audit columns (`voice_gate_passed`,
  `voice_gate_attempts`, `grade_completion`).
- `crates/zbrain-core/src/calibration/voice-gate.ts` — single `gateVoice()` function
  (D24), mode parameter (`pattern_statement` | `nudge` |
  `forecast_blurb` | `dashboard_caption` | `morning_pulse`). 2 regens
  then hand-written template fallback from
  `crates/zbrain-core/src/calibration/templates.ts`. Haiku judge with mode-specific
  rubrics; all rubrics structurally forbid clinical/preachy voice.
- `crates/zbrain-core/src/calibration/cross-brain.ts` — D18 4-rule contract for
  cross-brain calibration reads. Local-first → mount-fallback (only
  with `canReadMountsForCtx(ctx)` true) → cross-brain attribution via
  `source_brain_id` + `from_mount` → subagent prohibition closes the
  OAuth-token-to-cross-brain-leak surface. All 4 rules pinned in
  `tests/unit/cross-brain-calibration.test.ts`.
- `crates/zbrain-core/src/calibration/nudge.ts` — E7 real-time pattern surfacing.
  `evaluateAndFireNudge(opts)` is the full pipeline: threshold check
  (conviction > 0.7, holder match, slug-derived domain hint matches
  active bias tag), cooldown probe (14d via take_nudge_log), fire +
  log. STDERR-only output for v0.36.1.0; multi-channel deferred.
- `crates/zbrain-core/src/calibration/take-forecast.ts` — E5 Brier-trend at write
  time. Pure math over existing `TakesScorecard`; no LLM. Returns
  `predicted_brier`, `bucket_n`, `overall_brier`. Insufficient-data
  branch at `MIN_BUCKET_N = 5`. `batchForecast` memoizes per
  (holder, domain) tuple.
- `crates/zbrain-core/src/calibration/gstack-coupling.ts` — E4 outcome-driven
  learnings coupling. `writeIncorrectResolution(opts)` shells out to
  `gstack-learnings-log` binary. Config gate
  (`cycle.grade_takes.write_gstack_learnings`, default false for
  external users). Namespace prefix `zbrain:calibration:v0.36.1.0:` so
  `--undo-wave` can scrub.
- `crates/zbrain-core/src/calibration/svg-renderer.ts` — D23 server-rendered SVG for
  the admin SPA Calibration tab. Pure functions: data → SVG string.
  Inlines design tokens; XSS-safe via `escapeXml()`. Four chart
  renderers: `renderBrierTrend`, `renderDomainBars`,
  `renderAbandonedThreadsCard`, `renderPatternStatementsCard`. SPA
  renders via `<TrustedSVG>` wrapper behind `requireAdmin`.
- `crates/zbrain-core/src/calibration/undo-wave.ts` — D18 CDX-3 resolution. `undoWave`
  reverses the wave's mutations: unsets `takes.resolved_*` for
  wave-applied resolutions (cross-checks resolved_by so manual writes
  persist), deletes calibration_profiles, purges nudge logs, marks
  grade-cache rows applied=false. `--dry-run` shows counts without
  writing. Idempotent on wave_version match.
- `crates/zbrain-core/src/calibration/think-ab.ts` — D19 A/B harness. `runAbTrial`
  calls thinkRunner twice (baseline + with-calibration), records
  preference to `think_ab_results`. `buildAbReport` aggregates over
  30-day window; flags `calibration_net_negative` when n>=20 + win
  rate < 45% on decisive trials.
- `crates/zbrain-core/src/calibration/recall-footer.ts` — formatter for the morning
  pulse calibration block. Cold-brain branch when <5 resolved. v0.36
  ship state: opt-in via the wiring layer; auto-on in v0.37+.
- `crates/zbrain-core/src/eval-contradictions/calibration-join.ts` — E3 cross-
  reference. `tagFindingWithCalibration(finding, profile)` returns
  bias-tag context for contradictions that match active patterns.
  Returns null when profile missing (R2 regression — output
  byte-identical to v0.32.6).
- `crates/zbrain-core/src/think/prompt.ts` extension — E1 anti-bias rewrite.
  `withCalibration` option on `buildThinkSystemPrompt` adds anti-bias
  rules. New `buildCalibrationBlock()` emits the `<calibration>` XML.
  `buildThinkUserMessage` has TWO shapes: default (question first) for
  R1 regression, with-calibration (retrieval → calibration → question
  per D22) when opt-in. Wired into `runThink` via
  `opts.withCalibration` + `opts.calibrationHolder`.
- `crates/zbrain-cli/src/calibration.ts` — CLI: `zbrain calibration` (read +
  print), `--regenerate`, `--undo-wave <ver>` (T17), `ab-report` (T18).
  MCP op `get_calibration_profile` (scope: read) backs the same data
  path. Source-scoped via `sourceScopeOpts(ctx)`.
- `crates/zbrain-cli/src/serve-http.ts` extension — three new admin routes:
  `/admin/api/calibration/profile`, `/admin/api/calibration/charts/:type`
  (image/svg+xml; type in {brier-trend, domain-bars,
  pattern-statements, abandoned-threads}), and
  `/admin/api/calibration/pattern/:id` (TD3 drill-down).
- `crates/zbrain-cli/src/takes.ts` extension — `zbrain takes revisit <slug>`
  (TD4 / D30) opens $EDITOR on the source page with a
  `<!-- zbrain:revisit -->` cursor marker.
- `crates/zbrain-cli/src/doctor.ts` extension — 4 new checks: `abandoned_threads`,
  `calibration_freshness`, `grade_confidence_drift` (CDX-11 mitigation
  surface; math arrives v0.37+), `voice_gate_health`.
- `admin/src/pages/Calibration.tsx` — Calibration tab. Single-column
  Linear-calm-clarity layout matching the approved variant-B mockup.
  `<TrustedSVG>` wrapper handles `dangerouslySetInnerHTML` for the
  server-rendered SVG.
- `admin/src/index.css` extension — `--text-muted: #555 → #777` (TD2,
  WCAG AA contrast bump from 4.0 to ~5.5 on the #0a0a0f bg).
- `tests/unit/fixtures/calibration/extract-takes-corpus/` — synthetic prompt-
  tuning corpus. v0.36.1.0 ships 5 representative pages; full 50-page
  + 10-page holdout generated by `zbrain calibration build-corpus`
  (v0.37+ subcommand). All anonymized per CLAUDE.md placeholder list.
- `scripts/check-synthetic-corpus-privacy.sh` — CDX-14 mitigation. CI
  guard in `bun run verify`. Greps for explicit dollar amounts +
  verifies non-essay fixtures reference at least one placeholder name.
- `tests/unit/regressions/v0.36.1.0-iron-rule.test.ts` — R1-R5 regression
  inventory test file. Pins all 5 IRON-RULE regressions in one place
  for future bisects.
- `DESIGN.md` — repo-root design system. Formalizes the de facto admin
  tokens that landed v0.26.0. Calibration target for future
  `/plan-design-review` and `/design-review`.

## Thin-client routing (v0.31.1, Issue #734)

`zbrain init --mcp-only` (v0.29.2) sets up a thin-client install: no local
brain content, just an OAuth client pointing at a remote `zbrain serve --http`.
v0.29.2/v0.30.0 only refused 9 obvious local-only commands; the other ~25
silently fell through to `connectEngine()` and opened the empty local PGLite,
returning "No results." against a populated remote brain. v0.31.1 fixes the
silent-empty-results bug class for every operation surface.

Key files:

- `bin/zbrain-rs.js` — Routing seam INSIDE the existing op-dispatch path (CDX-1: no
  parallel `crates/zbrain-core/src/thin-client/` module; routing is a ~80-line conditional
  in `runThinClientRouted`). Detects `isThinClient(cfg)` BEFORE `connectEngine`
  so thin-client installs never open the empty PGLite. localOnly ops on
  thin-client refuse via `refuseThinClient` (with pinpoint hint table
  `THIN_CLIENT_REFUSE_HINTS`). Banner via `printIdentityBannerBestEffort`
  before each routed call (suppressed by `--quiet`, `ZBRAIN_NO_BANNER=1`,
  non-TTY default). Exhaustive TS `never` switch on `RemoteMcpError.reason`
  for canned, actionable error messages. ENG-2 renderer parity: local-engine
  path runs `JSON.parse(JSON.stringify(result))` so renderers see the same
  shape on both paths (kills Date/bigint/Buffer drift class).
- `crates/zbrain-core/src/mcp-client.ts` — `callRemoteTool(config, toolName, args, opts)`.
  Hardened in v0.31.1 (CDX-4): all transport errors normalized to
  `RemoteMcpError` via the `toRemoteMcpError` funnel. New `CallRemoteToolOptions
  {timeoutMs, signal}`; `buildAbortController` composes external signal with
  timeout. New `RemoteMcpErrorReason` stable union, `RemoteMcpErrorDetail.kind`
  ('timeout' | 'aborted' | 'unreachable') sub-tag, `RemoteMcpErrorDetail.code`
  field carrying server-supplied error codes (e.g. `missing_scope`).
  `extractToolErrorCode` parses JSON envelopes first, falls back to substring
  detection for legacy server messages. `unpackToolResult<T>(res)` unchanged
  (parses tool-call JSON content). `_clearMcpClientTokenCache()` test escape.
- `crates/zbrain-core/src/cli-options.ts` — `parseGlobalFlags` adds `--timeout=Ns` (accepts
  `30s`, `2m`, `500ms`, plain ms). Default `null` = per-command default (30s
  for most ops, 180s for `think`). `parseTimeout(s)` exported helper.
- `crates/zbrain-core/src/doctor-remote.ts` — `zbrain remote doctor` adds the
  `oauth_client_scopes_probe` check (CDX-5). Probes the read tier via
  `get_brain_identity` and admin tier via `get_health`; reports per-tier
  status with pinpoint remediation when admin is missing. `buildScopeCheck`
  + `ScopeProbeResult` exported for test access. Skippable via
  `ZBRAIN_DOCTOR_SKIP_SCOPE_PROBE=1` for fixtures that mock /mcp at JSON-RPC
  initialize level only (MCP SDK Client hangs on shape mismatch).
- `crates/zbrain-core/src/ssrf-validate.ts` (v0.36 Commit 0) — DNS-rebinding-defended URL validation. `validateAndResolveUrl(url)` resolves the hostname via `dns.lookup({all: true, family: 0})`, checks EVERY A AND AAAA record against the internal-IP deny list, returns the resolved IP so callers fetch by IP (defeats DNS rebinding: validation IP === fetch IP). `fetchWithSSRFGuard(url, opts)` does redirect-aware fetching with per-hop re-validation, max 3 hops by default. Reusable across all URL-fetching features. Test seam `__setDnsLookupForTests` for hermetic tests.
- `crates/zbrain-core/src/search/query-intent.ts` extension (v0.36 cross-modal wave) — new `suggestedModality: 'text' | 'image' | 'both'` axis on `QuerySuggestions`. Module-scope `CROSS_MODAL_PATTERNS` regex array (compiles once at module load). `isAmbiguousModalityQuery(query)` heuristic gate fires when a visual noun + reference marker combination indicates genuinely ambiguous routing — used by the Commit 4 LLM tie-break to bound LLM calls to <1% of queries.
- `crates/zbrain-core/src/search/mode.ts` extension (v0.36 cross-modal wave) — `ModeBundle` extended with 7 cross-modal knobs: `cross_modal_both_text_weight` / `cross_modal_both_image_weight` (D6 weighted RRF for `'both'` mode, defaults 0.6/0.4), `image_query_text_refinement_weight` / `image_query_image_refinement_weight` (D13 hybrid intersect for `searchByImage` query refinement, defaults 0.4/0.6), `unified_multimodal` + `unified_multimodal_only` (Phase 3 unified column routing flags), `cross_modal_llm_intent` (Commit 4 opt-in escalation). `SEARCH_MODE_CONFIG_KEYS` extended with 7 corresponding config keys. `KNOBS_HASH_VERSION` bumped 2→3 (D2 — closes the silent cache-hit class where a cached text-mode result could leak to an image-mode caller).
- `crates/zbrain-core/src/search/hybrid.ts` extension (v0.36 cross-modal wave) — cross-modal routing branch at the embed step. Resolves `effectiveModality` from per-call `opts.crossModal` (normalized: literal `'auto'` → undefined per D22-1) → `suggestions.suggestedModality` → `'text'` default. Image route: `embedQueryMultimodal` + `searchVector({embeddingColumn: 'embedding_image'})`, skip expansion + keyword (D9 mode-bundle override). 'both' route: parallel text + image vector searches merged via `rrfFusionWeighted` with `effectiveRrfK(baseRrfK, weight)` from the configured cross-modal weights. Phase 3 unified routing fires when `cfg.search.unified_multimodal === true` — bypasses dual-column branching, runs `embedQueryMultimodal` + `searchVector({embeddingColumn: 'embedding_multimodal'})`, D8 fail-open on zero rows + not strict-mode falls through to dual-column. Commit 4 LLM escalation fires only when (no explicit per-call opt) AND (regex returned 'text') AND (`cfg.search.cross_modal.llm_intent` is true) AND (`isAmbiguousModalityQuery` returns true). Fail-open on every error.
- `crates/zbrain-core/src/search/image-loader.ts` (v0.36 Phase 2) — `loadImageInput(input, opts)` accepts local path, `data:` URI, or `http(s)://` URL. Magic-byte sniff for PNG/JPEG/WebP. Hard size cap (default 10 MB, configurable via `search.image_query.max_bytes`). For URLs: routes through `fetchWithSSRFGuard` so DNS rebinding + redirect chains are defeated. Pre-flight Content-Length check + post-fetch size guard for lying servers. `ImageLoadError` with discriminated `code` (INVALID_FORMAT / OVERSIZED / INVALID_URL / FETCH_FAILED / TIMEOUT / SSRF_BLOCKED / NOT_FOUND).
- `crates/zbrain-core/src/search/by-image.ts` (v0.36 Phase 2) — `searchByImage(engine, input, opts)`. Always runs image branch (`embedQueryMultimodalImage` + `searchVector(embedding_image)`). D13 hybrid intersect: when caller provides optional `query`, runs parallel text branch via `embedQueryMultimodal(query)` and merges via `rrfFusionWeighted` with weights from resolved mode. Phase 3 widens to unified column once `search.unified_multimodal=true` (transparently upgrades the retrieval quality post-reindex).
- `crates/zbrain-core/src/spend-log.ts` (v0.36 Phase 2 D23-#6) — per-OAuth-client paid-API spend tracking against the `mcp_spend_log` table (migration v74). `checkBudget(engine, clientId, capCents)` is the pre-flight gate; throws `BudgetExceededError` when today's spend has hit the cap. `recordSpend(engine, entry)` is best-effort post-call. UTC day-aligned aggregation so caps roll over deterministically regardless of server timezone. Local CLI callers (no clientId) bypass the gate. Pre-v0.36 brains without the table fail open to spend=0. `VOYAGE_MULTIMODAL_3_PER_IMAGE_CENTS` = 0.12 cents per image embed.
- `crates/zbrain-core/src/search/llm-intent.ts` (v0.36 Commit 4) — opt-in LLM tie-break. `classifyModalityWithLLM(query, fallback)` routes through `gateway.chat()` with a fixed single-word-output system prompt. 1s timeout via AbortController. `parseModality(raw, fallback)` is the pure parser — tolerates trailing punctuation + casing. Fail-open on every error (gateway unavailable, timeout, parse failure, unrecognized output) — returns fallback so a misbehaving LLM can never break search. Cost-bounded by the ambiguity heuristic in `query-intent.ts` (fires <1% of queries when on).
- `crates/zbrain-cli/src/reindex-multimodal.ts` (v0.36 Phase 3) — `zbrain reindex --multimodal [--limit N] [--dry-run] [--cost-estimate] [--no-embed] [--yes] [--json]`. Walks `content_chunks WHERE embedding_multimodal IS NULL`, batches via `embedMultimodalSafe` (Commit 0 partial-failure-aware), persists. D7 lock acquisition via `tryAcquireDbLock('zbrain-reindex-multimodal', 360min)`. Cost prompt + 10s Ctrl-C grace window in TTY. `ZBRAIN_NO_REEMBED=1` bypass. Checkpoint at `~/.zbrain/reindex-multimodal-checkpoint.json` for resume. D23-#2 auto-flip prompt at coverage=100% completion (TTY: interactive; non-TTY: stderr hint with paste-ready command).
- `crates/zbrain-core/src/backfill-registry.ts` extension (v0.36) — new `modality` backfill kind. SQL filter requires `chunk_source='image_asset'` AND `embedding_image IS NOT NULL` AND `(modality IS NULL OR modality != 'image')`. D22-7 defensive guard: never flag a non-image chunk that happens to have `embedding_image` populated. Idempotent — second run finds zero rows.
- `crates/zbrain-core/src/migrate.ts` v74 (`mcp_spend_log`) + v75 (`embedding_multimodal_column`) — Phase 2 spend-log table + Phase 3 unified column ALTER. v75 is column-only (no HNSW index — deferred to post-reindex per pgvector best practice). v74 uses BTREE on `(client_id, created_at)` + `(token_name, created_at)` — `date_trunc('day', TIMESTAMPTZ)` is NOT IMMUTABLE so can't appear in index expressions; range scan on created_at covers the per-day rollup query.
- `crates/zbrain-core/src/operation.rs` — `get_brain_identity` op (read scope, no params,
  banner-only): cheap counter packet `{version, engine, page_count,
  chunk_count, last_sync_iso}` for the thin-client identity banner. Reuses
  `engine.getStats()`; banner's 60s client-side TTL bounds frequency to
  ≤1/60s per CLI process (well below the Fly.io health-check cadence that
  motivated the original `getStats` cost warning).
- `crates/zbrain-cli/src/{salience,anomalies,graph-query,think}.ts` — Per-command
  thin-client routing branches. These commands bypass the operation-layer
  dispatch in cli.ts (call `engine.foo()` directly), so each gets its own
  `if (isThinClient(cfg)) { callRemoteTool(...) }` branch that maps CLI flags
  to op params. `think` is a special case: the server's `think` op
  intentionally disables `--save`/`--take` for remote callers
  (operations.ts:1103-1135 trust-boundary gate); thin-client `think` warns
  loudly when those flags are set.

## Schema Cathedral v3 (v0.40.7.0)

The schema-pack mutation surface shipped in v0.40.7.0 as the production rebuild of
closed community PR #1321 (`@garrytan-agents`). Six new foundation modules + a
mutate skeleton + stats/sync data plane + 14 CLI verbs + 9 MCP ops + a first-class
agent skill. See `~/.claude/plans/system-instruction-you-are-working-recursive-thacker.md`
for the full plan + 21 captured design decisions.

Key files (v0.40.7.0 additions):
- `crates/zbrain-core/src/schema-pack/pack-lock.ts` — Atomic `O_CREAT|O_EXCL` per-pack lock. DELIBERATELY NOT the `existsSync + writeFileSync` TOCTOU shape from `crates/zbrain-core/src/page-lock.ts` (codex C8 caught the bug class). Default 60s TTL, refresh every 10s while `withPackLock(fn)` runs, `--force` semantics = "steal stale lock" NOT "skip locking." Lock path per-pack so two packs never block each other.
- `crates/zbrain-core/src/schema-pack/mutate-audit.ts` — ISO-week JSONL at `~/.zbrain/audit/schema-mutations-YYYY-Www.jsonl`. Privacy-redacted per D20: type names → sha8, prefixes → first slug segment only, matches `candidate-audit.ts` privacy posture. Logs BOTH success AND failure events so the v0.40.7+ `schema_pack_writability` doctor check has signal to read. `summarizeMutations()` is the cross-surface parity primitive.
- `crates/zbrain-core/src/schema-pack/registry.ts` extensions — `invalidatePackCache(name?)` walks the extends-chain reverse-graph (codex C6 fix; pre-v0.40.7, editing a parent pack silently left children stale). `tryCachedPack(name)` TTL-gated fast path: inside `STAT_TTL_MS` (default 1000ms, env `ZBRAIN_PACK_STAT_TTL_MS`) returns cached without statting. Outside the window: stats every file in the chain; cascade-invalidates on mtime change (D11 cross-process detection).
- `crates/zbrain-core/src/schema-pack/best-effort.ts` — `loadActivePackBestEffort(ctx)` returns `ResolvedPack | null`. Single source of truth for the T1.5 wiring sites. `null` means EMPTY FILTER (NOT hardcoded defaults — D4 contract closing the silent-violation bug class).
- `crates/zbrain-core/src/schema-pack/lint-rules.ts` — 11 pure rule functions. `withMutation`'s pre-write validation gate composes the 9 file-plane rules; the 2 DB-aware rules (`extractable_empty_corpus`, `mutation_count_anomaly`) need an engine. Single source of truth consumed by CLI lint + MCP `schema_lint` + the pre-write validation gate.
- `crates/zbrain-core/src/schema-pack/query-cache-invalidator.ts` — `invalidateQueryCache(engine, sourceId?)` DELETEs query_cache rows so cached search results bound to old page types don't survive a schema mutation. Codex C9 fix.
- `crates/zbrain-core/src/schema-pack/mutate.ts` — 8-step `withMutation` skeleton (bundled-guard → lock → read → mutator → validate → atomic write → audit → invalidate). 11 mutation primitives: `addTypeToPack`, `removeTypeFromPack` (with codex C14 reference check), `updateTypeOnPack`, `addAliasToType`, `removeAliasFromType`, `addPrefixToType`, `removePrefixFromType`, `addLinkTypeToPack`, `removeLinkTypeFromPack`, `setExtractableOnType`, `setExpertRoutingOnType`. Atomic write via `.tmp + fsync + rename` — pack file on disk is NEVER partial. Inline minimal JSON→YAML emitter so YAML packs stay YAML (does NOT preserve comments — pin pack.json if you care about layout).
- `crates/zbrain-core/src/schema-pack/stats.ts` — `runStatsCore(engine, opts)` returns per-source + aggregate page counts + coverage % + `dead_prefixes` (declared prefixes with zero matching pages — agent's drilldown signal). Multi-source aware (`sourceIds[]` federated, `sourceId` single, or whole-brain). PGLite + Postgres parity via `executeRaw`. Empty brain → coverage:1.0 (vacuous truth).
- `crates/zbrain-core/src/schema-pack/sync.ts` — `runSyncCore(engine, opts)` chunked UPDATE in 1000-row batches per declared prefix (D14). Concurrent writers never block on a single row >100ms. Codex C5 write-side scoping via `ctx.sourceId` directly (NOT `sourceScopeOpts` which inherits OAuth read federation). Idempotent on `--apply` re-run.
- `crates/zbrain-cli/src/schema.ts` extension — 14 new CLI verbs in the dispatch table: `add-type`, `remove-type`, `update-type`, `add-alias`, `remove-alias`, `add-prefix`, `remove-prefix`, `add-link-type`, `remove-link-type`, `set-extractable`, `set-expert-routing`, `stats`, `sync`, `reload`. `withConnectedEngine` defensive fix from closed PR #1321 retained. Lifecycle-grouped help text (Inspection / Activation / Authoring / Discovery+repair).
- `crates/zbrain-core/src/operation.rs` extension — 9 new MCP ops: `get_active_schema_pack`, `list_schema_packs`, `schema_stats`, `schema_lint`, `schema_graph`, `schema_explain_type`, `schema_review_orphans` (all read-scope, NOT localOnly), plus `schema_apply_mutations` (**admin scope, NOT localOnly per D2** so remote agents like your OpenClaw can author packs over HTTPS MCP — batched per D10, one MCP tool taking an `mutations[]` array atomically inside ONE `withPackLock`, audit log captures `actor: mcp:<clientId8>`) and `reload_schema_pack` (admin, NOT localOnly). Trust posture: per-call `schema_pack` opt STAYS rejected for remote callers via `op-trust-gate.ts` (R2 regression preserved).
- `crates/zbrain-cli/src/whoknows.ts` + `crates/zbrain-core/src/operation.rs:find_experts` — T1.5 wiring sites. Pack-aware via `expertTypesFromPack(pack.manifest)` from `best-effort.ts`. Pack-load failure → EMPTY filter (NOT hardcoded `['person', 'company']` defaults per D4). A `researcher` type declared `--expert` now surfaces in `whoknows` results; pre-v0.40.7 it silently never matched.
- `skills/schema-author/SKILL.md` — Agent dispatcher for "evolve the schema pack." Triggers: 15+ phrasings including "add a page type", "my brain has untyped pages", "propose new types from my corpus", "backfill page types". Explicit Non-goals callout to `brain-taxonomist` (files one page) and `eiirp` (schema-check during iteration) so agents pick the right surface. 7-phase workflow: brain → assess → propose → apply → sync → verify → commit. Lists every zbrain schema CLI verb + every MCP op the skill uses. `brain_first: exempt` frontmatter. Required v0.40.7+ conformance sections: Contract, Anti-Patterns, Output Format.
- `skills/conventions/schema-evolution.md` — Canonical convention: "when to add a type vs alias vs prefix." Decision tree: <20 pages → don't pack-codify; 20-100 → alias or narrow prefix on existing type; 100+ → first-class type. Don'ts section + "when to remove a type" + "when to commit the pack" all answered in one place.
- `skills/RESOLVER.md` + `skills/manifest.json` — schema-author wired into the dispatcher with full functional-area trigger list (compressed routing pattern per v0.32.3 dispatcher convention).

T1.5 wiring is partial in v0.40.7.0. Three follow-ups filed in TODOS.md under
"v0.40.7.0 Schema Cathedral v3 follow-ups (v0.40.7+)" — enrichment-service.ts
union widening (`'person' | 'company'` → `string`), facts/eligibility.ts
pack-aware `ELIGIBLE_TYPES` wiring, and 3 doctor checks (schema_pack_coverage,
schema_pack_writability, schema_pack_mutation_audit).

## Commands

All commands run through the `zbrain` binary (or `node bin/zbrain-rs.js`). Run
`zbrain --help` for the live list. Subcommands (grouped):

**Project & health**
- `zbrain init` — scaffold a new ZBrain project.
- `zbrain doctor` — validate install + connectivity + calibration.
- `zbrain check-update` — check for new ZBrain versions.
- `zbrain config` — read/write config values.
- `zbrain features` — scan usage, recommend unused features.

**Knowledge base**
- `zbrain think` / `zbrain auto-think` — synthesize answers across the KB.
- `zbrain query` — keyword search.
- `zbrain capture` — capture content from files/stdin.
- `zbrain sync` — sync a git repo into the KB.
- `zbrain sources` / `zbrain facts` / `zbrain links` / `zbrain takes` — manage sources, facts, links, takes.
- `zbrain get-page` / `put-page` / `delete-page` / `restore-page` / `purge-deleted-pages` / `list-pages` — page CRUD.
- `zbrain salience` / `zbrain orphans` / `zbrain integrity` / `zbrain anomalies` — maintenance reads.
- `zbrain recall` — recall hot memory (facts) for an entity/session/window.
- `zbrain find-trajectory` / `zbrain find-contradictions` — entity trajectory + contradiction detection.
- `zbrain graph-query` — BFS graph traversal from a root page.
- `zbrain history` / `revert` / `tag` / `untag` / `tags` / `timeline` / `timeline-add` — page versioning.
- `zbrain whoami` / `zbrain whoknows` / `zbrain publish` / `zbrain storage` / `zbrain models` — identity, experts, publish, storage, model routing.

**Code intelligence**
- `zbrain code-def` / `code-refs` / `code-callers` / `code-callees` / `code-blast` / `code-flow` — symbol + call-graph queries.
- `zbrain search-by-image` — image-based page search.
- `zbrain book-mirror` — chapter-by-chapter book analysis.

**Servers & jobs**
- `zbrain serve` — HTTP API + admin SPA.
- `zbrain serve-mcp` — MCP stdio server.
- `zbrain autopilot` — self-maintaining brain daemon (interval maintenance cycles).
- `zbrain remote` — thin-client commands round-tripping through a remote MCP host.
- `zbrain jobs` — background job queue (submit/list/get/cancel/retry/prune/stats).
- `zbrain agent` — submit subagent jobs + view logs.
- `zbrain dream` — run one maintenance cycle (lint -> ... -> orphans).

**Schema / skills / migrations**
- `zbrain schema` — 32-verb schema-pack manager (inspect/validate/lint).
- `zbrain schema-sql` — print DB schema DDL for the selected backend.
- `zbrain apply-migrations` — run pending upgrade-migration orchestrators.
- `zbrain mounts` — manage connected brains (`mounts.json`).
- `zbrain skillpack` — install/scaffold/search/harvest skillpacks.
- `zbrain skillify` — scaffold a new skill.
- `zbrain reindex` — re-embed content to refresh the vector index.
- `zbrain resolvers` — introspect the Resolver SDK registry.
- `zbrain check-resolvable` / `check-brain-first` / `routing-eval` — skill-tree gates.
## Testing

### Test command tiers (Rust port — cargo)

The Rust port uses `cargo` as the single build/test truth source. The legacy
TS-era `bun run test` / `scripts/*.sh` gates were retired with the `scripts/`
directory (see `docs/plans/zbrain-legacy-retire-reassessment.json`).

| Command | What it runs | When to use |
|---|---|---|
| `cargo build --workspace` | Compile all crates (debug). | After any Rust change. |
| `cargo test -p <crate>` | Unit + integration tests for one crate. | Inner edit loop. Default. |
| `cargo test --workspace` | Full workspace test sweep. | Pre-merge / pre-push sanity. |
| `zbrain check-resolvable --strict` | Resolver drift gate on bundled skills. | After changing `skills/`. |

CI runs the cargo suite via `.github/workflows/rust-tests.yml`; there is no
longer a sharded `bun` matrix.

### CI vs local

The legacy sharded `bun` matrix (`.github/workflows/test.yml`) was retired and
replaced with a minimal `cargo build` + `cargo test` smoke gate; the full Rust
test suite runs in `.github/workflows/rust-tests.yml`. Cargo's own test
parallelism is the fast loop — there is no separate local/CI file-set divergence
to maintain anymore.

### Failure-first logging

When `bun run test` finds any failure, the wrapper:

1. Writes failure blocks (each prefixed with `--- shard N: <test name> ---`) to `.context/test-failures.log` (workspace-local, gitignored). On systems without a writable `.context/`, falls back to `/tmp/zbrain-test-failures.log`.
2. Prints a loud stderr banner with the absolute log path, plus the last 30 lines of the failure log inlined. Banner survives `| head` / `| tail` / agent-side log truncation.
3. Writes a one-line-per-shard summary to `.context/test-summary.txt` (`shard N/M: pass=X fail=Y skip=Z rc=W`).
4. Exits non-zero. Empty failure log + non-zero exit = infrastructure problem (wedged shard, killed child); the banner says so.

If a shard wedges (per-shard `ZBRAIN_TEST_SHARD_TIMEOUT` cap, default 600s), the wrapper writes `--- shard N: WEDGED after ${SHARD_TIMEOUT}s ---` to the failure log, includes the last 50 lines of the shard log, and proceeds with other shards' results.

### File taxonomy

- `*.test.ts` → fast loop (parallel 8-shard fan-out).
- `*.slow.test.ts` → run via `bun run test:slow` only (intentional cold-path tests; would dominate the fast loop's wallclock).
- `*.serial.test.ts` → run via `bun run test:serial` after the parallel pass completes; uses `--max-concurrency=1`. Quarantine for tests that share file-wide state and race when run alongside other files in the same `bun test` process. Currently: `tests/unit/brain-registry.serial.test.ts`, `tests/unit/reconcile-links.serial.test.ts`, `tests/unit/core/cycle.serial.test.ts`, `tests/unit/embed.serial.test.ts` (the latter two added in v0.26.7 — they use `mock.module(...)` which leaks across files in the shard process). **Do not put the parallelism back on a serial file unless you've fixed the contention root cause** (it just re-introduces the flake).
- `tests/unit/e2e/*.test.ts` → real-Postgres E2E. Skipped when `DATABASE_URL` is unset.
- `tests/heavy/*.sh` → ops-shape shell scripts. Cost minutes per run; NOT in default `bun test`. Run via `bun run test:heavy` or scheduled nightly via `.github/workflows/heavy-tests.yml`. Examples: pg_upgrade matrix (boot legacy brain → walk to head), RSS budget gate (measure peak worker RSS vs committed baseline), read-latency-under-sync (p50/p95/p99 under concurrent writer load), sync lock regression (N concurrent syncs assert 1 winner + N-1 lock-busy + zero leaked `zbrain_cycle_locks` rows). See `tests/heavy/README.md` for when to add a script here vs `*.slow.test.ts`. Files prefixed with `_` (e.g. `tests/heavy/_build_legacy_fixtures.sh`) are helpers/libs invoked by sibling tests — the runner skips them.
- `tests/unit/fuzz/*.test.ts` → property-based fuzz harness. Pure-validator targets in `pure-validators.test.ts` are guarded by `scripts/check-fuzz-purity.sh` (in `bun run verify`), which `bun build --target=bun` bundles each target and greps the resulting bundle for banned transitive imports (`node:fs`, `node:child_process`, engine modules). Anything that fails the guard moves to `mixed-validators.test.ts` (still property-tested, but no purity guarantee) or `filesystem-validators.test.ts` (fs-backed, uses temp dirs). Fuzz tests run in the default `bun test` loop because they're fast (~3s for ~12 properties × 1000 runs each).

The intra-file parallelism project (turn `bun test` into `bun test --concurrent` after sweeping shared-state contention sites) is sliced across v0.26.7 (foundation), v0.26.8 (env-mutation sweep), and v0.26.9 (PGLite sweep + codemod + measurement). v0.26.4 ships file-level parallelism only.

### Test-isolation lint and helpers (RETIRED)

The TS-era cross-file flake class was enforced by `scripts/check-test-isolation.sh`
(wired into `bun run verify` / `bun run check:all`). That script and the
`*.test.ts` suite it guarded were retired with the TS→Rust port. The Rust
port enforces isolation at the type/lifetime level instead; see
`crates/zbrain-core/tests/` for the cargo-based integration tests.

#### Canonical PGLite block (R3 + R4 compliant)

Every test file that needs a PGLite engine should use this exact pattern:

```ts
import { PGLiteEngine } from '../crates/zbrain-core/src/pglite-engine.ts';
import { resetPgliteState } from './helpers/reset-pglite.ts';

let engine: PGLiteEngine;

beforeAll(async () => {
  engine = new PGLiteEngine();
  await engine.connect({});
  await engine.initSchema();
});

afterAll(async () => {
  await engine.disconnect();
});

beforeEach(async () => {
  await resetPgliteState(engine);
});
```

Why this exact shape: `beforeAll` creates a single engine per file (PGLite WASM cold-start + initSchema is ~20s); `beforeEach` truncates user data via `resetPgliteState` ("two orders of magnitude faster" than fresh-engine-per-test); `afterAll` disconnects so the engine doesn't leak across file boundaries within a shard process.

#### `withEnv` pattern (R1 fix)

```ts
import { withEnv } from './helpers/with-env.ts';

test('reads OPENAI_API_KEY', async () => {
  await withEnv({ OPENAI_API_KEY: 'sk-test' }, async () => {
    expect(loadConfig().openai_key).toBe('sk-test');
  });
});

// Delete a var (override is undefined):
await withEnv({ ZBRAIN_HOME: undefined }, fn);

// Multiple keys:
await withEnv({ A: '1', B: '2', C: undefined }, fn);
```

`withEnv` saves the prior value of every key it touches and restores via try/finally — including when the callback throws. **It is cross-test safe but NOT intra-file concurrent-safe.** `process.env` is process-global; two `test.concurrent()` calls in the same file both touching the same key will race. Files using `withEnv` stay outside the future `test.concurrent()` codemod's eligibility filter.

#### When to quarantine instead of fix

Rename to `*.serial.test.ts` when:
- The file uses `mock.module(...)` (R2 — there's no clean fix without changing production code).
- The file is genuinely env-coupled (e.g. `zbrain-home-isolation.test.ts`, `claw-test-cli.test.ts`) — module-load env readers + ESM caching defeat dynamic-import-after-env tricks.
- The file's tests intentionally share state across `it()` boundaries.

Quarantine count cap: 10 (informational). Beyond that, push back on the design.

### Inventory (legacy)

`bun test` runs all tests. After the v0.12.1 release: ~75 unit test files + 8 E2E test files (1412 unit pass, 119 E2E when `DATABASE_URL` is set — skip gracefully otherwise). Unit tests run
without a database. E2E tests skip gracefully when `DATABASE_URL` is not set.

Unit tests: `tests/unit/markdown.test.ts` (frontmatter parsing), `tests/unit/chunkers/recursive.test.ts`
(chunking), `tests/unit/parity.test.ts` (operations contract
parity), `tests/unit/cli.test.ts` (CLI structure), `tests/unit/config.test.ts` (config redaction),
`tests/unit/files.test.ts` (MIME/hash), `tests/unit/import-file.test.ts` (import pipeline),
`tests/unit/upgrade.test.ts` (schema migrations),
`tests/unit/file-migration.test.ts` (file migration), `tests/unit/file-resolver.test.ts` (file resolution),
`tests/unit/import-resume.test.ts` (import checkpoints), `tests/unit/migrate.test.ts` (migration; v8/v9 helper-btree-index SQL structural assertions + 1000-row wall-clock fixtures that guard the O(n²)→O(n log n) fix + v0.13.1 assertions on v12/v13 SQL shape, `sqlFor` + `transaction:false` runner semantics, the `max_stalled DEFAULT 1` regression guard, and v0.22.6.1 v24 `sqlFor.pglite: ''` no-op assertion),
`tests/unit/bootstrap.test.ts` (v0.22.6.1 — bootstrap contract: no-op on fresh install, idempotent across two `initSchema()` calls, no-op on modern brain that already has every probed column, full bootstrap path on simulated pre-v0.18 brain, fresh-install regression guard, pre-v0.13 `links` shape coverage),
`tests/unit/schema-bootstrap-coverage.test.ts` (v0.22.6.1 CI guard — `REQUIRED_BOOTSTRAP_COVERAGE` lists every forward reference in PGLITE_SCHEMA_SQL; the test fails loudly if `applyForwardReferenceBootstrap` skips one. When you add a column-with-index to the embedded schema blob, you extend both arrays or this guard fails. The pattern that broke zbrain ten times in two years is now structurally prevented. **v0.35.5.0:** test now also parses `crates/zbrain-core/src/migrate.ts` source text for every `ALTER TABLE ... ADD COLUMN` (top-level `sql:`, `sqlFor.{postgres,pglite}` overrides, AND handler-body `engine.runMigration(N, \`ALTER TABLE ...\`)`), and asserts each (table, column) pair is covered by the bootstrap OR by the schema blob's CREATE TABLE bodies. Catches the column-only forward-reference class (e.g. `sources.archived` shape from v0.26.5, `oauth_clients.source_id` from v0.34.1) that the pre-existing CREATE INDEX parser couldn't see. Pre-existing parser bug fixed in same wave: `parseBaseTableColumns` now strips SQL line + block comments before identifying column names so commented-out lines no longer hide adjacent columns from coverage.),
`tests/unit/helpers/schema-diff.ts` + `tests/unit/helpers/schema-diff.test.ts` + `tests/unit/e2e/schema-drift.test.ts` (v0.26.6 #588 — cross-engine schema parity gate. Helper exports pure `snapshotSchema(query)` / `diffSnapshots(pg, pglite, opts)` / `formatDiffForFailure(diff)` / `isCleanDiff(diff)` over a four-tuple per column (`data_type`, `udt_name`, `is_nullable`, `column_default`). E2E test spins up fresh PGLite + Postgres, runs `engine.initSchema()` on each (bootstrap + schema replay + migrations), snapshots `information_schema.columns`, then diffs. 2-table allowlist (`files`, `file_migration_ledger`) — every other Postgres table must reach PGLite via PGLITE_SCHEMA_SQL or a migration's `sqlFor.pglite` branch. Sentinels for `oauth_clients`, `mcp_request_log`, `access_tokens`, `eval_candidates` give tighter blame messages. Skip-gracefully without `DATABASE_URL`. Wired into `scripts/e2e-test-map.ts` so changes to `src/schema.sql`, `crates/zbrain-core/src/pglite-schema.ts`, or `crates/zbrain-core/src/migrate.ts` trigger it. The failure message names every drift with a paste-ready hint pointing at `crates/zbrain-core/src/pglite-schema.ts`.),
`tests/unit/setup-branching.test.ts` (setup flow), `tests/unit/slug-validation.test.ts` (slug validation),
`tests/unit/storage.test.ts` (storage backends), `tests/unit/supabase-admin.test.ts` (Supabase admin),
`tests/unit/yaml-lite.test.ts` (YAML parsing), `tests/unit/check-update.test.ts` (version check + update CLI),
`tests/unit/pglite-engine.test.ts` (PGLite engine, all 40 BrainEngine methods including 11 cases for `addLinksBatch` / `addTimelineEntriesBatch`: empty batch, missing optionals, within-batch dedup via ON CONFLICT, missing-slug rows dropped by JOIN, half-existing batch, batch of 100 + v0.13.1 `connect()` error-wrap assertion (original error nested, #223 link in message, lock released)),
`tests/unit/engine-factory.test.ts` (engine factory + dynamic imports),
`tests/unit/integrations.test.ts` (recipe parsing, CLI routing, recipe validation),
`tests/unit/publish.test.ts` (content stripping, encryption, password generation, HTML output),
`tests/unit/backlinks.test.ts` (entity extraction, back-link detection, timeline entry generation),
`tests/unit/lint.test.ts` (LLM artifact detection, code fence stripping, frontmatter validation),
`tests/unit/report.test.ts` (report format, directory structure),
`tests/unit/skills-conformance.test.ts` (skill frontmatter + required sections validation),
`tests/unit/resolver.test.ts` (RESOLVER.md coverage, routing validation + v0.20.4 round-trip: every quoted RESOLVER.md trigger must match a frontmatter `triggers:` entry in the target skill, and every `name="<word>"` reference in any SKILL.md must resolve to a declared op in `crates/zbrain-core/src/operation.rs` or a Minions handler in `PROTECTED_JOB_NAMES`),
`tests/unit/search.test.ts` (RRF normalization, compiled truth boost, cosine similarity, dedup key),
`tests/unit/sql-ranking.test.ts` (v0.22.0 source-boost helpers: 39 cases covering longest-prefix-match in SQL CASE, detail=high temporal-bypass, three-meta-char LIKE escape (%, _, \\), single-quote SQL-literal doubling, env override parsing for ZBRAIN_SOURCE_BOOST + ZBRAIN_SEARCH_EXCLUDE, resolveBoostMap / resolveHardExcludes merge semantics),
`tests/unit/dedup.test.ts` (source-aware dedup, compiled truth guarantee, layer interactions),
`tests/unit/intent.test.ts` (query intent classification: entity/temporal/event/general),
`tests/unit/eval.test.ts` (retrieval metrics: precisionAtK, recallAtK, mrr, ndcgAtK, parseQrels),
`tests/unit/check-resolvable.test.ts` (resolver reachability, MECE overlap, gap detection, DRY checks + v0.14.1 proximity-based DRY detection + `extractDelegationTargets` coverage — 13 DRY cases),
`tests/unit/dry-fix.test.ts` (v0.14.1 auto-fix: three shape-aware expander pure-function tests, five guards — working-tree-dirty, no-git-backup, inside-code-fence, already-delegated within 40 lines, ambiguous-multi-match, block-is-callout — 28 cases),
`tests/unit/doctor-fix.test.ts` (v0.14.1 `zbrain doctor --fix` CLI integration: dry-run preview, apply path, JSON output shape — 3 cases),
`tests/unit/backoff.test.ts` (load-aware throttling, concurrency limits, active hours),
`tests/unit/fail-improve.test.ts` (deterministic/LLM cascade, JSONL logging, test generation, rotation),
`tests/unit/transcription.test.ts` (provider detection, format validation, API key errors),
`tests/unit/enrichment-service.test.ts` (entity slugification, extraction, tier escalation),
`tests/unit/data-research.test.ts` (recipe validation, MRR/ARR extraction, dedup, tracker parsing, HTML stripping),
`tests/unit/minions.test.ts` (Minions job queue v7: CRUD, state machine, backoff, stall detection, dependencies, worker lifecycle, lock management, claim mechanics, depth/child-cap, timeouts, cascade kill, idempotency, child_done inbox, attachments, removeOnComplete/Fail + v0.13.1 `max_stalled` clamp/default/plumbing coverage),
`tests/unit/extract.test.ts` (link extraction, timeline extraction, frontmatter parsing, directory type inference),
`tests/unit/extract-db.test.ts` (zbrain extract --source db: typed link inference, idempotency, --type filter, --dry-run JSON output),
`tests/unit/extract-fs.test.ts` (zbrain extract --source fs: first-run inserts + second-run reports zero, dry-run dedups candidates across files, second-run perf regression guard — the v0.12.1 N+1 dedup bug),
`tests/unit/link-extraction.test.ts` (canonical extractEntityRefs both formats, extractPageLinks dedup, inferLinkType heuristics, parseTimelineEntries date variants, isAutoLinkEnabled config),
`tests/unit/graph-query.test.ts` (direction in/out/both, type filter, indented tree output),
`tests/unit/features.test.ts` (feature scanning, brain_score calculation, CLI routing, persistence),
`tests/unit/file-upload-security.test.ts` (symlink traversal, cwd confinement, slug + filename allowlists, remote vs local trust),
`tests/unit/query-sanitization.test.ts` (prompt-injection stripping, output sanitization, structural boundary),
`tests/unit/search-limit.test.ts` (clampSearchLimit default/cap behavior across list_pages and get_ingest_log),
`tests/unit/repair-jsonb.test.ts` (v0.12.2 JSONB repair: TARGETS list, idempotency, engine-awareness),
`tests/unit/migrations-v0_12_2.test.ts` (v0.12.2 orchestrator phases: schema → repair → verify → record),
`tests/unit/markdown.test.ts` (splitBody sentinel precedence, horizontal-rule preservation, inferType wiki subtypes),
`tests/unit/orphans.test.ts` (v0.12.3 orphans command: detection, pseudo filtering, text/json/count outputs, MCP op),
`tests/unit/postgres-engine.test.ts` (v0.12.3 statement_timeout scoping: `sql.begin` + `SET LOCAL` shape, source-level grep guardrail against reintroduced bare `SET statement_timeout`),
`tests/unit/sync.test.ts` (sync logic + v0.12.3 regression guard asserting top-level `engine.transaction` is not called),
`tests/unit/sync-concurrency.test.ts` (v0.22.13 PR #490: 17 cases covering `autoConcurrency()` thresholds + PGLite-forces-serial + explicit-override clamping, `shouldRunParallel()` Q1 explicit-bypasses-floor contract, and `parseWorkers()` validation that rejects `'0'`/`'-3'`/`'foo'`/`'1.5'`/trailing chars),
`tests/unit/sync-parallel.test.ts` (v0.22.13 PR #490: PGLite-routed coverage of the bookmark gate under concurrency request, head-drift gate, vanished-file failure capture, PGLite-stays-serial, and the `zbrain-sync` writer-lock contract — 7 cases),
`tests/unit/sync-failures.test.ts` (v0.22.12: 28 cases pinning `classifyErrorCode` regex coverage for all 12 codes against literal production message strings from `markdown.ts:159-244` and `import-file.ts:199, 347, 352, 401`; `summarizeFailuresByCode` sort + pre-classified-honor; `recordSyncFailures` code-field persistence; `acknowledgeSyncFailures` AcknowledgeResult shape + backfill on pre-v0.22.12 entries),
`tests/unit/doctor.test.ts` (doctor command + v0.12.3 assertions that `jsonb_integrity` scans the four v0.12.0 write sites and `markdown_body_completeness` is present),
`tests/unit/utils.test.ts` (shared SQL utilities + `tryParseEmbedding` null-return and single-warn semantics),
`tests/unit/build-llms.test.ts` (llms.txt/llms-full.txt generator: path resolution, idempotence, spec shape, regen-drift guard, content contract, AGENTS.md install-path mirror, size-budget enforcement — 7 cases),
`tests/unit/oauth.test.ts` (v0.26.0 OAuth 2.1 provider — 27 cases: register, getClient, `client_credentials` grant exchange, `authorization_code` flow with PKCE challenge / verifier, refresh token rotation, `verifyAccessToken` with both OAuth + legacy `access_tokens` fallback, `revokeToken`, `sweepExpiredTokens`, and a contract test asserting `scope` + `localOnly` annotations are set correctly on all 30 operations; **v0.26.2** adds 5 `coerceTimestamp` unit cases (null/undefined/string/number/throw-on-NaN), NULL-`expires_at`-as-expired contract tests for both refresh + access token paths, and a cascade-delete contract test asserting `revoke-client` purges `oauth_tokens` + `oauth_codes` rows via FK CASCADE; **v0.26.9** adds 14 cases pinning the F1/F2/F3/F4/F5/F6/F7c/F12 invariants, including the F1/F4 cross-client isolation pattern (wrong-client attempt MUST reject AND rightful owner MUST still succeed atomically afterward) and the empty-string `redirect_uri` bypass guard surfaced during adversarial review),
`tests/unit/mcp-dispatch-summarize.test.ts` (v0.26.9 — 7 cases pinning F8 `summarizeMcpParams` invariants: declared-keys allow-list intersection, attacker-key-name leak guard (unknown keys counted not named), 1KB byte bucketing for size-probe defense, missing op falls through to fully-redacted shape, declared-keys sorted for deterministic output),
`tests/unit/trust-boundary-contract.test.ts` (v0.26.9 — 4 cases pinning F7b fail-closed semantics under cast bypass: `ctx.remote === undefined` treated as remote/untrusted at every flipped call site, `as any` and `Partial<>` spreads can't downgrade trust by accident),
`tests/unit/check-resolvable-cli.test.ts` (v0.19 CLI wrapper: exit codes, JSON envelope shape, AGENTS.md fallback chain),
`tests/unit/regression-v0_16_4.test.ts` (findRepoRoot regression guard — hermetic startDir parameterization),
`tests/unit/repo-root.test.ts` (v0.16.4 / v0.19 / v0.31.7 — 20 cases: `findRepoRoot` walk semantics + default-arg parity, the 4-tier `autoDetectSkillsDir` fallback chain (`$OPENCLAW_WORKSPACE` → `~/.openclaw/workspace` → repo-root → `./skills`), W1 RESOLVER.md/AGENTS.md filename precedence, D-CX-4 explicit-env-wins-over-repo-root, and 8 new v0.31.7 D3+D5 cases pinning tier-0 `$ZBRAIN_SKILLS_DIR` valid/invalid/precedence-over-OPENCLAW_WORKSPACE, the install-path walk in `autoDetectSkillsDirReadOnly`, no-drift on primary success, `AUTO_DETECT_HINT` + `AUTO_DETECT_HINT_READ_ONLY` content, and the D5 regression guard asserting the shared `autoDetectSkillsDir` MUST NEVER return `'install_path'` source — that's how the read-path/write-path split stays safe),
`tests/unit/resolver-merge.test.ts` (v0.31.7 — 8 cases pinning the multi-file resolver merge: `findAllResolverFiles` empty / RESOLVER.md-only / AGENTS.md-only / both-present (RESOLVER.md first), and `checkResolvable` merge semantics across `skills/RESOLVER.md` + `../AGENTS.md` for the OpenClaw layout where the skillpack ships a thin RESOLVER.md and the real dispatcher lives at the workspace root — dedup by `skillPath` (first occurrence wins), AGENTS.md-at-workspace-root works alone, and the previously-unreachable 187/224 OpenClaw skills become reachable),
`tests/unit/filing-audit.test.ts` (v0.19 Check 6: `writes_pages` / `writes_to` frontmatter, filing-rules JSON validation),
`tests/unit/skill-brain-first.test.ts` (v0.37.1.0 — 56 cases: shared frontmatter parser, `analyzeSkillBrainFirst` compliance ladder across 9 fixtures under `tests/unit/fixtures/brain-first-skills/` (compliant-callout, compliant-phase, compliant-position, exempt-frontmatter, missing-brain-first, multi-pattern, negation-prose, no-external, typo-frontmatter), offset helpers, external-lookup regex shape, audit snapshot+diff transition logic, PR #1206 `FORMERLY_HARDCODED_EXEMPT` regression absorption),
`tests/unit/e2e/skill-brain-first.test.ts` (v0.37.1.0 — 12 E2E cases: doctor reports `skill_brain_first` check with structured issues; `--fix --dry-run` previews insertion without writing; `--fix` applies the canonical Convention callout idempotently; `brain_first: exempt` frontmatter resolves the warn; `brain_first_typo` surfaces paste-ready hint; audit JSONL records `detected` / `resolved` / `fixed` transitions; stable brain emits 0 audit lines/run),
`tests/unit/routing-eval.test.ts` (v0.19 Check 5: fixture parsing, structural routing, ambiguous_with, Haiku tie-break layer),
`tests/unit/skill-manifest.test.ts` (v0.19 skill manifest parser: drift detection, managed-block markers),
`tests/unit/skillify-scaffold.test.ts` (v0.19 `zbrain skillify scaffold` stubs: SKILL.md, script, tests, routing-eval fixtures),
`tests/unit/skillpack-install.test.ts` (v0.19 `zbrain skillpack install` managed-block install / update / no-clobber semantics),
`tests/unit/skillpack-sync-guard.test.ts` (v0.19 sync-guard: bundled skills stay byte-identical to `skills/` source),
`tests/unit/http-transport.test.ts` (v0.22.7 HTTP transport: 23 unit cases covering bearer auth + missing/no-Bearer/unknown/revoked + `/health` bypass, F1+F2 round-trip via dispatch.ts, F3 invalid_params, application/json response shape (not SSE), CORS default-deny + allowlist, body cap on Content-Length AND chunked, two-bucket rate limit (refill, exhaust+Retry-After, LRU eviction, TTL prune, pre-auth IP fires before DB), and `mcp_request_log` audit on success + auth_failed),
`tests/unit/restart-sweep.test.ts` (v0.28.3 — 27 bun:test cases for the `recipes/restart-sweep.md` inlined script: sentinel-anchored fenced-block extraction with salted tmp filenames to bypass ESM cache; constructor-time env reads (proves no module-load snapshot); idempotency layer load/save/atomic-tmp-rename/corrupt-JSON-recovery/30-day-prune; `(sessionKey, lastAlertedAt)` cooldown gate with 6h threshold (the C1 fix that survives synthesized restartTime); AGGRESSIVE-gate two-state tests; execFile argv shape proving shell metachars in `OPENCLAW_TELEGRAM_GROUP` cannot reach `/bin/sh`; real-`\n`-not-literal alert formatting; `ZBRAIN_HOME` state path override),
`tests/unit/eval-longmemeval.test.ts` (v0.28.8 LongMemEval harness — 12 hermetic cases with no `DATABASE_URL` and no API keys: PGLite create + reset over runtime-enumerated `pg_tables`, infrastructure-table preservation across resets, JSONL question parsing, retrieval-only and answer-gen modes via stubbed `ThinkLLMClient`, `--limit` cutoff, `--keyword-only` vs hybrid, default `--expansion=off` behavior, perf gate (p50 < 30ms / p99 < 50ms warm reset+import+search on Apple Silicon), `--help` works without a configured brain, fixture round-trip via `tests/unit/fixtures/longmemeval-mini.jsonl`),
`tests/unit/longmemeval-sanitize.test.ts` (v0.28.8 sanitization parity: 12 cases pinning that `INJECTION_PATTERNS` from `crates/zbrain-core/src/think/sanitize.ts` is the single source of truth — adding a pattern there must cover both `<take>` framing and `<chat_session>` framing, no per-surface regex drift).

E2E tests (`tests/unit/e2e/`): Run against real Postgres+pgvector. Require `DATABASE_URL`.
- `bun run test:e2e` runs Tier 1 (mechanical, all operations, no API keys). Includes 9 dedicated cases for the postgres-engine `addLinksBatch` / `addTimelineEntriesBatch` bind path — postgres-js's `unnest()` binding is structurally different from PGLite's and gets its own coverage.
- `tests/unit/e2e/search-quality.test.ts` runs search quality E2E against PGLite (no API keys, in-memory)
- `tests/unit/e2e/graph-quality.test.ts` runs the v0.10.3 knowledge graph pipeline (auto-link via put_page, reconciliation, traversePaths) against PGLite in-memory
- `tests/unit/e2e/postgres-jsonb.test.ts` — v0.12.2 regression test. Round-trips all 5 JSONB write sites (pages.frontmatter, raw_data.data, ingest_log.pages_updated, files.metadata, page_versions.frontmatter) against real Postgres and asserts `jsonb_typeof='object'` plus `->>'key'` returns the expected scalar. The test that should have caught the original double-encode bug.
- `tests/unit/e2e/integrity-batch.test.ts` (v0.22.8) — parity tests for `scanIntegrity`'s batch-load fast path vs sequential. Four cases (dedup, hits, validate, topPages) seed a fixture and assert both paths return identical results. Dedup case uses raw SQL via `getConn().unsafe()` to seed a `(test-source-2, people/alice)` row alongside the default-source row, since `engine.putPage` doesn't take a `source_id`. Pins the codex-caught multi-source overcounting regression.
- `tests/unit/e2e/jsonb-roundtrip.test.ts` — v0.12.3 companion regression against the 4 doctor-scanned JSONB sites. Assertion-level overlap with `postgres-jsonb.test.ts` is intentional defense-in-depth: if doctor's scan surface ever drifts from the actual write surface, one of these tests catches it.
- `tests/unit/e2e/sync.test.ts` (v0.22.12 — `--skip-failed` failure-loop test, alongside the existing 13 happy-path tests): exercises the full chain — broken file → `performSync` returns `blocked_by_failures` with grouped breakdown → `performSync({skipFailed: true})` advances bookmark and returns `AcknowledgeResult` with code summary → second broken file → second cycle. Saves and restores the user's real `~/.zbrain/sync-failures.jsonl` so the test is hermetic on a developer machine. Asserts bookmark gating, JSONL state, dedup across paths, summary aggregation, and the literal doctor-rendering string format. This is the integration test that proves the v0.22.12 chain holds together — unit tests cover the pure functions in isolation, this covers the integration.
- `tests/unit/e2e/upgrade.test.ts` runs check-update E2E against real GitHub API (network required)
- `tests/unit/e2e/minions-shell-pglite.test.ts` (v0.20.4) exercises the PGLite `--follow` inline shell-job path (in-memory, no `DATABASE_URL` required) — the path the consolidated minion-orchestrator skill documents for dev use
- `tests/unit/e2e/openclaw-reference-compat.test.ts` (v0.19) — exercises `check-resolvable` + `skillpack install` against a minimal AGENTS.md workspace fixture (`tests/unit/fixtures/openclaw-reference-minimal/`), regression guard for the 107-skill OpenClaw deployment shape
- `tests/unit/e2e/search-swamp.test.ts` (v0.22.0) — reproduces the headline source-swamp case. Seeds a curated `originals/talks/article-outline-fat-code` page against two `wintermute/chat/` pages stuffed with the same multi-word phrase. Asserts the article wins keyword AND vector ranking, that `detail=high` lets the chat swamp re-surface (temporal-query workflow preserved), and that `source_id` passes through the two-stage CTE intact. PGLite in-memory.
- `tests/unit/e2e/search-exclude.test.ts` (v0.22.0) — verifies `tests/unit/` + `archive/` pages are hidden by default, that `include_slug_prefixes` opts back in, and that caller-supplied `exclude_slug_prefixes` adds to defaults. Both keyword and vector search paths covered.
- `tests/unit/e2e/engine-parity.test.ts` (v0.22.0) — Postgres ↔ PGLite top-result and result-set parity for `searchKeyword` + `searchVector`. Codex flagged that Postgres ranks pages then picks best chunk while PGLite returns chunks directly — without parity coverage the source-boost fix could pass on PGLite and fail on Postgres. Skips gracefully when `DATABASE_URL` is unset.
- `tests/unit/e2e/postgres-bootstrap.test.ts` (v0.22.6.1) — exercises `PostgresEngine.initSchema()` directly against a fresh real Postgres database. Asserts the bootstrap path is no-op on fresh installs and that SCHEMA_SQL replays cleanly through the engine path (not via the standalone `db.initSchema` from `crates/zbrain-core/src/db.ts`, which would have produced false-positive coverage). Codex caught the E2E-shape gap during plan review.
- `tests/unit/e2e/http-transport.test.ts` (v0.22.7) — 8 cases against real Postgres covering `zbrain serve --http` end-to-end: bearer auth round-trip, `last_used_at` SQL-level debounce semantics, `mcp_request_log` row insertion on success and auth_failed paths, `/health` DB-down → 503 (DB-probing health check), and the F1+F2+F3 dispatch round-trip with a real operation. Skips gracefully when `DATABASE_URL` is unset.
- `tests/unit/e2e/serve-http-oauth.test.ts` (v0.26.0, expanded v0.26.2, expanded v0.26.9) — real-Postgres E2E against `zbrain serve --http` with full OAuth 2.1. Spawns a subprocess server, registers a client via the CLI, mints `client_credentials` tokens, exercises the `/mcp` JSON-RPC pipeline. **v0.26.2 adds:** real DCR `/register` HTTP-level response-shape test (asserts `typeof body.client_id_issued_at === 'number'` over the wire — RFC 7591 §3.2.1 spec compliance, not just internal-store shape); real CLI subprocess test for `revoke-client` (registers → mints token → revokes via `execSync` → asserts token rejected at `/mcp` → asserts re-run exits 1); server fixture flips on `--enable-dcr` so `/register` is reachable. **bun execSync env-inheritance fix:** bun's `execSync` does NOT inherit env mutations done via `process.env.X = ...`, only OS-level env from before bun started. helpers.ts loads `.env.testing` and sets `DATABASE_URL` via `process.env` mutation, which is invisible to subprocesses unless `env: { ...process.env }` is passed explicitly — every subprocess call in this file passes `env: { ...process.env }` for that reason. Reference fix for the next maintainer hitting the same failure mode in sibling sync/cycle/dream/claw-test E2Es. `afterAll` cleanup is guarded on `clientId` (won't throw if `beforeAll` failed before registration); cleanup errors surface to stderr without throwing so real test failures aren't masked. Tracks DCR-registered clients alongside the manual one. **v0.26.9** adds 2 regressions for the F7 trust-boundary fix: an HTTP MCP `submit_job` for `name: "shell"` MUST reject with a permission error (proving the request handler now sets `remote: true` and `submit_job`'s protected-name guard fires), and the same guard rejects subagent submission. Closes the OAuth-token-to-RCE escalation path. Skips gracefully when `DATABASE_URL` is unset.
- `tests/unit/e2e/sync-parallel.test.ts` (v0.22.13 PR #490) — DATABASE_URL-gated. T2: 60-file Postgres sync at concurrency=4 imports all + no connection leak (probes `pg_stat_activity` before/after to confirm worker engines disconnected). P4: 120-file serial-vs-parallel benchmark prints `SYNC_PARALLEL_BENCH N files | serial=Xms | parallel(4)=Yms | speedup=Zx` for CHANGELOG quoting. Asserts parallel ≤ serial × 1.5 (CI-noise tolerant; not a strict speedup gate).
- `tests/unit/e2e/multi-source-bug-class.test.ts` (v0.32.8, PR #860) — 7-case PGLite in-memory regression suite pinning every bug site fixed in this PR: `listAllPageRefs` ordering by `(source_id, slug)` (F11), `getPage` with sourceId picks the right `(source, slug)` row (F2), `extract-takes` processes both overlapping `people/alice` rows independently, `listPages` filters correctly with `PageFilters.sourceId`, `addLinksBatch` with `from/to_source_id` targets the right rows (F4), `validateSourceId` rejects path traversal (F6), reverse-write disk layout uses `brainDir/.sources/<id>/<slug>.md` for non-default sources (F6). No DATABASE_URL needed. Wired into `scripts/e2e-test-map.ts` so changes to extract-takes / patterns / synthesize / embed / extract / migrate-engine auto-trigger this test. Companion: `tests/unit/e2e/integrity-batch.test.ts`'s "multi-source duplicate slugs scan once" case was pinning the pre-fix bug — assertion flipped in v0.32.8 to expect both batch + sequential paths report 2.
- `tests/unit/e2e/source-isolation-pglite.test.ts` (v0.34.1.0, #861) — 14-case PGLite in-memory regression suite pinning the source-isolation P0 seal at two layers. Engine layer: `searchKeyword` / `searchVector` / `searchKeywordChunks` / `listPages` / `getPage` / `traverseGraph` / `traversePaths` apply `sourceId` (scalar fast path) and `sourceIds` (array path) correctly across both engines. Op-handler layer: routes through `sourceScopeOpts(ctx)` so a `read+write`-scoped OAuth client bound to `--source dept-x` cannot see rows from neighboring sources via `search`, `query`, `list_pages`, `get_page`, or `find_experts`. Covers both `ctx.sourceId` (single-source clients) and `ctx.auth.allowedSources` (federated_read clients) precedence; federated array wins over scalar wins over nothing. No DATABASE_URL needed.
- `tests/unit/openai-compat-multimodal.test.ts` (v0.34.1.0, #875) — 11-case unit suite for the gateway's openai-compatible multimodal path: happy-path single + multi-input embedding, unauthenticated proxy mode, dimension-mismatch guard (D12; throws `AIConfigError` with model id + observed + expected pre-storage), default-dim fallback when recipe declares `default_dims`, HTTP 401 / 400 / malformed-JSON / non-array error paths, plus a regression test that the existing Voyage `/multimodalembeddings` recipe still routes through its dedicated path (not the openai-compatible one). Hermetic via the `__setEmbedTransportForTests` seam.
- `tests/unit/serve-stdio-lifecycle.test.ts` (extended v0.34.1.0, #870) — adds 3 new cases for the `MCP_STDIO=1` env guard: stdin EOF does NOT trigger shutdown when the env is set, SIGTERM still does (guard scope is correct), unset env preserves the pre-v0.34 CLI lifecycle. Exercises the `ServeOptions.mcpStdio?: boolean` test seam directly so tests don't mutate `process.env`.
- `tests/unit/oauth.test.ts` (extended v0.34.1.0, #909) — 5 new cases for the PKCE DCR public-client gate: `registerClient` with `token_endpoint_auth_method: "none"` returns no `client_secret` field on the public client, default `client_secret_post` clients still get the one-time-reveal secret, `getClient` NULL→undefined normalization so the SDK's clientAuth path accepts public clients, full PKCE `/authorize` → `/token` round-trip against a public client (no client_secret presented), and a regression test that the public-vs-confidential branch doesn't break confidential client `client_secret_post` exchange.
- Tier 2 (`skills.test.ts`) requires OpenClaw + API keys, runs nightly in CI
- If `.env.testing` doesn't exist in this directory, check sibling worktrees for one:
  `find ../  -maxdepth 2 -name .env.testing -print -quit` and copy it here if found.
- **Run E2E tests without asking permission.** When you want to verify behavior,
  there's a relevant E2E test, or you're shipping touching anything covered by an
  E2E suite — just spin up the test DB, run the tests, and tear down. Don't ask,
  don't propose it, don't defer. The lifecycle is short (~2-30s startup, sub-minute
  tests, instant teardown) and the gate value is high. Skipping with "DATABASE_URL
  unset" is silent regression, not caution.

### API keys and running ALL tests

ALWAYS source the user's shell profile before running tests:

```bash
source ~/.zshrc 2>/dev/null || true
```

This loads `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`. Without these, Tier 2 tests
skip silently. Do NOT skip Tier 2 tests just because they require API keys — load
the keys and run them.

When asked to "run all E2E tests" or "run tests", that means ALL tiers:
- Tier 1: `bun run test:e2e` (mechanical, sync, upgrade — no API keys needed)
- Tier 2: `tests/unit/e2e/skills.test.ts` (requires OpenAI + Anthropic + openclaw CLI)
- Always spin up the test DB, source zshrc, run everything, tear down.

### E2E test DB lifecycle (ALWAYS follow this)

You are responsible for spinning up and tearing down the test Postgres container.
Do not leave containers running after tests. Do not skip E2E tests, do not ask
permission to run them — see the "run without asking" rule above.

1. **Check for `.env.testing`** — if missing, copy from sibling worktree.
   Read it to get the DATABASE_URL (it has the port number).
2. **Check if the port is free:**
   `docker ps --filter "publish=PORT"` — if another container is on that port,
   pick a different port (try 5435, 5436, 5437) and start on that one instead.
3. **Start the test DB:**
   ```bash
   docker run -d --name zbrain-test-pg \
     -e POSTGRES_USER=postgres -e POSTGRES_PASSWORD=postgres \
     -e POSTGRES_DB=zbrain_test \
     -p PORT:5432 pgvector/pgvector:pg16
   ```
   Wait for ready: `docker exec zbrain-test-pg pg_isready -U postgres`
4. **Bootstrap the schema** (required — fresh containers have no `oauth_clients`,
   `mcp_request_log`, `pages` etc.; tests like `serve-http-oauth.test.ts` will fail
   with `relation "oauth_clients" does not exist` if you skip this):
   ```bash
   DATABASE_URL=postgresql://postgres:postgres@localhost:PORT/zbrain_test \
     bun run bin/zbrain-rs.js doctor --json > /dev/null 2>&1
   ```
   `zbrain doctor` triggers `initSchema()` on first connect, which is the canonical
   way to bring a fresh DB to head. `apply-migrations --yes` alone does NOT seed
   the base schema — it runs ALTER-style migrations on top of `initSchema`. Tests
   that bypass the engine (raw `execSync`-spawned `auth register-client`) hit the
   schema directly and need this step to have run first.
5. **Run E2E tests:**
   `DATABASE_URL=postgresql://postgres:postgres@localhost:PORT/zbrain_test bun run test:e2e`
6. **Tear down immediately after tests finish (pass or fail):**
   `docker stop zbrain-test-pg && docker rm zbrain-test-pg`

Never leave `zbrain-test-pg` running. If you find a stale one from a previous run,
stop and remove it before starting a new one.

## Search Mode (v0.32.3)

ZBrain ships three named search modes that bundle the search-lite knobs from
PR #897 into a single config key. Pick one at install time; the rest of the
project resolves through `crates/zbrain-core/src/search/mode.ts`.

| Knob                          | `conservative` | `balanced` | `tokenmax`     |
|-------------------------------|----------------|------------|----------------|
| `cache.enabled`               | true           | true       | true           |
| `cache.similarity_threshold`  | 0.92           | 0.92       | 0.92           |
| `cache.ttl_seconds`           | 3600           | 3600       | 3600           |
| `intentWeighting`             | true           | true       | true           |
| `tokenBudget`                 | **4000**       | **12000**  | **off**        |
| `expansion` (LLM multi-query) | false          | false      | **true**       |
| `searchLimit` default         | 10             | 25         | 50             |

**Cost anchors (downstream agent input cost — zbrain itself is rounding error).**
The corner-to-corner spread is 25x once you pair mode with downstream model.
Chunks ~400 tokens avg. Per-query cost @ 10K queries/month (typical
single-user volume), full search payload, no cache savings:

| Mode \ Downstream | Haiku 4.5 (\$1/M) | Sonnet 4.6 (\$3/M) | Opus 4.7 (\$5/M) |
|---|---|---|---|
| conservative (~4K) | **\$40/mo** | \$120/mo | \$200/mo |
| balanced (~10K) | \$100/mo | \$300/mo | \$500/mo |
| tokenmax (~20K) | \$200/mo | \$600/mo | **\$1,000/mo** |

Scales linearly: multiply by 10 for 100K/mo (heavy power user / multi-user
fleet); divide by 10 for 1K/mo (light usage). Natural pairings span ~4x.
Mismatches (tokenmax+Haiku, conservative+Opus) waste capacity differently
— too-big payload overwhelms a cheap model; too-small payload starves an
expensive one.

tokenmax adds ~\$1.50 per 1K queries in Haiku expansion calls on top of
the matrix (\$15/mo @ 10K). Cache hits cut all numbers ~50%. **The cost
picker copy in `zbrain init` carries the same matrix verbatim** — update
both when refreshing.

**Per-query math vs real-world spend.** The matrix above is what an
isolated benchmark would measure. Real agent loops with disciplined
Anthropic prompt caching see 50-80% discount on top (cache hits skip
downstream entirely). The realistic-scale anchor in
`docs/eval/SEARCH_MODE_METHODOLOGY.md` walks the natural pairings at
single-power-user volume (~860 turns/mo): tokenmax+Opus ~\$700/mo,
balanced+Sonnet ~\$430/mo, conservative+Haiku ~\$170/mo. Setups WITHOUT
cache-aware prompt layout (frequent prefix churn) see the per-query
matrix dominate — mode + model choice matters more there.

**Resolution chain** (matches the v0.31.12 model-tier pattern at
`crates/zbrain-core/src/model-config.ts:resolveModel`):

    per-call SearchOpts → per-key config (search.cache.enabled, …) →
      MODE_BUNDLES[search.mode] → MODE_BUNDLES.balanced (fallback)

Mode resolution lives in **bare `hybridSearch`** (NOT just the cached wrapper)
per `[CDX-5+6]` in `~/.claude/plans/lets-take-a-look-validated-parrot.md` — so
`zbrain eval replay` and `zbrain eval longmemeval` test the same mode-affected
behavior as the production `query` op.

**Cache-key contamination hotfix `[CDX-4]`:** migration v56 added a
`knobs_hash` column to `query_cache`. The lookup filter is now
`WHERE source_id = $ AND knobs_hash = $ AND embedding similarity < $` so a
tokenmax write (expansion=on, limit=50) can't be served to a conservative
read.

**v0.36.3.0 knobs_hash v=2 → v=3.** The hash now folds the active
embedding column name + provider into the cache key, so a query routed
through `embedding_voyage` (1024d Voyage) can't be served a cache row
written against `embedding` (1536d OpenAI). Existing v=2 rows become
unreachable on first re-query (one-time miss spike on upgrade);
`mode.ts:KNOBS_HASH_VERSION` is the single source of truth.

**Three CLI surfaces:**

    zbrain search modes              # what is running, with per-knob attribution
    zbrain search modes --reset      # clear search.* overrides (mode bundle wins)
    zbrain search stats [--days N]   # cache hit rate, intent mix, budget drops
    zbrain search tune [--apply]     # data-driven recommendations

The install picker fires inside `zbrain init` AFTER `engine.initSchema()`
(non-TTY auto-selects). The upgrade banner fires once via `runPostUpgrade`
in `crates/zbrain-cli/src/upgrade.ts`, gated by `search.mode_upgrade_notice_shown`.

## Eval discipline (v0.32.3)

Every metric printed by any `zbrain eval *` or `zbrain search stats` command
resolves through `crates/zbrain-core/src/eval/metric-glossary.ts` so industry terms
(`P@k`, `nDCG@k`, `MRR`, `Jaccard@k`) carry a plain-English line in human
output and a `_meta.metric_glossary` block in JSON output (one block per
response per `[CDX-25]`, NOT sibling `_gloss` fields).

The full methodology — datasets, sample selection, pre-registered
expectations, threats to validity, paired-bootstrap + Bonferroni p-value
discipline `[CDX-14]` — lives in `docs/eval/SEARCH_MODE_METHODOLOGY.md`.
Auto-regenerated `docs/eval/METRIC_GLOSSARY.md` is CI-guarded against
drift (`scripts/check-eval-glossary-fresh.sh`).

Per-run records land at `<repo>/.zbrain-evals/eval-results.jsonl` per
`[CDX-23]`. The user's personal `~/.zbrain` brain is NEVER touched —
audit trail lives in the source repo's git history.

## Skills

Read the skill files in `skills/` before doing brain operations. ZBrain ships 29 skills
organized by `skills/RESOLVER.md` (`AGENTS.md` is also accepted as of v0.19):

**Original 8 (conformance-migrated):** ingest (thin router), query, maintain, enrich,
briefing, migrate, setup, publish.

**Brain skills (ported from an upstream agent fork):** signal-detector, brain-ops, idea-ingest, media-ingest,
meeting-ingestion, citation-fixer, repo-architecture, skill-creator, daily-task-manager.

**Operational + identity:** daily-task-prep, cross-modal-review, cron-scheduler, reports,
testing, soul-audit, webhook-transforms, data-research, minion-orchestrator. As of
v0.20.4, `minion-orchestrator` is the single unified skill for both lanes of background
work (shell jobs via `zbrain jobs submit shell`, LLM subagents via `zbrain agent run`) ...
the prior `zbrain-jobs` skill was merged in, Preconditions are shared, and trigger
routing is narrowed to what the skill actually covers.

**Skillify loop (v0.19):** skillify (the markdown orchestration), skillpack-check
(agent-readable health report).

**Routing-table compression (v0.32.3.0):** `skills/functional-area-resolver/` —
two-layer dispatch pattern for shrinking large AGENTS.md / RESOLVER.md files
(>=12KB) without losing routing accuracy. Replaces one row per skill with one
entry per functional area, where each area declares its sub-skills in a
`(dispatcher for: ...)` clause. The static-prompt analog of hierarchical agent
routing (AnyTool [arXiv:2402.04253](https://arxiv.org/abs/2402.04253), RAG-MCP
[arXiv:2505.03275](https://arxiv.org/html/2505.03275v1), Anthropic Agent Skills
progressive disclosure). Empirically validated across Opus 4.7 / Sonnet 4.6 /
Haiku 4.5: +13 to +17pp over the verbose baseline at 48% the size (25KB → 13KB
on a real fork). The `(dispatcher for: ...)` clause is the load-bearing signal
— strip it and lenient accuracy collapses to 41.7% on Sonnet (the
`resolver-of-resolvers` ablation case). A/B eval surface lives at
`evals/functional-area-resolver/` (outside `skills/` deliberately so the
skillpack bundler doesn't ship eval infrastructure to downstream installs):
gateway-routed TypeScript harness, 20 training + 5 held-out fixtures, strict +
lenient scoring, three committed cross-model receipts in `baseline-runs/`.
Receipt header binds (model, prompt_template_hash, fixtures_hash, harness_sha,
ts) so future contributors can verify reproduction. Companion `rescore.mjs`
re-scores existing JSONL with lenient tolerance for zero API cost. Reproduce
with `cd evals/functional-area-resolver && node harness.mjs --model
{opus|sonnet|haiku}` (~$0.30–1.70 per model). Nine v0.33.x follow-up TODOs
filed for held-out corpus growth, cross-vendor verification, hierarchical
area-of-areas, embedding-based pre-router, and the run-1 vs run-2
prompt-design ablation methodology.

**Operational health (v0.19.1):** smoke-test (8 post-restart health checks with auto-fix
for Bun, CLI, DB, worker, Zod CJS, gateway, API key, brain repo; user-extensible via
`~/.zbrain/smoke-tests.d/*.sh`).

**Conventions:** `skills/conventions/` has cross-cutting rules (quality, brain-first,
model-routing, test-before-bulk, cross-modal). `skills/_brain-filing-rules.md` and
`skills/_output-rules.md` are shared references.

## Bulk-action progress reporting

All bulk commands (doctor, embed, import, export, sync, extract, migrate,
repair-jsonb, orphans, check-backlinks, lint, integrity auto, eval, files
sync, and apply-migrations) stream progress through the shared reporter
at `crates/zbrain-core/src/progress.ts`. Agents get heartbeats within 1 second of every
iteration regardless of how slow the underlying work is.

Rules:
- Progress always writes to **stderr**. Stdout stays clean for data output
  (`--json` payloads, final summaries, JSON action events from `extract`).
- Non-TTY default: plain one-line-per-event human text. JSON requires the
  explicit `--progress-json` flag.
- Global flags (`--quiet`, `--progress-json`, `--progress-interval=<ms>`)
  are parsed by `crates/zbrain-core/src/cli-options.ts` BEFORE command dispatch.
- Phase names are machine-stable `snake_case.dot.path` (e.g.
  `doctor.db_checks`, `sync.imports`). Documented in
  `docs/progress-events.md`; additive changes only.
- `scripts/check-progress-to-stdout.sh` is a CI guard that fails the build
  if any new code writes `\r` progress to stdout. Wired into `bun run test`.
- Minion handlers pass `job.updateProgress` as the `onProgress` callback
  to core functions (DB-backed primary progress channel); stderr from
  `jobs work` stays coarse for daemon liveness only.

When wiring a new bulk command: `import { createProgress } from '../core/progress.ts'`
and `import { getCliOptions, cliOptsToProgressOptions } from '../core/cli-options.ts'`.
Create a reporter with `createProgress(cliOptsToProgressOptions(getCliOptions()))`,
`start(phase, total?)` before the loop, `tick()` inside it, `finish()` after.
For single long-running queries, use `startHeartbeat(reporter, note)` with a
try/finally to guarantee cleanup. Never call `process.stdout.write('\r...')`
in bulk paths, the CI guard will fail the build.

## Capturing test output (NEVER pipe through `tail` / `head`)

**Iron rule:** when running `bun test`, `bun run test:e2e`, `bun run typecheck`,
or any other tests/unit/check command, redirect to a file FIRST, then `tail` the file
separately:

```bash
# RIGHT — full output preserved, real exit code visible
bun test > /tmp/ship_units.txt 2>&1
echo "EXIT=$?"
tail -50 /tmp/ship_units.txt
grep -E '(fail\)|✗|error:' /tmp/ship_units.txt | head -30
```

```bash
# WRONG — exit code is `tail`'s (always 0), failures truncated, ship gates fail open
bun test 2>&1 | tail -10
```

The pipe form silently breaks /ship Step T1 (test failure ownership triage) and
the test verification gate (Step 16) because:
- `$?` after a pipe is the LAST command's exit code (`tail` → 0), not bun's
- bun prints failure details before the summary line, so `tail -N` drops them
- Step T1 needs the full failure list to classify in-branch vs pre-existing

This bit us during v0.26.2 ship: `bun test 2>&1 | tail -10` reported "3911 pass / 23 fail"
but no failure details survived, forcing a 23-minute re-run to triage.

Apply the same pattern to any long-running command whose exit code matters:
`bun run typecheck`, `bun run ci:local`, migration runs, eval suites, etc.
For background tasks (`run_in_background: true`), the harness captures the exit
file separately — use it via the bg task's `<id>.exit` file, not the streamed
output.

## Build

```bash
cargo build --workspace           # build everything (debug)
cargo build -p zbrain-cli         # just the CLI
cargo test --workspace            # run the Rust test suite
cargo clippy --workspace --all-targets
```

The Node wrapper `bin/zbrain-rs.js` runs `cargo build -p zbrain-cli` automatically
when it cannot find a prebuilt `zbrain` binary. Release builds: `cargo build --release -p zbrain-cli`.
## Version locations (single source of truth: `VERSION` file)

Every release advances the version in **five files at once**. Keep these in
sync. `/ship` enforces this via Step 12's idempotency check (VERSION vs
Cargo.toml drift), but the canonical list lives here so future runs and
the auto-update agent know where to look.

**Version format is mandatory: `MAJOR.MINOR.PATCH.MICRO` (four numeric
segments, dot-separated, no leading `v`).** Every new release MUST use the
4-segment form. The `.MICRO` slot is the dot-suffix follow-up channel: when
a release ships its commit subject ahead of its VERSION bump (e.g. PR #795
landing as `v0.31.4` without bumping the file), the corrective ship lands
as `0.31.4.1` rather than churning the patch number to `0.31.5`. Suffixes
like `-fixwave` are still allowed as needed (`0.31.1.1-fixwave`), but the
four numeric segments are required first. Historical 3-segment versions
(`0.31.3`, `0.22.1`) remain valid in `git log` and migration filenames
(`skills/migrations/v0.21.0.md`); do NOT rewrite them. Going forward only.

**Required (every release must update all five):**

| File | What lives there | Format |
|---|---|---|
| `VERSION` | The single source of truth. Read first by `/ship`, the binary, and CI version-gate. | Bare 4-segment string `MAJOR.MINOR.PATCH.MICRO` (e.g. `0.31.4.1`), no leading `v`. |
| `Cargo.toml` | Bun/npm package version. `zbrain --version` reads it via the compiled binary's bundled package metadata. CI version-gate cross-checks this against `VERSION` and fails if they drift. | `"version": "0.31.4.1"` |
| `CHANGELOG.md` | Top entry header `## [0.31.4.1] - YYYY-MM-DD` plus the "To take advantage of v0.31.4.1" block. | Standard Keep-a-Changelog header. |
| `TODOS.md` | Any TODO entries that mention "follow-up from vX.Y.Z.W" use the version of the release that filed them. Update only when filing NEW follow-up TODOs. | Inline `vX.Y.Z.W` references in TODO bodies. |
| `CLAUDE.md` | The Key Files section's per-file annotations carry `vX.Y.Z.W (#NNN)` tags noting which release introduced a behavior. Update whenever a wave's annotations get folded in. | Inline `vX.Y.Z.W (#NNN, contributed by @user)` references. |

**Auto-derived (no manual edit; refreshed by their own commands):**

- `Cargo.lock` — root-package version is auto-pinned from `Cargo.toml`. After
  bumping `Cargo.toml`, run `bun install` to refresh the lockfile.
- `llms-full.txt` / `llms.txt` — auto-generated documentation bundles. **Any
  CLAUDE.md edit MUST be followed by `bun run build:llms` in the same commit
  (or a follow-up commit before push).** The committed bundles are checked
  against fresh generator output by `tests/unit/build-llms.test.ts`, which runs in
  CI shard 1. If you edited CLAUDE.md and didn't regenerate, CI will fail.
  This has bitten the wave 3 times — every CLAUDE.md edit gets a `bun run
  build:llms` chaser, no exceptions. (The `verify` gate doesn't run this
  test; only the full unit suite does. So `bun run typecheck` clean is NOT
  enough to know you can push after a CLAUDE.md edit.)

**Historical (DO NOT bump on release):**

- `skills/migrations/v0.21.0.md` — migration files use the version they
  shipped FROM as their filename. v0.21.0's migration always says v0.21.0.
- `crates/zbrain-cli/src/migrations/v0_21_0.ts` — same: migration code references
  the schema version it migrates to.
- `tests/unit/migrations-v0_21_0.test.ts`, `tests/unit/migration-orchestrator-v0_21_0.test.ts`,
  `tests/unit/migrate.test.ts` — migration tests reference historical migration
  versions; these are correct as-is and should not move.
- `crates/zbrain-core/src/db.ts`, `crates/zbrain-core/src/migrate.ts`, `crates/zbrain-core/src/import-file.ts`,
  `crates/zbrain-cli/src/reindex-code.ts` — code comments cite the release that
  introduced a feature. Once written, these are historical record.
- `README.md` — references the latest published feature names by version
  (e.g. "v0.21.0 Code Cathedral"); update only when the README's marketing
  copy is intentionally being refreshed, NOT on every micro/patch bump.

**The /ship workflow's version idempotency check:** Step 12 reads
`VERSION` and `Cargo.toml`, classifies as FRESH / ALREADY_BUMPED /
DRIFT_STALE_PKG / DRIFT_UNEXPECTED, and refuses to proceed on
DRIFT_UNEXPECTED. This is why the two must move together.

**The CI version-gate** rejects pushes where `VERSION` and
`Cargo.toml` disagree, OR where `VERSION` is not strictly greater
than master's VERSION. If a queue collision claims your version on
master before yours lands, /ship's queue-aware allocator (Step 12)
will detect drift and re-bump on the next run.

### Mandatory version-consistency audit (run after EVERY merge or commit that touches VERSION, Cargo.toml, or CHANGELOG)

**The trio MUST agree.** Every merge from master will hit conflicts on
VERSION + Cargo.toml + CHANGELOG.md because master ships its own
version bumps. Auto-merge sometimes resolves these silently in unexpected
ways. After any merge, branch update, or version-related edit, run this
audit. It's three lines and never lies:

```bash
echo "VERSION:     $(cat VERSION)"
echo "Cargo.toml: $(node -e 'process.stdout.write(require("./Cargo.toml").version)')"
grep -E "^## \[" CHANGELOG.md | head -1
```

All three MUST show the same `MAJOR.MINOR.PATCH.MICRO`. If any one
disagrees, you have not finished the merge. Fix it before pushing or
shipping. There is no situation in which "I'll fix it next push" is OK,
because:

- A green local test run with mismatched VERSION/Cargo.toml still
  fails the CI version-gate.
- A green CHANGELOG entry under the wrong version header silently lies
  to release-notes consumers.
- /ship's Step 12 idempotency check classifies a mismatch as
  `DRIFT_UNEXPECTED` and HALTS — but only if you remember to run /ship
  before pushing. Manual `git push` skips the check.

### Merge-conflict recovery procedure (memorize this)

When `git merge origin/master` reports conflicts on VERSION,
Cargo.toml, or CHANGELOG.md, resolve in this exact order:

1. **VERSION** — overwrite with the wave's version (`echo -n "X.Y.Z.W"
   > VERSION`). Highest semver wins; do NOT take master's lower version.
2. **Cargo.toml** — strip the conflict markers, keep the wave's
   version line. Sed pattern:
   `sed -i.bak '/^<<<<<<< HEAD$/d; /^=======$/,/^>>>>>>> /d' Cargo.toml && rm Cargo.toml.bak`
   (assumes ours is above the `=======`).
3. **CHANGELOG.md** — strip ALL three conflict markers; both your entry
   and master's entry stay. Sed pattern:
   `sed -i.bak '/^<<<<<<< HEAD$/d; /^=======$/d; /^>>>>>>> origin\/master$/d' CHANGELOG.md && rm CHANGELOG.md.bak`
   Then verify your entry is the topmost `## [X.Y.Z.W]` and master's
   newer-than-yours entries (if any) sit below.
4. **Run the 3-line audit above.** If it doesn't show your version on
   all three lines, you missed a marker.
5. **Run `bun install`** to refresh `Cargo.lock` against the resolved
   `Cargo.toml`. Stage and commit if it changed.
6. **Run `bun run typecheck`** before committing the merge.
7. Only THEN run `git commit` for the merge.

If the audit shows drift after step 4, do NOT proceed to step 5. Re-run
steps 1-3 against the actual file content; you missed a marker or
resolved one in the wrong direction.

**Anti-pattern to avoid:** Resolving via `git checkout --ours Cargo.toml`
and `git checkout --theirs scripts/test-shard.sh` mixed in the same
commit. The selective directional resolution is fine, but on
VERSION/Cargo.toml/CHANGELOG specifically, ALWAYS use the explicit
`echo > VERSION` + sed-strip-markers pattern above. The directional
checkout flags have bitten us when the conflict shape was unexpected
(e.g. master stripped a section we expected to keep).

### Pre-push gate (manual; tighten when you remember to)

Before any `git push` of a merge commit, run the audit one more time:

```bash
echo "VERSION:     $(cat VERSION)"
echo "Cargo.toml: $(node -e 'process.stdout.write(require("./Cargo.toml").version)')"
grep -E "^## \[" CHANGELOG.md | head -1
```

If you've been editing the branch via `/ship` you can rely on Step 12's
idempotency check. If you've been editing manually (merge resolution,
conflict fix, version bump), the audit is the last line of defense
before CI yells at you.

## Conductor branch-name = workspace-name (IRON RULE)

Conductor workspaces expect the git branch name to match the workspace
directory name. When they disagree, Conductor silently fails to render the
PR view + show ship state, leading to "did you actually push?" confusion.

**Check this FIRST on every ship and BEFORE creating any PR:**

```bash
WORKSPACE=$(basename "$PWD")              # e.g. puebla-v4
BRANCH=$(git branch --show-current)        # e.g. garrytan/gstack-requests
case "$BRANCH" in
  */"$WORKSPACE") echo "OK: branch tail matches workspace" ;;
  "$WORKSPACE")   echo "OK: branch == workspace" ;;
  *)              echo "MISMATCH: branch=$BRANCH workspace=$WORKSPACE — RENAME BEFORE SHIPPING" ;;
esac
```

If MISMATCH (branch is `garrytan/foo` but workspace is `puebla-v4`):

```bash
# Rename local, push under new name, delete old remote (and old PR if it
# was already created — github auto-closes it when head ref dies).
git branch -m garrytan/<workspace-name>
git push -u origin garrytan/<workspace-name>
git push origin --delete <old-branch-name>
# If a PR existed against the old branch:
#   gh pr comment <old-pr> --body "Superseded by #<new>: branch renamed to match Conductor workspace."
#   gh pr create --base master --title "..." --body "..."  # recreate from renamed branch
```

Caught the hard way on v0.41.9.0 ship: workspace `puebla-v4` but branch
`garrytan/gstack-requests` produced PR #1439 that Conductor wouldn't
display. Renamed to `garrytan/puebla-v4`; recreated as #1440.

The /ship workflow's Step 1 should be augmented to run the mismatch
check; until that lands upstream, ALWAYS run the check above before
`/ship` invokes its first push or PR-create step.

## Pre-ship requirements

Before shipping (/ship) or reviewing (/review), always run the full test suite.
Two equivalent paths:

**Path A — local CI gate (recommended, v0.23.1+):**
- `bun run ci:local` runs the entire stack inside Docker: gitleaks (host), unit
  tests with `DATABASE_URL` unset, and all 29 E2E files sequentially against a
  fresh pgvector container. Stronger than PR CI's 2-file Tier 1 set; closer to
  what nightly Tier 1 catches. Spins up + tears down postgres automatically via
  `docker-compose.ci.yml`. Override the host port with
  `ZBRAIN_CI_PG_PORT=5435 bun run ci:local` if 5434 collides.
- `bun run ci:local:diff` runs only the E2E files matched by the diff selector
  (`scripts/select-e2e.ts`), falling back to all 29 on unmapped src/ paths or
  schema/skills/Cargo.toml changes. Fast iteration during a focused branch.

**Path B — manual lifecycle (still supported):**
- `bun test` — unit tests (no database required)
- Follow the "E2E test DB lifecycle" steps above to spin up the test DB,
  run `bun run test:e2e`, then tear it down.

Both must pass. Do not ship with failing E2E tests. Do not skip E2E tests.

**Always run typecheck before pushing.** `bun test` (the bun runner)
skips TypeScript type checking — it only enforces runtime behavior.
Three ways to actually gate on types:

1. `bun run test` (npm script in `Cargo.toml`) — includes `bun run typecheck`
   plus the four shell pre-checks (`check-jsonb-pattern.sh`,
   `check-progress-to-stdout.sh`, `check-trailing-newline.sh`,
   `check-wasm-embedded.sh`) before the runner. Use this mid-branch.
2. `bun run typecheck` — `tsc --noEmit` standalone. Fast (~5s on this repo).
3. `bun run ci:local` — the full local CI gate from Path A.

The trap is: writing a new test, running `bun test tests/unit/foo.test.ts`,
seeing it pass, pushing — and CI's separate typecheck stage rejects an
invalid type literal that the runner accepted. Caught one of these
shipping the v0.23.2 round-trip E2E (`type: 'reflection'` is not a
member of `PageType`). Run `bun run typecheck` once before push, even
when only test files changed.

## Post-ship requirements (MANDATORY)

After EVERY /ship, you MUST run /document-release. This is NOT optional. Do NOT
skip it. Do NOT say "docs look fine" without running it. The skill reads every .md
file in the project, cross-references the diff, and updates anything that drifted.

If /ship's Step 8.5 triggers document-release automatically, that counts. But if
it gets skipped for ANY reason (timeout, error, oversight), you MUST run it manually
before considering the ship complete.

Files that MUST be checked on every ship:
- README.md — does it reflect new features, commands, or setup steps?
- CLAUDE.md — does it reflect new files, test files, or architecture changes?
- CHANGELOG.md — does it cover every commit?
- TODOS.md — are completed items marked done?
- docs/ — do any guides need updating?

A ship without updated docs is an incomplete ship. Period.

## CHANGELOG + VERSION are branch-scoped

**VERSION and CHANGELOG describe what THIS branch adds vs master, not how we got
here.** Every feature branch that ships gets its own version bump and CHANGELOG
entry. The entry is product release notes for users; it is not a log of internal
decisions, review rounds, or codex findings.

**Write the CHANGELOG entry at /ship time, not during development.** Mid-branch
iterations, review rounds (CEO/Eng/Codex/DX), and implementation detours belong
in the plan file at `~/.claude/plans/`, not in the CHANGELOG. One unified entry
per branch, covering what the branch added vs the base branch.

**Never edit a CHANGELOG entry that already landed on master.** If master has
v0.18.2 and your branch adds features, bump to the next version (v0.19.0, not
editing master's v0.18.2). When merging master into your branch, master may
bring new CHANGELOG entries above yours — push your entry above master's
latest and verify:

- Does CHANGELOG have your branch's own entry separate from master's entries?
- Is VERSION higher than master's VERSION?
- Is your entry the topmost `## [X.Y.Z]` entry?
- `grep "^## \[" CHANGELOG.md` shows a contiguous version sequence?

If any answer is no, fix it before continuing.

**CHANGELOG is for users, not contributors.** Write like product release notes:

- Lead with what the user can now **do** that they couldn't before. Sell the capability.
- Plain language, not implementation details. "You can now..." not "Refactored the..."
- **Never mention internal artifacts**: plan file IDs, decision tags (D-CX-#, F-ENG-#),
  review rounds, codex findings, subcontractor credits. These are invisible to users.
- Put contributor-facing changes in a separate `### For contributors` section at the bottom.
- Every entry should make someone think "oh nice, I want to try that."

**What to omit:**
- "Codex caught X that the CEO review missed" — private process detail.
- "D-CX-3 split errors/warnings" — tag is meaningless to users; name the feature instead.
- "Fix-wave PR #N supersedes #M" — supersede chains belong in PR bodies, not release notes.
- "215 new cases, 3 decisions applied, 7 reviews cleared" — these are planning-mode metrics.

**What to keep:**
- The user-facing change: what commands exist now, what flag was added, what behavior fixed.
- Numbers that mean something to the user: TTHW, commands that timed out before, detection counts.
- Upgrade instructions: `zbrain upgrade` + any manual step if needed.
- Credit to external contributors when a community PR was incorporated.

## CHANGELOG voice + release-summary format

**IRON RULE: the CHANGELOG describes what the user gets, not how the work
happened.** Nobody reading release notes cares that codex caught a bug, that
the plan went through CEO + eng review, that the migration was originally
numbered v68 and renumbered to v79 during master merge, or that two
review rounds caught architectural mistakes. The reader cares what
`zbrain brainstorm` does and how to use it. If a fact only exists because
of the development process, it does NOT belong in the CHANGELOG.

**Specifically forbidden in CHANGELOG entries:**

- Any mention of review processes (CEO review, eng review, codex review,
  plan-eng-review, outside voice, adversarial review, autoplan, /review).
- "What we caught and fixed before merging" sections. Bugs found pre-merge
  are not changes — they're things that didn't ship.
- Plan file references, plan IDs, plan decision tags (D1, D14, D-CDX-3).
- Migration version drama ("originally v68", "renumbered to v77", "claimed
  by parallel waves") — just say "Migration v79 adds X." If the user
  cares about migration ordering, they read the diff.
- Round counts, finding counts, decision counts ("25 findings across 2
  rounds", "8 architectural decisions", "5/6 expansions accepted").
- Names of internal collaborators ("codex caught", "the reviewer flagged",
  "Claude noticed").
- "Plan + reviews" summary bullets. The plan lives in `~/.claude/plans/`;
  if a future reader wants the backstory they can grep there.
- Any wording that frames a shipped feature as a *recovery* from a planning
  mistake ("the first plan was wrong", "we corrected the approach", "the
  shipped version supersedes the original design").

**Smell test:** read the entry as a stranger who has never touched zbrain.
If any sentence makes them think "why are you telling me this?", cut it.
Every sentence in the release-summary AND in the itemized changes must
answer one of three questions: *What can I now do? How do I use it? What
should I watch for after I upgrade?*

Every version entry in `CHANGELOG.md` MUST start with a release-summary section in
the GStack/Garry voice — one viewport's worth of prose + tables that lands like a
verdict, not marketing. The itemized changelog (subsections, bullets, files) goes
BELOW that summary, separated by a `### Itemized changes` header.

The release-summary section gets read by humans, by the auto-update agent, and by
anyone deciding whether to upgrade. The itemized list is for agents that need to
know exactly what changed.

### Release-summary template

**Iron rule: lead ELI10, get precise after.** The first ~150 words of every entry
must be readable by someone who does NOT know zbrain's internals. No file paths,
no function names, no internal constants, no acronyms (no "RRF", no "knobsHash",
no "MODE_BUNDLES", no "CDX-4"), no jargon that requires reading the codebase to
parse. Lead with the user-visible behavior change, in everyday English, like
you're explaining it to a smart engineer who has never opened the repo.

THEN, once the reader knows what shipped and why they'd care, drill into the
precise details: real file paths, real function names, real config keys, real
numbers. The precision part is required (the entry is also the technical record
of what changed), but it lives AFTER the plain-English lead, never before it.

The shape:

1. **One-line bold headline.** What changed for the user, in human English. No
   jargon. No internal terms. Example good: "Your search stops boosting weak
   pages just because they have a lot of links pointing at them." Example bad:
   "PostFusionOpts gains floorRatio; KNOBS_HASH_VERSION bumped 2→3."
2. **Plain-English opener** (~3-5 sentences). Describe the problem this fixes in
   everyday terms. Pretend the reader has a brain full of meeting notes and
   people pages and wants to know if this release helps them. Concrete example
   beats abstract description.
3. **A "How to turn it on" or "How to use it" section** with paste-ready
   commands. Real flags, real config keys. This is where precision starts.
4. **A "What you'd see in a concrete example" or "The X numbers that matter"
   section** with a table. Use everyday-language column headers ("Page",
   "Match quality", "Has many backlinks?") even when the underlying mechanism
   is technical. The table teaches what the feature does without requiring the
   reader to understand how.
5. **A "What's safe to know about" or "Things to watch" section** for caveats,
   side effects, cache invalidation, mid-deploy notes. Still in plain language.
6. **A "What we caught and fixed before merging" section** if the work went
   through review (CEO/eng/codex/outside-voice). Translate review findings into
   plain English. "We caught a stale-cache bug" beats "knobsHash() did not
   include floorRatio in the v=2 hash input."
7. **`### Itemized changes`** (precision lives here). File paths, function
   names, types, constants, line numbers. This section is for engineers who
   need to know exactly what moved.

Voice rules (apply throughout):
- No em dashes (use commas, periods, "...").
- No AI vocabulary (delve, robust, comprehensive, nuanced, fundamental, etc.) or
  banned phrases ("here's the kicker", "the bottom line", etc.).
- Real numbers, real file names, real commands AFTER the ELI10 lead. Not "fast"
  but "~30s on 30K pages." In the ELI10 lead, "fast enough that you won't
  notice" or "~30 seconds even on a big brain."
- Short paragraphs, mix one-sentence punches with 2-3 sentence runs.
- Connect to user outcomes: "the agent does ~3x less reading" beats "improved
  precision."
- Be direct about quality. "Well-designed" or "this is a mess." No dancing.

**The smell test:** if someone who has never opened zbrain reads the first 150
words and walks away knowing what shipped and whether they care, the entry
passes. If they need to grep the codebase to follow along, rewrite the lead.

**Canonical examples in this CHANGELOG:** v0.35.6.0 (floor-ratio gate, written
ELI10-lead-first), v0.34.4.0 (embed stale fix wave). Use those shapes when in
doubt. Avoid the shape of entries that lead with internal constants or release
mechanics; those exist in older history but should not be the model for new
work.

Source material to pull from:
- CHANGELOG.md previous entry for prior context
- Latest `zbrain-evals/docs/benchmarks/[latest].md` for headline numbers (sibling repo)
- Recent commits (`git log <prev-version>..HEAD --oneline`) for what shipped
- Don't make up numbers. If a metric isn't in a benchmark or production data, don't
  include it. Say "no measurement yet" if asked.

Target length: ~250-350 words for the summary. Should render as one viewport.

### "To take advantage of v[version]" block (required, v0.13+)

After the release-summary and BEFORE `### Itemized changes`, every `## [X.Y.Z]`
entry MUST include a human-readable self-repair block under the heading
`## To take advantage of v[version]`.

Why: `zbrain upgrade` runs `zbrain post-upgrade` which runs `zbrain apply-migrations`.
This chain has a known weak link — `upgrade.ts` catches post-upgrade failures as
best-effort (so the binary still works). When that chain silently fails, users end
up with half-upgraded brains. The self-repair block gives them a paste-ready
recovery path; the v0.13+ `~/.zbrain/upgrade-errors.jsonl` trail + `zbrain doctor`
integration close the loop.

Template (adapt the verify commands per release):

```markdown
## To take advantage of v[version]

`zbrain upgrade` should do this automatically. If it didn't, or if `zbrain doctor`
warns about a partial migration:

1. **Run the orchestrator manually:**
   ```bash
   zbrain apply-migrations --yes
   ```
2. **Your agent reads `skills/migrations/v[version].md` the next time you interact with it.**
   [One sentence on whether headless agents need manual action, or whether the
   orchestrator already handled the mechanical side.]
3. **Verify the outcome:**
   ```bash
   [release-specific verify commands, e.g. `zbrain graph ... --depth 2`]
   zbrain stats
   ```
4. **If any step fails or the numbers look wrong,** please file an issue:
   https://github.com/garrytan/zbrain/issues with:
   - output of `zbrain doctor`
   - contents of `~/.zbrain/upgrade-errors.jsonl` if it exists
   - which step broke

   This feedback loop is how the zbrain maintainers find fragile upgrade paths. Thank you.
```

**Skip this block** for patches that are pure bug fixes with zero user-facing action
(rare). If the release has a schema migration, data backfill, or new feature the
user needs to verify, the block is required.

The v0.13.0 entry in CHANGELOG.md is the canonical example.

### Itemized changes (the existing rules)

Below the release summary, write `### Itemized changes` and continue with the
detailed subsections (Knowledge Graph Layer, Schema migrations, Security hardening,
Tests, etc.). Same rules as before:

- Lead with what the user can now DO that they couldn't before
- Frame as benefits and capabilities, not files changed or code written
- Make the user think "hell yeah, I want that"
- Bad: "Added ZBRAIN_VERIFY.md installation verification runbook"
- Good: "Your agent now verifies the entire ZBrain installation end-to-end, catching
  silent sync failures and stale embeddings before they bite you"
- Bad: "Setup skill Phase H and Phase I added"
- Good: "New installs automatically set up live sync so your brain never falls behind"
- **Always credit community contributions.** When a CHANGELOG entry includes work from
  a community PR, name the contributor with `Contributed by @username`. Contributors
  did real work. Thank them publicly every time, no exceptions.

### Reference: v0.12.0 entry as canonical example

The v0.12.0 entry in CHANGELOG.md is the canonical example of the format. Match its
structure for every future version: bold headline, lead paragraph, "numbers that
matter" with BrainBench-style before/after table, "what this means" closer, then
`### Itemized changes` with the detailed sections below.

## Version migrations

Create a migration file at `skills/migrations/v[version].md` when a release
includes changes that existing users need to act on. The auto-update agent
reads these files post-upgrade (Section 17, Step 4) and executes them.

**You need a migration file when:**
- New setup step that existing installs don't have (e.g., v0.5.0 added live sync,
  existing users need to set it up, not just new installs)
- New SKILLPACK section with a MUST ADD setup requirement
- Schema changes that require `zbrain init` or manual SQL
- Changed defaults that affect existing behavior
- Deprecated commands or flags that need replacement
- New verification steps that should run on existing installs
- New cron jobs or background processes that should be registered

**You do NOT need a migration file when:**
- Bug fixes with no behavior changes
- Documentation-only improvements (the agent re-reads docs automatically)
- New optional features that don't affect existing setups
- Performance improvements that are transparent

**The key test:** if an existing user upgrades and does nothing else, will their
brain work worse than before? If yes, migration file. If no, skip it.

Write migration files as agent instructions, not technical notes. Tell the agent
what to do, step by step, with exact commands. See `skills/migrations/v0.5.0.md`
for the pattern.

## Migration is canonical, not advisory

ZBrain's job is to deliver a canonical, working setup to every user on upgrade.
Anything that looks like a "host-repo change" — AGENTS.md, cron manifests,
launchctl units, config files outside `~/.zbrain/` — is a ZBrain migration
step, not a nudge we leave for the host-repo maintainer. Migrations edit host
files (with backups) to make the canonical setup real. Exceptions: changes
that require human judgment (content edits, renames that break semantics,
host-specific handler registration where shell-exec would be an RCE surface).
Everything mechanical ships in the migration.

**Test:** if shipping a feature requires a sentence that starts with "in
your AGENTS.md, add…" or "in your cron/jobs.json, rewrite…", the migration
orchestrator should be doing that edit, not the user.

**The exception is host-specific code.** For custom Minion handlers
(host-specific integrations like inbox sweeps or third-party API scanners), shipping them as a
data file the worker would exec is an RCE surface. Those get registered in
the host's own repo via the plugin contract (`docs/guides/plugin-handlers.md`);
the migration orchestrator emits a structured TODO to
`~/.zbrain/migrations/pending-host-work.jsonl` + the host agent walks the
TODOs using `skills/migrations/v0.11.0.md` — stays host-agnostic, still
canonical.

## Privacy rule: scrub real names from public docs

**Never reference real people, companies, funds, or private agent names in any
public-facing artifact.** Public artifacts include: `CHANGELOG.md`, `README.md`,
`docs/`, `skills/`, PR titles + bodies, commit messages, and comments in checked-in
code. Query examples, benchmark stories, and migration guides MUST use generic
placeholders.

Why: zbrain runs a personal knowledge brain containing notes on real people and
real companies (YC founders, portfolio companies, funds, investors, meeting
attendees). When a doc copies a query like `zbrain graph diana-hu --depth 2` or
names a specific agent fork like `Wintermute`, that real name gets indexed by
search engines, surfaced in cross-references, and distributed with every release.

**Name mapping** to use in examples:
- Agent forks → `your agent fork`, `a downstream agent`, or `agent-fork`
- Example person → `alice-example`, `charlie-example`, or `a-founder`
- Example company → `acme-example`, `widget-co`, or `a-company`
- Example fund → `fund-a`, `fund-b`, `fund-c`
- Example deal → `acme-seed`, `widget-series-a`
- Example meeting → `meetings/2026-04-03` (generic date is fine)
- Example user → `you` or `the user`, never a proper name

**Specific rule: never say `Wintermute` in any CHANGELOG, README, doc, PR, or
commit message.** When the temptation is to illustrate with the real fork name:
- Reader-facing copy → `your OpenClaw` (covers Wintermute, Hermes, AlphaClaw,
  and any other downstream OpenClaw deployment in one term the reader already
  recognizes).
- First-person / origin-story copy → `Garry's OpenClaw` (honest that this is
  the production deployment driving the feature, without exposing the private
  agent's name).

`Wintermute` may appear in private artifacts (scratch plans under
`~/.gstack/projects/…`, memory files, conversation transcripts, CEO-review
plans) — those aren't distributed. Anything checked into this repo or shipped
in a release must use the OpenClaw phrasing above. Sweeping a stale reference
is a small clean-up PR, not a debate.

**When in doubt, ask yourself:** "Would this query reveal private information
about the user's contacts, investments, or portfolio if it were read by a
stranger?" If yes, replace with generic placeholders.

**Illustrative API examples with household-brand companies** (Stripe, Brex, OpenAI,
GitHub, etc.) are fine — they're public entities, not contacts in anyone's brain.
Do not confuse illustrative API examples with queries that reveal real
relationships.

## Responsible-disclosure rule: don't broadcast attack surface in release notes

**When a release fixes a security gap or a user-impacting bug, describe the fix
functionally. Do not enumerate the attack surface, quantify the exposure window,
or highlight the most sensitive records by name in public-facing artifacts.**

Public-facing artifacts include: `CHANGELOG.md`, `README.md`, `docs/`, PR titles
and bodies, commit messages, GitHub issue titles and comments, release pages,
tweets, blog posts.

**Don't write:**
- "10 tables were publicly readable by the anon key for months, including X, Y, Z"
- "X and Y are the most sensitive ones"
- "N tables exposed. Fix: enable RLS on these specific tables: ..."

**Do write:**
- "Security hardening pass. Fresh installs secure by default. Existing brains
  brought to the same bar automatically on upgrade."
- "If `zbrain doctor` still flags anything after upgrade, the message names each
  table and gives the exact fix."

Why: anyone reading the release page before they've upgraded now has a directed
probe list for unpatched installs. The source code ships the specifics anyway
(`src/schema.sql`, `crates/zbrain-core/src/migrate.ts`, test fixtures) — reverse engineers can
get them. But the release page is a broadcast channel. Don't hand attackers a
curated list with a banner.

**The test:** if a reader with no prior context could read the release note and
walk away knowing "zbrain at version X has table Y readable by anon key until
they patch," the note is too specific. Rewrite until that's no longer possible.

**What IS fine in public artifacts:**
- The mechanism of the fix ("the check now scans every public table instead of
  a hardcoded allowlist").
- User-facing operator ergonomics (the escape-hatch SQL template, the upgrade
  commands, the breaking-change flag).
- Credit to contributors.
- Generic framing of severity ("security posture tightening pass") without
  quantification.

**What stays in private artifacts (plan files, private memories, internal docs):**
- Specific table names, record counts, exposure duration.
- Which records stand out as highest-risk.
- Detailed before/after tables in the "numbers that matter" format.

If the CEO/Eng review of a plan produces a detailed exposure table, keep it in
the plan file under `~/.claude/plans/` or `~/.gstack/projects/`. Don't copy it
into the CHANGELOG or PR body.

Applies retroactively: if you see a prior CHANGELOG entry naming attack-surface
specifics, scrub it as a small cleanup commit, the same way a stale Wintermute
reference gets swept.

## Schema state tracking

`~/.zbrain/update-state.json` tracks which recommended schema directories the user
adopted, declined, or added custom. The auto-update agent (SKILLPACK Section 17)
reads this during upgrades to suggest new schema additions without re-suggesting
things the user already declined. The setup skill writes the initial state during
Phase C/E. Never modify a user's custom directories or re-suggest declined ones.

## GitHub Actions SHA maintenance

All GitHub Actions in `.github/workflows/` are pinned to commit SHAs. Before shipping
(`/ship`) or reviewing (`/review`), check for stale pins and update them:

```bash
for action in actions/checkout oven-sh/setup-bun actions/upload-artifact actions/download-artifact softprops/action-gh-release gitleaks/gitleaks-action; do
  tag=$(grep -r "$action@" .github/workflows/ | head -1 | grep -o '#.*' | tr -d '# ')
  [ -n "$tag" ] && echo "$action@$tag: $(gh api repos/$action/git/ref/tags/$tag --jq .object.sha 2>/dev/null)"
done
```

If any SHA differs from what's in the workflow files, update the pin and version comment.

## PR descriptions cover the whole branch

Pull request titles and bodies must describe **everything in the PR diff against the
base branch**, not just the most recent commit you made. When you open or update a
PR, walk the full commit range with `git log --oneline <base>..<head>` and write the
body to cover all of it. Group by feature area (schema, code, tests, docs) — not
chronologically by commit.

This matters because reviewers read the PR body to understand what's shipping. If
the body only covers your last commit, they miss everything else and can't review
properly. A 7-commit PR with a body that describes commit 7 is worse than no body
at all — it actively misleads.

When in doubt, run `gh pr view <N> --json commits --jq '[.commits[].messageHeadline]'`
to see what's actually in the PR before writing the body.

## Community PR wave process

Never merge external PRs directly into master. Instead, use the "fix wave" workflow:

1. **Categorize** — group PRs by theme (bug fixes, features, infra, docs)
2. **Deduplicate** — if two PRs fix the same thing, pick the one that changes fewer
   lines. Close the other with a note pointing to the winner.
3. **Collector branch** — create a feature branch (e.g. `garrytan/fix-wave-N`), cherry-pick
   or manually re-implement the best fixes from each PR. Do NOT merge PR branches directly —
   read the diff, understand the fix, and write it yourself if needed.
4. **Test the wave** — verify with `bun test && bun run test:e2e` (full E2E lifecycle).
   Every fix in the wave must have test coverage.
5. **Close with context** — every closed PR gets a comment explaining why and what (if
   anything) supersedes it. Contributors did real work; respect that with clear communication
   and thank them.
6. **Ship as one PR** — single PR to master with all attributions preserved via
   `Co-Authored-By:` trailers. Include a summary of what merged and what closed.

**Community PR guardrails:**
- Always AskUserQuestion before accepting commits that touch voice, tone, or
  promotional material (README intro, CHANGELOG voice, skill templates).
- Never auto-merge PRs that remove YC references or "neutralize" the founder perspective.
- Preserve contributor attribution in commit messages.

## Checking out PRs from garrytan-agents

`garrytan-agents` is the AI-authored PR account and is NOT a collaborator on
this repo. Its PRs live in a fork, so GitHub Actions triggered by
`pull_request` events on those PRs do not receive base-repo secrets. Any CI
job that needs `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or similar will fail
with empty-env auth errors, regardless of what's set on the base repo. This
is a GitHub security default, not a config bug.

When the user says "check out <PR link>" and the PR is from `garrytan-agents`
(or any other non-collaborator fork), move the branch into the base repo
before running CI:

1. `gh pr checkout <N>` — pull down the fork's branch. Note the PR number and
   head branch name (`gh pr view <N> --json headRefName --jq .headRefName`).
2. `git push origin HEAD:<branch-name>` — push the same branch to the base
   repo (origin points at `garrytan/zbrain`, not the fork). This is the move
   that gives CI access to secrets.
3. `gh pr close <N> --comment "moving to base-repo branch for secret access"`
   — close the fork PR so the queue stays clean.
4. `gh pr create --base master --head <branch-name>` — open the replacement
   PR from the base-repo branch. **Preserve the original PR's title and body
   verbatim** (`gh pr view <N> --json title,body`); contributor attribution
   moves to a `Co-Authored-By:` trailer if needed.

Why this over alternatives: adding `garrytan-agents` as a collaborator, or
flipping the repo-wide "send secrets to fork PRs" toggle, both broaden
secret distribution to every fork PR from that account or any fork. Moving
the branch keeps secret scope tight to just the one PR being shipped.

## Skill routing

When the user's request matches an available skill, ALWAYS invoke it using the Skill
tool as your FIRST action. Do NOT answer directly, do NOT use other tools first.
The skill has specialized workflows that produce better results than ad-hoc answers.

**NEVER hand-roll ship operations.** Do not manually run git commit + push + gh pr
create when /ship is available. /ship handles VERSION bump, CHANGELOG, document-release,
pre-landing review, test coverage audit, and adversarial review. Manually creating a PR
skips all of these. If the user says "commit and ship", "push and ship", "bisect and
ship", or any combination that ends with shipping — invoke /ship and let it handle
everything including the commits. If the branch name contains a version (e.g.
`v0.5-live-sync`), /ship should use that version for the bump.

Key routing rules:
- Product ideas, "is this worth building", brainstorming → invoke office-hours
- Bugs, errors, "why is this broken", 500 errors → invoke investigate
- Ship, deploy, push, create PR, "commit and ship", "push and ship" → invoke ship
- QA, test the site, find bugs → invoke qa
- Code review, check my diff → invoke review
- Update docs after shipping → invoke document-release
- Weekly retro → invoke retro
- Design system, brand → invoke design-consultation
- Visual audit, design polish → invoke design-review
- Architecture review → invoke plan-eng-review
- Save progress, checkpoint, resume → invoke checkpoint
- Code quality, health check → invoke health
