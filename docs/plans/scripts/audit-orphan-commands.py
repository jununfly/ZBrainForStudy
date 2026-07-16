#!/usr/bin/env python3
"""
1-6-1 Orphan command audit.

Cross-references the TS live dispatch surface (src/cli.ts) against the Rust
`Commands` enum (crates/zbrain-cli/src/lib.rs) and classifies every TS command
into one of four buckets:

  RUST_OWNED     - Rust already implements an equivalent command. The TS copy is
                   a duplicate shell / error stub. Delete the TS copy ONLY after
                   the parity gate (zero src + zero test references + real,
                   non-stub Rust coverage).
  TRIVIAL_DELETE - Pure TS utility with no domain value worth porting (dev
                   tooling, diagnostics, one-shot scripts). Delete entirely.
  REAL_MIGRATE   - Real domain functionality with no Rust equivalent. Needs a
                   port to Rust (spawns its own sub-slice later).
  PARITY_REVIEW  - Could be RUST_OWNED but the Rust coverage must be confirmed
                   (feature parity / not a stub) before deletion.

Run:  python docs/plans/scripts/audit-orphan-commands.py
Emits markdown to stdout and (optionally) writes docs/plans/1-6-orphan-audit.md
when given --write.

CORRECTION (2026-07-16, 1-6-3 PARITY_GATE pass):
  This original classifier bucketed TRIVIAL_DELETE purely by command *semantics*
  ("is it a dev/diagnostic tool?") and did NOT check test references. The
  PARITY_GATE pass (docs/plans/scripts/audit-trivial-deps.py) found that most
  TRIVIAL_DELETE candidates still have TEST references — test files directly
  `import` the command module's exported functions as library units. Deleting
  those modules breaks the tests, so they are NOT trivially deletable; they
  belong with 1-6-4 REAL_MIGRATE (port + migrate/retire the tests alongside the
  Rust port). Truly zero-dep deletions found: cache / claw-test / report.
  Also corrected below: discovery / network / parse are NOT commands — they were
  mis-scraped from a RemoteMcpError.reason switch (cli.ts ~324-365). `call` is a
  ghost CLI_ONLY entry with no handler (removed from the set, nothing to delete).
  The real deletion gate is: zero src refs AND zero test refs AND (for
  RUST_OWNED) real non-stub Rust coverage.
"""

import sys

# ---------------------------------------------------------------------------
# Rust `Commands` enum — top-level variants (kebab-case CLI names).
# Subcommand groups (jobs/agent/schema/sources/facts/links/takes) are noted
# where they matter for equivalence.
# ---------------------------------------------------------------------------
RUST_CMDS = {
    "init", "doctor", "config", "schema-sql", "get-page", "think", "query",
    "put-page", "delete-page", "restore-page", "purge-deleted-pages",
    "list-pages", "serve-mcp", "serve", "sync", "sources", "capture", "facts",
    "links", "takes", "salience", "orphans", "graph-query", "autopilot",
    "remote", "jobs", "agent", "schema",
}
# Rust subcommand groups that absorb TS verbs.
RUST_GROUPS = {
    "jobs": ["submit", "list", "get", "cancel", "retry", "prune", "stats"],
    "agent": ["run", "list", "get", "cancel", "retry", "prune", "logs"],
    "schema": ["inspect", "validate", "lint", "activate", "author", "discover", "repair"],
    "sources": ["add", "list", "remove", "status", "capture"],
    "facts": ["insert", "list", "health", "expire"],
    "links": ["add", "remove", "list", "reconcile"],
    "takes": ["add", "list", "remove", "get", "adjust"],
}

