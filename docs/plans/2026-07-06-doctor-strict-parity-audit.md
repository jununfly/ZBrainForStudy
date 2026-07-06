# doctor command — strict TS parity audit (roadmap 1-3)

Status: grill complete (Q1–Q5 decided), ready for TDD.
TS source of truth: `src/commands/doctor.ts @ 5d5b404~1` (deleted in `5d5b404`, was ~6035 lines).
Rust target: `crates/zbrain-cli/src/lib.rs` (`run_doctor_command`, `DoctorArgs`, `DoctorCheck`, `CheckStatus`).

## The reframe (from "对齐 TS")

Investigating the real TS implementation changed the center of gravity of this slice.

**TS doctor never had `--offline`.** It is a flag Rust invented, declared on `DoctorArgs` but
bound as `_args` and ignored — a dead flag. The real TS contract is the `--json` envelope, which
Rust is missing entirely.

| Dimension        | TS (`5d5b404~1`)                                             | Rust (current)                          |
|------------------|-------------------------------------------------------------|-----------------------------------------|
| `--offline`      | ❌ does not exist                                            | ⚠️ declared but ignored (dead flag)     |
| `--json`         | ✅ `{schema_version:2, status, health_score, checks[]}`      | ❌ **missing entirely**                  |
| health_score     | ✅ `100 − fail*20 − warn*5`, clamp 0                          | ❌ only prints Pass/Warn/Fail counts     |
| status tri-state | ✅ healthy / warnings / unhealthy                            | ❌ none                                  |
| network check    | always runs, failure → warn, non-fatal                      | ✅ already aligned                       |
| exit code        | fail → 1, else → 0                                           | ✅ already aligned                       |

## Decisions (Q1–Q5)

- **Q1 — parity 尺度:** behavior-contract parity (exit-code semantics / `--json` schema / health_score),
  NOT reproducing the 70+ checks. Unmigrated checks must leave a trace (not silently omitted) so a
  later agent cannot mistake doctor for fully migrated.
- **Q2 — 留痕机制:** new `not-implemented` outcome explicitly lists unmigrated checks +
  code-side `UNMIGRATED_TS_DOCTOR_CHECKS` constant + a test anchor. Does NOT affect exit code or
  health_score (reporting `fail` would poison the exit code and make CI permanently red).
- **Q3 — 清单粒度:** subsystem-aggregated ~8-12 coarse entries, each labeled `covers N sub-checks`;
  the full 70+ detail stays in this audit doc.
- **Q4 — `--offline`:** **remove it.** TS never had it. Network check keeps its always-run,
  warn-on-failure, non-fatal semantics — that alone matches TS.
- **Q5 — `--json` check[] 对齐尺度:** envelope aligned field-for-field with TS
  (`schema_version:2` / `status` / `health_score` / `checks[]`); each `checks[]` entry uses the
  TS mandatory core subset `{name, status, message}` (`details/issues/remediation` deferred).
  Rust keeps its existing 4 real checks + injects the `UNMIGRATED_TS_DOCTOR_CHECKS` entries as
  `not-implemented`, visible but excluded from health_score / exit code.

## Contract to implement

### status mapping (TS `computeDoctorReport` line 80-92)
```
hasFail  -> "unhealthy"
hasWarn  -> "warnings"
else     -> "healthy"
```
`not-implemented` entries count as neither fail nor warn for status/score.

### health_score (TS `outputResults` line 5368-5385)
```
score = 100 - (fail_count * 20) - (warn_count * 5)   // clamp to [0, ..]
score = max(0, score)
```
`not-implemented` entries contribute 0.

### `--json` envelope (TS line 5382)
```json
{ "schema_version": 2, "status": "...", "health_score": 0-100, "checks": [ {"name","status","message"} ] }
```
When `--json` is set, ALL human-readable lines ("Running ZBrain doctor...", the ✅/⚠️/❌ lines,
"--- Summary ---") are suppressed; only the single JSON line is printed. Exit code unchanged.

### exit code (TS `runDoctor` line 5146)
`process.exit(hasFail ? 1 : 0)` — warn / not-implemented never trigger exit 1.

## UNMIGRATED_TS_DOCTOR_CHECKS (subsystem-aggregated, Q3)

Coarse entries surfaced as `not-implemented`; each covers a cluster of the TS 70+ checks.

| entry                  | covers (TS sub-checks, non-exhaustive)                                  |
|------------------------|-------------------------------------------------------------------------|
| embedding_health       | embedding provider reachability, embedding column, coverage backfill    |
| sync_freshness         | per-source lag, unacked parse failures, federated staleness             |
| reranker_health        | reranker provider / recipe check                                        |
| search_mode            | search modes overrides, mode drift (`search modes --reset`)             |
| federation_health      | federated source sync, mount reachability                               |
| schema_packs           | schema pack presence / drift                                            |
| resolver_health        | resolver conformance, check-resolvable mirror                           |
| skill_conformance      | skillpack-check, RESOLVER.md conformance                                |
| frontmatter_integrity  | bounded frontmatter scan, partial-state surfacing (v0.38.2.0)           |
| eval_drift             | whoknows eval regression, calibration profile staleness                 |
| brain_score            | 5-component brain-health composite                                      |
| takes_weight_grid      | takes.weight 0.05 grid integrity (v0.32 EXP-2)                          |

Full 70+ detail lives in TS history; migrate a subsystem → move its entry from the constant into a
real check, and the test anchor forces it to appear as a real (non-`not-implemented`) check.

## Test matrix (TDD tracer bullets, pure functions where possible)

1. `doctor_offline_flag_removed` — `Cli::try_parse_from(["zbrain","doctor","--offline"])` errors (flag gone).
2. `doctor_status_mapping` — pure fn: fail→unhealthy, warn(no fail)→warnings, clean→healthy;
   not-implemented alone → healthy.
3. `doctor_health_score` — pure fn: `100 − fail*20 − warn*5` clamp 0; not-implemented contributes 0.
4. `doctor_json_envelope_shape` — pure fn builds `{schema_version:2,status,health_score,checks[]}`,
   checks entries are `{name,status,message}`.
5. `doctor_json_includes_unmigrated_as_not_implemented` — envelope contains the ~8-12 entries with
   `status == "not-implemented"`, and they do not change status/score.
6. `unmigrated_checks_constant_is_nonempty_and_anchored` — `UNMIGRATED_TS_DOCTOR_CHECKS` len in
   expected band; guards against silent removal (Q2 hard trace for later agents).

## Implementation order

1. Remove `--offline` from `DoctorArgs`; update the parse test. (Q4)
2. Add `not-implemented` to `CheckStatus` (or a message-only convention — decide at impl; enum keeps
   it typed and testable, preferred). (Q2)
3. Add `UNMIGRATED_TS_DOCTOR_CHECKS` constant + `not-implemented` DoctorCheck constructor. (Q2/Q3)
4. Extract pure `doctor_status(&[DoctorCheck]) -> &str` and `doctor_health_score(&[DoctorCheck]) -> i64`. (Q5)
5. Extract pure `doctor_json_report(&[DoctorCheck]) -> serde_json::Value` envelope. (Q5)
6. Wire `--json` into `run_doctor_command`: suppress human text, print single JSON line, keep exit code. (Q5)
7. Inject the unmigrated entries into the check list. (Q2)
