#!/usr/bin/env bash
# Phase 11 (1-1) cutover assertions — the executable "test" for a config change.
#
# TDD framing: this is the RED→GREEN spec for the package.json cutover.
# Run BEFORE cutover  → expected FAIL (JS-library identity still present).
# Run AFTER  cutover  → expected PASS (JS-library identity fully removed).
#
# Scope: package.json contract only. Rust regression is guarded separately
# by `cargo build -p zbrain-cli` + `zbrain --help` smoke.
#
# Throwaway harness: delete in 1-4 finalization (registered in the Part9 roadmap).
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
pass() { echo "  PASS  $1"; }
bad()  { echo "  FAIL  $1"; fail=1; }

echo "=== Phase11 1-1 cutover assertions ==="

# A1: package.json has NO "main" key (JS-library entry removed)
if node -e 'process.exit(require("./package.json").main===undefined?0:1)'; then
  pass 'package.json has no "main"'
else
  bad  'package.json still has "main"'
fi

# A2: package.json has NO "exports" (public API surface removed)
if node -e 'process.exit(require("./package.json").exports===undefined?0:1)'; then
  pass 'package.json has no "exports"'
else
  bad  'package.json still has "exports"'
fi

# A3: openclaw.extensions removed (dangling → openclaw-context-engine.ts is gone)
if node -e 'const p=require("./package.json");process.exit((p.openclaw&&p.openclaw.extensions)?1:0)'; then
  pass 'no openclaw.extensions'
else
  bad  'openclaw.extensions still present (dangling)'
fi

# A4: dangling scripts removed (postinstall → deleted postinstall.ts; dev → src/cli.ts to be deleted)
if node -e 'const s=require("./package.json").scripts||{};process.exit(s.postinstall?1:0)'; then
  pass 'no scripts.postinstall'
else
  bad  'scripts.postinstall still present (dangling postinstall.ts)'
fi

# A5: exports-count guard removed from disk
if [ ! -f scripts/check-exports-count.sh ]; then
  pass 'scripts/check-exports-count.sh deleted'
else
  bad  'scripts/check-exports-count.sh still exists'
fi

# A6: check:all no longer invokes the exports guard
if ! grep -q 'check-exports-count' package.json; then
  pass 'package.json no longer references check-exports-count'
else
  bad  'package.json still references check-exports-count'
fi

# A7: bin still points at the Rust CLI wrapper (identity we KEEP)
if node -e 'const b=require("./package.json").bin||{};process.exit(b.zbrain==="bin/zbrain-rs.js"?0:1)'; then
  pass 'bin.zbrain still = bin/zbrain-rs.js (Rust CLI kept)'
else
  bad  'bin.zbrain changed/removed — MUST keep Rust CLI'
fi

echo "==="
if [ "$fail" -eq 0 ]; then
  echo "RESULT: GREEN (cutover complete)"; exit 0
else
  echo "RESULT: RED (cutover not yet done)"; exit 1
fi