# ---------------------------------------------------------------------------
# TS live dispatch surface (union of cliOps switch cases + CLI_ONLY set +
# special-case stubs). Sourced from grep of src/cli.ts @ 2026-07-16.
# ---------------------------------------------------------------------------
TS_LIVE = [
    # --- cliOps engine-op switch (src/cli.ts ~611-705) ---
    "anomalies", "auth", "backfill", "book-mirror", "brainstorm", "calibration",
    "code-callees", "code-callers", "code-def", "code-refs", "config",
    "discovery", "edges-backfill", "embed", "eval", "export", "extract",
    "extract-conversation-facts", "features", "files", "forget", "founder",
    "import", "lsd", "migrate", "models", "network", "notability-eval",
    "orphans", "pages", "parse", "query", "recall", "reconcile-links",
    "reindex", "reindex-code", "reindex-frontmatter", "search", "storage",
    "sync", "takes", "transcripts", "whoknows",
    # --- CLI_ONLY set (handleCliOnly) ---
    "reinit-pglite", "upgrade", "post-upgrade", "check-update", "integrations",
    "publish", "check-backlinks", "lint", "report", "files", "embed", "call",
    "migrate", "eval", "sync", "extract", "extract-conversation-facts",
    "features", "apply-migrations", "skillpack-check", "skillpack", "resolvers",
    "integrity", "repair-jsonb", "orphans", "mounts", "dream",
    "check-resolvable", "routing-eval", "skillify", "smoke-test", "providers",
    "storage", "code-def", "code-refs", "reindex", "reindex-code",
    "reindex-frontmatter", "code-callers", "code-callees", "frontmatter",
    "auth", "friction", "claw-test", "book-mirror", "takes", "anomalies",
    "transcripts", "models", "recall", "forget", "edges-backfill", "cache",
    "ze-switch", "founder", "brainstorm", "lsd",
    # --- special-case stubs / aliases ---
    "schema", "init", "doctor", "ask", "get_page", "list_pages", "get_stats",
    "get_health", "get_timeline", "get_versions", "get_tags",
]

# Map TS command -> (category, rust_equiv, rationale)
# rust_equiv is the Rust command name if RUST_OWNED / PARITY_REVIEW, else "".
C = {}
def add(cmd, cat, rust, why):
    C[cmd] = (cat, rust, why)

# --- RUST_OWNED: clear Rust equivalent, delete TS copy after parity gate ---
add("config", "RUST_OWNED", "config", "Rust Config subcommand covers config get/set/list.")
add("query", "RUST_OWNED", "query", "Rust Query implements lexical+vector search.")
add("search", "RUST_OWNED", "query", "TS search is keyword search; Rust Query superset.")
add("get_page", "RUST_OWNED", "get-page", "Rust GetPage reads by slug.")
add("pages", "RUST_OWNED", "get-page", "TS 'pages' case == get_page by slug.")
add("list_pages", "RUST_OWNED", "list-pages", "Rust ListPages lists with filters.")
add("think", "RUST_OWNED", "think", "Rust Think synthesizes answers.")
add("sync", "RUST_OWNED", "sync", "Rust Sync migrates a git repo into KB.")
add("takes", "RUST_OWNED", "takes", "Rust Takes group (add/list/remove/get/adjust).")
add("orphans", "RUST_OWNED", "orphans", "Rust Orphans finds zero-inbound pages.")
add("import", "RUST_OWNED", "capture", "TS import -> Rust Capture (import_from_content).")
add("schema", "RUST_OWNED", "schema", "Rust Schema group is the pack manager; TS is error stub.")
add("init", "RUST_OWNED", "init", "Rust Init; TS is error stub ('use Rust CLI').")
add("doctor", "RUST_OWNED", "doctor", "Rust Doctor; TS doctor.ts deleted @5d5b404. CLI_ONLY entry is dangling.")
add("reconcile-links", "RUST_OWNED", "links", "Rust Links group has reconcile verb.")
add("skillpack", "RUST_OWNED", "schema", "Rust Schema group supersedes TS skillpack (G4 resolved).")
add("skillpack-check", "RUST_OWNED", "schema", "Rust Schema validate/lint supersede skillpack-check.")

