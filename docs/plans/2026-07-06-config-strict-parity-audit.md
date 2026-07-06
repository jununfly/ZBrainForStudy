# Config strict TS flag parity audit

Date: 2026-07-06
Roadmap node: `1-2-1 Write config strict parity audit and test matrix`
Parent roadmap node: `1-2 config command strict TS flag parity`

## Scope

Audit only. Do not modify production config behavior in this slice.

Baseline: TS CLI historical user-visible behavior. Rust should either match it
or explicitly document a deliberate deviation and replacement path.

## Deliberate deviation (roadmap `1-2` Q2)

**TS used two storage planes**: `config show` read the file plane
(`loadConfig()` = `~/.zbrain/config.json` + env), while `config get` /
`set` / `unset` operated the DB plane (`engine.getConfig/setConfig`, a flat
key table). Rust unifies everything on the **file plane** (`config::load_config`
-> serde_yaml dot-path -> `config::write_config`, all against `zbrain.yml`).

This is an accepted, deliberate deviation:
- The file/DB split was a TS historical artifact; replicating it re-introduces
  complexity plus an engine/DB dependency for a command that should work with
  no database present.
- Migration principle allows documenting a deliberate deviation rather than
  copying legacy topology.

Consequence: TS validations that are only meaningful on the DB plane
(`embedding_model` / `embedding_dimensions` hard-reject, `search_embedding_column`
coverage gate, `embedding_columns` JSON validation, `search.mode` switch UX) are
**TS-only** and are not migrated in this subtree. They are recorded here for
traceability and intentionally left out of the implementation slices.

## Current Rust behavior

Source: `crates/zbrain-cli/src/lib.rs` (`run_config_command`, ~L1895)

- Subcommands present: `show`, `get <key>`, `set <key> <value>`,
  `unset <key>`, `unset --pattern <prefix>`.
- All operate the file plane via serde_yaml dot-path traversal.
- `show` redacts sensitive keys via `config::redact_value`.
- `get` **also redacts** the returned value (deviates from TS; see Gap 2).
- `get` not-found prints to stderr but **returns Ok (exit 0)** (deviates from
  TS; see Gap 1).
- `set` performs a **bare dot-path write**: any key, including a typo like
  `embeding.model`, is silently written as a new nested field (see Gap 3).

## TS historical user-visible surface

Evidence: `src/commands/config.ts` (recovered from `5d5b404~1`, 327 lines);
current `src/cli.ts` routes `config` to Rust with a stub.

- `config show` — no flags, prints all keys (redacted).
- `config get <key>` — prints raw value (NOT redacted), exit 1 if not found.
- `config set <key> <value>` — flags `--force` (bypass unknown-key reject),
  `--coverage-override` / `--yes` (bypass embedding coverage gate).
- `config unset <key>` and `config unset --pattern <prefix>`.
- No `list` / `edit` / `path` / `--json` / `--global` / `--local`.

| Area | TS-visible behavior | Rust status | Required parity action |
| --- | --- | --- | --- |
| Subcommand set | show/get/set/unset/unset --pattern | Present | None (parity holds) |
| get not-found | stderr message + exit 1 | Exit 0 | Gap 1: fail-loud exit code |
| get redaction | NOT redacted (raw value) | Redacted | Gap 2: stop redacting get |
| set unknown key | KNOWN_CONFIG_KEYS gate + Levenshtein hint + --force | Bare write | Gap 3: schema-gated set + --force |
| show redaction | redacted | Redacted | None (parity holds) |
| set value typing | stored as string (DB plane) | serde_yaml typed | Deviation (file plane); acceptable |
| embedding_* / coverage guards | DB-plane hard rejects/gates | Absent | TS-only; not migrated (Q2) |
| --json output | none | none | None (TS has no --json) |

## Parity gaps to fix in this subtree

- **Gap 1 (`1-2-3`)**: `config get <missing>` must fail loud with a non-zero
  exit code, matching TS `exit 1`. Rust currently returns Ok.
- **Gap 2 (`1-2-3`)**: `config get <key>` must NOT redact. `get` is an explicit
  single-value read used by scripts to read back secrets
  (`zbrain config get oauth_client_secret`); redaction breaks that use.
  `show` continues to redact (scrollback-leak protection).
- **Gap 3 (`1-2-2`)**: `config set <unknown/typo key>` must be rejected with a
  non-zero exit code instead of silently creating a stray nested field. The
  authoritative whitelist is the strongly-typed `Config` schema itself
  (attempt set, then round-trip back to `Config`; reject if the key is not a
  known field). `--force` provides the same escape hatch as TS. A Levenshtein
  "did you mean" hint is optional (schema validation already rejects).

## Test matrix

| Test group | Minimum Rust coverage |
| --- | --- |
| get not-found exit | `config get <missing>` returns a non-zero / Err result. |
| get no-redaction | `config get <sensitive key>` returns the raw stored value. |
| show still redacts | `config show` still redacts sensitive keys. |
| set unknown reject | `config set <typo key>` without --force is rejected and does not write the file. |
| set unknown with force | `config set <typo key> --force` writes the value. |
| set known key | `config set <valid key>` still succeeds and round-trips. |

## Roadmap child nodes

- `1-2-1 Write config strict parity audit and test matrix` (this doc)
- `1-2-2 Enforce config set schema validation and unknown-key gating`
- `1-2-3 Align config get not-found exit code and redaction semantics`

## Next implementation order

1. `1-2-2` set schema validation (write path safety first — closes the silent
   stray-field bug).
2. `1-2-3` get exit code + redaction alignment (read path semantics).
3. After each slice, run focused Rust CLI tests before broad validation.
