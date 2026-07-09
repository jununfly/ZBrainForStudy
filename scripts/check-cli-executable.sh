#!/bin/bash
# CI guard: bin/zbrain-rs.js must be tracked by git in executable mode (100755).
#
# Why: bun-link installs a symlink to this file (the package.json "zbrain" bin
# entry). If the mode bit regresses to 100644, the very first `zbrain --version`
# invocation fails with `permission denied`.
#
# Wired into `bun run verify`. Fast, no external deps.
set -e

MODE=$(git ls-files --stage bin/zbrain-rs.js | awk '{print $1}')
if [ "$MODE" != "100755" ]; then
  echo "FAIL: bin/zbrain-rs.js is tracked at mode $MODE; expected 100755 (executable)."
  echo ""
  echo "Fix: chmod +x bin/zbrain-rs.js && git add --chmod=+x bin/zbrain-rs.js"
  echo ""
  echo "Background: bun-link installs symlink to this file directly. Mode 100644"
  echo "produces 'permission denied' on first invocation."
  exit 1
fi

echo "OK: bin/zbrain-rs.js is git-tracked as executable (100755)"