# --- TRIVIAL_DELETE: pure dev/diagnostic utility, no domain value to port ---
# NOTE: see CORRECTION in module docstring. Only cache/claw-test/report are
# truly zero-dep. The rest carry TEST references (tests import the module's
# exported functions) and must migrate with 1-6-4, not delete here.
for cmd, why in [
    ("lint", "Code linting dev tool. [1-6-3 gate: 4 test refs -> 1-6-4]"),
    ("upgrade", "Self-update shim. [1-6-3 gate: 4 test refs -> 1-6-4]"),
    ("post-upgrade", "Post-update hook shim (upgrade.ts). [4 test refs -> 1-6-4]"),
    ("check-update", "Update availability check. [2 test refs -> 1-6-4]"),
    ("reinit-pglite", "Dev wipe-and-reinit. [3 test refs + embedding-dim-check.ts hint -> 1-6-4]"),
    ("apply-migrations", "DB migration runner. [10 test refs -> 1-6-4]"),
    ("repair-jsonb", "Dev JSONB repair utility. [2 test refs -> 1-6-4]"),
    ("report", "Diagnostic report generator. [1-6-3 gate: zero-dep, DELETED @1-6-3]"),
    ("smoke-test", "Dev smoke test (inline handler, no module). [defer -> 1-6-4]"),
    ("claw-test", "Dev claw test harness (CLI shell). [1-6-3 gate: zero-dep, DELETED @1-6-3]"),
    ("ze-switch", "Manual ZE-default switch lever. [1 test ref -> 1-6-4]"),
    ("check-backlinks", "TS-only backlink check. [3 test refs -> 1-6-4]"),
    ("cache", "Cache ops dev utility. [1-6-3 gate: zero-dep, DELETED @1-6-3]"),
    ("friction", "Friction-log dev tool. [2 test refs -> 1-6-4]"),
    ("lsd", "LSD ideation (re-exports brainstorm.ts). [re-export dep -> 1-6-4]"),
    ("founder", "Founder scorecard. [2 test refs -> 1-6-4]"),
    ("files", "File listing utility. [1 test ref -> 1-6-4]"),
    ("anomalies", "Anomaly detection. [operations.ts op-shadow (find_anomalies) -> 1-6-4]"),
    ("transcripts", "Transcript ops. [operations.ts op-shadow (get_recent_transcripts) -> 1-6-4]"),
    ("book-mirror", "Book mirroring dev tool. [1 test ref -> 1-6-4]"),
    ("integrations", "Integration listing. [4 test refs -> 1-6-4]"),
    ("frontmatter", "Frontmatter ops. [2 test refs -> 1-6-4]"),
    ("mounts", "Mount-engine dev ops. [6 test refs -> 1-6-4]"),
]:
    add(cmd, "TRIVIAL_DELETE", "", why)

# --- NON_COMMAND: mis-scraped entries that are NOT CLI commands ---
for cmd, why in [
    ("discovery", "NOT a command — RemoteMcpError.reason switch case (cli.ts ~328)."),
    ("network", "NOT a command — RemoteMcpError.reason switch case (cli.ts ~341)."),
    ("parse", "NOT a command — RemoteMcpError.reason switch case (cli.ts ~363)."),
    ("call", "Ghost CLI_ONLY entry, no handler. Removed from CLI_ONLY @1-6-3."),
]:
    add(cmd, "NON_COMMAND", "", why)

# --- REAL_MIGRATE: real domain functionality, no Rust equivalent ---
for cmd, why in [
    ("whoknows", "Real natural-language 'who knows about X' query feature."),
    ("brainstorm", "Real ideation feature."),
    ("dream", "Real generative feature."),
    ("models", "Model registry management (real)."),
    ("providers", "Provider config management (real; Rust has config.providers map only)."),
    ("publish", "Publishing pipeline (real)."),
    ("resolvers", "Resolver management (real; G45)."),
    ("integrity", "Integrity check (real; G39)."),
    ("auth", "Auth flow (real; Rust has no auth command)."),
    ("eval", "Eval harness (real; Part11 1-2 blocked)."),
    ("recall", "Memory recall (real; no Rust equivalent)."),
    ("forget", "Memory forget/prune (real; no Rust equivalent)."),
    ("extract", "Content extraction (real)."),
    ("extract-conversation-facts", "Conversation fact extraction (real)."),
    ("routing-eval", "Routing eval (real; G45-ish)."),
    ("check-resolvable", "Resolvable check (real; G45)."),
    ("skillify", "Skillify (real)."),
    ("embed", "Embedding ops (real; Rust has embeddings internally)."),
    ("export", "KB export (real; no Rust export command)."),
    ("migrate", "Data migration (real; Rust handles internally but no CLI)."),
    ("features", "Feature-flag management (real)."),
    ("storage", "Storage backend ops (real; Rust owns backend but no CLI)."),
    ("code-def", "Code definition lookup (code-intel domain)."),
    ("code-refs", "Code references lookup (code-intel domain)."),
    ("code-callers", "Code callers lookup (code-intel domain)."),
    ("code-callees", "Code callees lookup (code-intel domain)."),
    ("reindex", "Code reindex (code-intel domain)."),
    ("reindex-code", "Code reindex (code-intel domain)."),
    ("reindex-frontmatter", "Frontmatter reindex (code-intel domain)."),
    ("backfill", "Backfill job (real)."),
    ("edges-backfill", "Edges backfill (real)."),
    ("calibration", "Calibration (Part11 1-3-3 blocked Phase2)."),
    ("notability-eval", "Notability eval variant (real)."),
]:
    add(cmd, "REAL_MIGRATE", "", why)

