#!/usr/bin/env bash
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/zbrain.exe"
export ZBRAIN_HOME="C:/tmp/zbrain-e2e-mounts-$$"
rm -rf "$ZBRAIN_HOME"
mkdir -p "$ZBRAIN_HOME"
CLONE="$ZBRAIN_HOME/yc"
mkdir -p "$CLONE"
OTHER="$ZBRAIN_HOME/other"
mkdir -p "$OTHER"

pass=0; fail=0
check() { # desc, expected-substr, actual
  local desc="$1" exp="$2" act="$3"
  if printf '%s' "$act" | grep -q -- "$exp"; then
    echo "PASS: $desc"; pass=$((pass+1))
  else
    echo "FAIL: $desc (expected substring '$exp')"; echo "  got: $act"; fail=$((fail+1))
  fi
}

# 1. add postgres mount (with password -> must be redacted on list --json)
OUT=$("$BIN" mounts add yc-media --path "$CLONE" --engine postgres --db-url "postgres://u:secretpw@db.example.com:5432/yc")
echo "[add] $OUT"
# 2. list (human) shows id
OUT=$("$BIN" mounts list)
check "list shows id" "yc-media" "$OUT"
# 3. list --json redacts db_url password
OUT=$("$BIN" mounts list --json)
check "json redacts password" "postgres://u:\*\*\*@db.example.com:5432/yc" "$OUT"
# 4. duplicate id rejected
OUT=$("$BIN" mounts add yc-media --path "$OTHER" --engine pglite --db-path "$OTHER/.pglite" 2>&1); RC=$?
check "duplicate id rejected (rc!=0)" "already exists" "$OUT"; [ $RC -ne 0 ] && echo "  (rc=$RC ok)"
# 5. host reserved rejected
OUT=$("$BIN" mounts add host --path "$OTHER" --engine pglite --db-path "$OTHER/.pglite" 2>&1); RC=$?
check "host reserved rejected" "Reserved mount id" "$OUT"; [ $RC -ne 0 ] && echo "  (rc=$RC ok)"
# 6. enable then disable cycle
"$BIN" mounts disable yc-media 2>&1
OUT=$("$BIN" mounts list --json)
check "disable sets enabled=false" '"enabled": false' "$OUT"
"$BIN" mounts enable yc-media 2>&1
OUT=$("$BIN" mounts list --json)
check "enable sets enabled=true" '"enabled": true' "$OUT"
# 7. idempotent enable
"$BIN" mounts enable yc-media 2>&1
OUT=$("$BIN" mounts list --json)
check "idempotent enable" '"enabled": true' "$OUT"
# 8. trust-frontmatter cycle preserves fields
"$BIN" mounts trust-frontmatter yc-media 2>&1
OUT=$("$BIN" mounts list --json)
check "trust sets flag true" '"trust_frontmatter_overrides": true' "$OUT"
"$BIN" mounts untrust-frontmatter yc-media 2>&1
OUT=$("$BIN" mounts list --json)
# false is omitted via skip_serializing_if (faithful to TS default), so assert it is NOT true
if printf '%s' "$OUT" | grep -q '"trust_frontmatter_overrides": true'; then
  echo "FAIL: untrust still true"; fail=$((fail+1))
else
  echo "PASS: untrust sets flag false (omitted)"; pass=$((pass+1))
fi
# 9. add second mount (pglite) -> duplicate path rejected
"$BIN" mounts add other --path "$OTHER" --engine pglite --db-path "$OTHER/.pglite" 2>&1
OUT=$("$BIN" mounts add dup-path --path "$OTHER" --engine pglite --db-path "$OTHER/.pglite" 2>&1); RC=$?
check "duplicate path rejected" "path" "$OUT"; [ $RC -ne 0 ] && echo "  (rc=$RC ok)"
# 10. remove then list empty of that id
"$BIN" mounts remove yc-media 2>&1
OUT=$("$BIN" mounts list)
if printf '%s' "$OUT" | grep -q "yc-media"; then
  echo "FAIL: remove did not drop yc-media"; fail=$((fail+1))
else
  echo "PASS: remove dropped yc-media"; pass=$((pass+1))
fi
# 11. list --json valid JSON
OUT=$("$BIN" mounts list --json)
printf '%s' "$OUT" | python3 -c "import sys,json; json.load(sys.stdin)" 2>/dev/null && echo "PASS: list --json is valid JSON" || { echo "FAIL: invalid JSON"; fail=$((fail+1)); }

echo "━━━━━━━━━━━━━━━━━━━━━"
echo "E2E mounts: pass=$pass fail=$fail"
rm -rf "$ZBRAIN_HOME"
[ "$fail" -eq 0 ]
