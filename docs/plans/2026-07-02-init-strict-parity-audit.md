# Init strict TS flag parity audit

Date: 2026-07-02
Roadmap node: `1-1-1 Write init strict parity audit and test matrix`
Parent roadmap node: `1-1 init command strict TS flag parity`

## Scope

Audit only. Do not modify production init behavior in this slice.

Baseline: TS CLI historical user-visible behavior. Rust should either match it or explicitly document a deliberate deviation and replacement path.

## Current Rust behavior

Source: `crates/zbrain-cli/src/lib.rs`

- `InitArgs` currently exposes only `--force`.
- `run_init_command` creates config directory, initializes libsql/PGLite-like DB at `~/.zbrain/brain.pglite`, runs `init_schema`, writes config, and prints success text.
- Existing config exits successfully unless `--force` is supplied.
- Existing-config message mentions `zbrain init --migrate-only`, but Rust does not parse `--migrate-only`.

## TS historical user-visible surface

Evidence sources:

- `src/cli.ts` help: `init [--pglite|--supabase|--url]`.
- `src/cli.ts` dispatcher now routes `init` to Rust and treats TS init as replaced.
- `tests/unit/init*.test.ts` and `tests/unit/e2e/init-fresh-pglite.test.ts`.
- Migration commands under `src/commands/migrations/*` reference `zbrain init --migrate-only`.

| Area | TS-visible flags / behavior | Rust status | Required parity action |
| --- | --- | --- | --- |
| Engine selection | `--pglite`, `--supabase`, `--url <connection_string>` | Missing | Add parser tests and implement or fail-loud stub. |
| Reinit safety | `--force` | Partial | Verify existing-config and overwrite semantics. |
| Schema-only migration | `--migrate-only` | Missing | Must parse and avoid config rewrite. |
| Thin client | `--mcp-only`, `--issuer-url`, `--mcp-url`, `--oauth-client-id`, `--oauth-client-secret` | Missing | Preserve thin-client setup or explicit replacement. |
| Structured output | `--json` | Missing | Required for automation/tests. |
| Non-interactive install | `--non-interactive` | Missing | Fail-loud when provider setup is impossible. |
| Embedding setup | `--embedding-model`, `--embedding-dimensions`, `--no-embedding` | Missing | Preserve dimension validation and deferred setup sentinel. |

## Test matrix

| Test group | Minimum Rust coverage |
| --- | --- |
| Parser parity | `Cli::try_parse_from` accepts all TS-visible init flags and rejects invalid conflicts. |
| PGLite init | Fresh `init --pglite` writes local config and initializes schema. |
| URL/Postgres init | `init --url <connection>` and `init --supabase` behavior is implemented or explicitly blocked. |
| Migrate-only | `init --migrate-only` requires existing config, runs schema migration, and does not rewrite config. |
| Thin client | `init --mcp-only --json ...` writes remote MCP config and does not create local DB. |
| Embedding | `--no-embedding` writes disabled sentinel; invalid dimensions fail before disk writes. |
| Existing config | Existing config without `--force` refuses destructive overwrite; with `--force` follows documented semantics. |

## Roadmap child nodes to add

- `1-1-2 Add init parser parity tests`
- `1-1-3 Implement engine selection flags for init`
- `1-1-4 Implement migrate-only init behavior`
- `1-1-5 Implement thin-client MCP-only init behavior`
- `1-1-6 Implement init embedding setup flags`
- `1-1-7 Validate init existing-config and JSON output behavior`

## Next implementation order

1. Add parser parity tests first, without production changes.
2. Implement the narrowest parser surface needed to make tests compile and fail at behavior boundaries.
3. Implement behavior slices in this order: engine selection, migrate-only, thin-client MCP-only, embedding setup, existing-config/JSON output.
4. After each slice, run the focused Rust CLI tests before broad validation.
