#!/usr/bin/env bash
# scripts/typecheck-baseline.sh — tsc gate with a frozen error baseline.
#
# Why a baseline instead of a clean `tsc --noEmit`:
# During the TS -> Rust migration the TS tree is a live intermediate state.
# `tsc --noEmit` currently reports a fixed set of pre-existing errors (mostly
# TS2307 dangling imports to already-deleted command modules, plus some TS7006
# implicit-any in the doctor test cluster). Those are inherited debt, not
# regressions. This gate FAILS only when a *new* error appears on top of the
# frozen baseline — the same "block new, tolerate inherited" discipline used
# for the Phase 11 src/core deletions.
#
# The baseline lives in scripts/tsc-baseline.txt. Each line is a tsc error with
# the (line,col) location stripped, so ordinary edits that shift line numbers
# do NOT produce false positives. Duplicate lines are kept, so an added error
# of an existing kind/file still shows up as a diff.
#
# Usage:
#   bash scripts/typecheck-baseline.sh            # gate: exit 1 on new errors
#   bash scripts/typecheck-baseline.sh --update   # freeze current output as new baseline
#
# When you legitimately fix or delete baseline errors, re-run with --update and
# commit the shrunken baseline. When you migrate more TS away, the baseline
# should only ever get smaller.
set -uo pipefail
cd "$(dirname "$0")/.."

BASELINE="scripts/tsc-baseline.txt"
TSC=(node ./node_modules/typescript/bin/tsc --noEmit)

# Normalize tsc output into stable, location-free keys.
normalize() {
  "${TSC[@]}" 2>&1 | grep 'error TS' | sed -E 's/\(([0-9]+),([0-9]+)\)//' | sort
}

CURRENT="$(normalize)"

if [ "${1:-}" = "--update" ]; then
  printf '%s\n' "$CURRENT" > "$BASELINE"
  echo "[typecheck-baseline] baseline updated: $(printf '%s\n' "$CURRENT" | grep -c 'error TS') error(s)"
  exit 0
fi

if [ ! -f "$BASELINE" ]; then
  echo "[typecheck-baseline] ERROR: $BASELINE missing. Run with --update to create it." >&2
  exit 2
fi

# New errors = present now but not in baseline (comm -13).
NEW="$(comm -13 "$BASELINE" <(printf '%s\n' "$CURRENT"))"
# Fixed errors = in baseline but gone now (comm -23) — informational.
FIXED="$(comm -23 "$BASELINE" <(printf '%s\n' "$CURRENT"))"

if [ -n "$FIXED" ]; then
  echo "[typecheck-baseline] $(printf '%s\n' "$FIXED" | grep -c 'error TS') baseline error(s) no longer reproduce — consider: bash scripts/typecheck-baseline.sh --update" >&2
fi

if [ -n "$NEW" ]; then
  {
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "❌ typecheck: $(printf '%s\n' "$NEW" | grep -c 'error TS') NEW type error(s) above the frozen baseline"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    printf '%s\n' "$NEW"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Fix the error, or if intentional run: bash scripts/typecheck-baseline.sh --update"
  } >&2
  exit 1
fi

echo "[typecheck-baseline] OK — no new errors above baseline ($(grep -c 'error TS' "$BASELINE") inherited)"
exit 0
