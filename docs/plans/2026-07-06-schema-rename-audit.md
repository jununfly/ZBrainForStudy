# schema command rename audit (roadmap 1-4)

Status: grill complete (Q1–Q5 decided), TDD landed.
TS reference: `src/commands/schema.ts @ 5d5b404~1` (deleted in `5d5b404`, was 1166 lines).
Rust target: `crates/zbrain-cli/src/lib.rs` (`SchemaSql` command, `SchemaArgs`, `run_schema_command`).

## The reframe: this was never a flag-parity slice

The node started as "schema command strict TS flag parity". Investigating the
real TS implementation showed that premise was **false** — it rests on a
mis-read in the 2026-06-25 audit.

- `docs/plans/2026-06-25-config-bootstrap-entrypoint-audit.md` recorded
  `schema` as "~150 lines, Print database schema SQL" and spawned issue #37
  ("print libsql/postgres schema SQL").
- Git history contradicts this: `schema.ts` has been a **schema-pack manager**
  since at least `3c1cc8a~1` (795 lines, v0.38 Phase C), growing to 1166 lines
  at `3c1cc8a` (Schema Cathedral v3). It exposes a **32-verb taxonomy**
  (active/list/show/validate/use/detect/init/fork/diff/graph/lint/stats/sync/
  add-type/add-link-type/...). It never printed DDL.
- The real TS DDL capability is scattered across `apply-migrations` / `migrate`
  / `src/core/pglite-schema.ts` — there was never a TS command named `schema`
  that dumped DDL.

So the Rust `schema` command implemented the audit's *imagined* TS `schema`,
not the real one. Its own intent (walk `LIBQL_MIGRATIONS` / `POSTGRES_MIGRATIONS`
and print DDL) is clean and self-consistent — it just squatted on a name that
the real TS `schema` (a pack manager) owns.

There is no "TS flag parity" to reach because the two commands share zero
semantics. The honest move is a **naming fix + trace**, not fake parity.

## Decisions (Q1–Q5)

- **Q1 — scale/naming:** Rust `schema` is a DDL dumper with zero semantic
  overlap with TS `schema` (a 32-verb pack manager). Rename the Rust DDL dumper
  `schema` -> `schema-sql`; do NOT reproduce the 32 verbs. Free up `schema` for
  a future schema-pack port.
- **Q2 — alias:** No compatibility alias. Breaking rename, per `AGENTS.md`
  first-phase stance (no online users, no GBrain aliases/fallbacks). An alias
  would keep the DDL dumper squatting on `schema`.
- **Q3 — trace:** Code-side constant `UNMIGRATED_TS_SCHEMA_PACK_VERBS` (32
  verbs) + anchor test + this audit. Mirrors doctor's
  `UNMIGRATED_TS_DOCTOR_CHECKS` hard-trace pattern.
- **Q4 — DDL dumper flags:** Rename only; leave flags as-is (no `--json`).
  Output is plain SQL text with no JSON consumer (unlike doctor's CI contract);
  unknown backend already `exit(1)` fail-loud. Stay focused, no gold-plating.
- **Q5 — node rename / sub-node:** No sub-node. The old "flag parity" goal is a
  disproven premise, not deferred work. The schema-pack migration is out of the
  Part2 config/bootstrap scope; it is tracked by the code constant + a
  `FUTURE(schema-pack)` anchor comment for grep-back, not a roadmap node here.

## Implemented (TDD tracer bullets)

1. `schema_sql_command_parses_default` / `schema_sql_command_postgres_parses` —
   new `schema-sql` name parses; `SchemaSql(SchemaArgs)` with clap
   `#[command(name = "schema-sql")]`.
2. `bare_schema_name_is_no_longer_the_ddl_dumper` — `zbrain schema` now errors
   (breaking rename, no alias).
3. `unmigrated_ts_schema_pack_verbs_are_anchored` — constant has exactly 32
   verbs and contains representative entries (`add-link-type`,
   `review-candidates`); guards against silent removal / typos.

`run_executes_schema_stub` updated to `schema-sql`. `run` dispatch, `SchemaArgs`
doc comment, and command doc comment all updated.

## Trace mechanism for the future

- Constant: `UNMIGRATED_TS_SCHEMA_PACK_VERBS` in `crates/zbrain-cli/src/lib.rs`.
- Anchor comment: `FUTURE(schema-pack)` immediately above it.
- A later agent greps either token to find the tracking point. Migrating the
  manager = wire these verbs under a new `schema` subcommand tree and remove
  them from the constant; the anchor test then forces the list to stay honest.

## Not touched (deliberate)

- README's `zbrain schema use/detect/review-candidates/...` examples describe
  the TS schema-pack manager, which is still the running product. `schema` stays
  reserved for it, so those docs remain correct and are left unchanged.
- DDL dumper `--backend` aliasing (libsql/sqlite/pglite, postgres/pg) and its
  fail-loud unknown-backend path — already correct.
