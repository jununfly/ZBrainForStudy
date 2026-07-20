/**
 * zbrain-bin.ts — resolve the path to the `zbrain` Rust binary for
 * subprocess invocation from TS code (skillify-check, e2e tests).
 *
 * The Rust CLI is the canonical entrypoint for `check-resolvable`,
 * `check-brain-first`, etc. TS code that needs to shell out to it must
 * locate the binary. In deployed installs `zbrain` is on PATH; in the
 * dev/test tree it lives at `target/<profile>/zbrain[.exe]`. This helper
 * mirrors the resolution that `scripts/e2e-mounts-smoke.sh` hard-codes
 * (`$ROOT/target/debug/zbrain.exe`), but prefers an explicit override and
 * falls back to PATH so production behavior is unchanged.
 *
 * Resolution order:
 *   1. $ZBRAIN_BIN        — explicit operator override (absolute path)
 *   2. target/debug/zbrain[.exe]    — dev build (exists in-repo)
 *   3. target/release/zbrain[.exe]  — release build
 *   4. 'zbrain'           — fallback to PATH (deployed installs)
 */

import { existsSync } from 'fs';
import { dirname, join } from 'path';

function projectRoot(): string {
  let dir = process.cwd();
  for (let i = 0; i < 20; i++) {
    if (existsSync(join(dir, 'package.json'))) return dir;
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return process.cwd();
}

export function resolveZbrainBin(): string {
  const env = process.env.ZBRAIN_BIN;
  if (env && existsSync(env)) return env;

  const root = projectRoot();
  const candidates = [
    'target/debug/zbrain',
    'target/debug/zbrain.exe',
    'target/release/zbrain',
    'target/release/zbrain.exe',
  ];
  for (const sub of candidates) {
    const p = join(root, sub);
    if (existsSync(p)) return p;
  }
  return 'zbrain';
}