# --- PARITY_REVIEW: could be RUST_OWNED but must confirm coverage ---
for cmd, why in [
    ("get_stats", "Rust has stats via doctor/engine; no standalone get-stats command. Review."),
    ("get_health", "Rust Doctor covers health; no standalone get-health. Review."),
    ("get_timeline", "Rust has salience/graph-query; timeline may be covered. Review."),
    ("get_versions", "Rust has versioning internally; no CLI. Review."),
    ("get_tags", "Rust has tags internally; no CLI. Review."),
    ("ask", "Alias for query; collapses into Rust Query once query deleted."),
]:
    add(cmd, "PARITY_REVIEW", "", why)

# ---------------------------------------------------------------------------
# Emit
# ---------------------------------------------------------------------------
def main():
    write = "--write" in sys.argv
    lines = []
    lines.append("# 1-6 Orphan Command Audit")
    lines.append("")
    lines.append("Cross-reference of the TS live dispatch surface (`src/cli.ts`) "
                 "against the Rust `Commands` enum (`crates/zbrain-cli/src/lib.rs`).")
    lines.append("")
    lines.append("Generated by `docs/plans/scripts/audit-orphan-commands.py` @ 2026-07-16.")
    lines.append("")

    counts = {}
    for cmd in C:
        cat = C[cmd][0]
        counts[cat] = counts.get(cat, 0) + 1
    lines.append("## Summary")
    lines.append("")
    for cat in ["RUST_OWNED", "TRIVIAL_DELETE", "REAL_MIGRATE", "PARITY_REVIEW", "NON_COMMAND"]:
        lines.append(f"- **{cat}**: {counts.get(cat, 0)}")
    lines.append(f"- **Total classified**: {len(C)}")
    lines.append("")

    lines.append("## Classification table")
    lines.append("")
    lines.append("| Command | Category | Rust equivalent | Rationale |")
    lines.append("|---------|----------|-----------------|-----------|")
    # stable order: by category then name
    order = {"RUST_OWNED": 0, "PARITY_REVIEW": 1, "TRIVIAL_DELETE": 2, "REAL_MIGRATE": 3, "NON_COMMAND": 4}
    for cmd in sorted(C.keys(), key=lambda c: (order[C[c][0]], c)):
        cat, rust, why = C[cmd]
        r = rust or "—"
        lines.append(f"| `{cmd}` | {cat} | {r} | {why} |")
    lines.append("")

    lines.append("## Slicing plan (roadmap sub-structure under 1-6)")
    lines.append("")
    lines.append("- **1-6-2 RUST_OWNED shell cleanup** — delete TS copies of "
                 "RUST_OWNED commands, each gated by the parity check "
                 "(zero src + zero test references + real non-stub Rust coverage).")
    lines.append("- **1-6-3 TRIVIAL_DELETE batch** — delete TRIVIAL_DELETE "
                 "dev/diagnostic utilities entirely (no Rust needed).")
    lines.append("- **1-6-4 REAL_MIGRATE track** — port REAL_MIGRATE commands to "
                 "Rust; itself spawns per-command sub-slices grouped by domain "
                 "(memory: recall/forget; model/provider: models/providers; "
                 "content: extract/embed/export; resolver: resolvers/integrity/"
                 "routing-eval/check-resolvable/skillify; code-intel: code-*/"
                 "reindex-*; misc: whoknows/brainstorm/dream/auth/eval/...).")
    lines.append("- **1-6-5 PARITY_GATE** — shared verification gate for 1-6-2/1-6-3: "
                 "before deleting any TS command, confirm Rust coverage is real "
                 "and no surviving src/test references remain.")
    lines.append("")

    out = "\n".join(lines)
    if write:
        path = "docs/plans/1-6-orphan-audit.md"
        with open(path, "w", encoding="utf-8") as f:
            f.write(out)
        print(f"wrote {path}")
    else:
        print(out)

if __name__ == "__main__":
    main()
