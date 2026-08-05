/**
 * E2E helper — spawn the real ZBrain Rust binary (`zbrain`) as a subprocess.
 *
 * This is the re-route infrastructure for #143. Historically the E2E suite
 * imported TS command functions (e.g. `runDream`, `runCycle`) and invoked
 * them in-process. As the TS commands are deleted and the Rust binary becomes
 * the single shipped artifact, E2E tests must drive the ACTUAL binary — the
 * thing users run — rather than in-process function calls. That validates the
 * real CLI surface and end-to-end engine wiring, not just unit-level logic.
 *
 * Resolution order (resolveZbrainBin):
 *   1. process.env.ZBRAIN_BIN — explicit override. Local dev points this at a
 *      prebuilt binary in an unwatched temp dir, because the default
 *      target/ directory is locked by a file watcher in some dev environments
 *      (and thus cannot be built there). See project memory: the Windows
 *      watcher-lock workaround builds into C:/Users/.../AppData/Local/Temp/
 *      zb_targetN with a Windows-style (not msys C:/c/...) CARGO_TARGET_DIR.
 *   2. Default cargo output locations under <root>/target/ — what CI builds.
 *      Mirrors bin/zbrain-rs.js getBinaryCandidates (platform/arch triple,
 *      release before debug, both triple-specific and bare).
 *   3. null — the caller should skip the test. We do NOT hard-fail and do NOT
 *      fall back to `cargo build` here: a test-triggered build is slow and
 *      would wedge under the watcher lock. CI builds the binary first, so it
 *      resolves via (2); a dev without a binary gets a clean skip instead of
 *      a red suite.
 *
 * NOTE: we spawn the raw binary directly (not bin/zbrain-rs.js), because the
 * wrapper uses stdio:'inherit' and cannot capture stdout/stderr for asserts.
 * On Windows we do NOT use shell:true (cmd.exe re-parses argv and mangles
 * args with spaces/quotes); passing the absolute .exe path to CreateProcess
 * forwards argv verbatim.
 */

import { spawnSync, type SpawnSyncReturns } from 'child_process';
import { existsSync } from 'fs';
import { join, dirname, resolve } from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
// tests/unit/e2e/spawn-zbrain.ts -> tests/unit/e2e -> tests/unit -> tests -> <root>
const PROJECT_ROOT = resolve(__dirname, '../../..');

const TARGET_MAP: Record<string, string> = {
  'win32-x64': 'x86_64-pc-windows-msvc',
  'win32-ia32': 'x86_64-pc-windows-msvc',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
};

/**
 * Ordered candidate binary paths for a platform/arch. Pure + exported so a
 * unit test can exercise resolution without touching the real filesystem.
 * Mirrors bin/zbrain-rs.js getBinaryCandidates.
 */
export function getBinaryCandidates(platform: string, arch: string, root: string): string[] {
  const target = TARGET_MAP[`${platform}-${arch}`];
  const names = ['zbrain', 'zbrain.exe'];
  const candidates: string[] = [];

  if (target) {
    for (const profile of ['release', 'debug']) {
      for (const name of names) {
        candidates.push(join(root, 'target', target, profile, name));
      }
    }
  }
  for (const profile of ['release', 'debug']) {
    for (const name of names) {
      candidates.push(join(root, 'target', profile, name));
    }
  }
  return candidates;
}

/**
 * Resolve the zbrain binary path, or null if none is available.
 */
export function resolveZbrainBin(): string | null {
  if (process.env.ZBRAIN_BIN && existsSync(process.env.ZBRAIN_BIN)) {
    return process.env.ZBRAIN_BIN;
  }
  for (const candidate of getBinaryCandidates(process.platform, process.arch, PROJECT_ROOT)) {
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

export interface SpawnZbrainOptions {
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  input?: string;
  encoding?: BufferEncoding;
  timeoutMs?: number;
}

export interface SpawnZbrainResult {
  status: number | null;
  signal: string | null;
  stdout: string;
  stderr: string;
  error?: Error;
}

/**
 * Spawn the real zbrain binary with the given argv and capture output.
 * Throws if no binary resolves (callers should gate on resolveZbrainBin()
 * first via describe.skipIf).
 */
export function spawnZbrain(args: string[], opts: SpawnZbrainOptions = {}): SpawnZbrainResult {
  const bin = resolveZbrainBin();
  if (!bin) {
    throw new Error(
      'resolveZbrainBin() returned null: no zbrain binary found. Set ZBRAIN_BIN to a ' +
        'prebuilt binary, or run `cargo build --bin zbrain` so one lands in target/.',
    );
  }
  const result: SpawnSyncReturns<string> = spawnSync(bin, args, {
    cwd: opts.cwd,
    env: opts.env ?? process.env,
    input: opts.input,
    encoding: (opts.encoding ?? 'utf-8') as BufferEncoding,
    timeout: opts.timeoutMs,
  });
  return {
    status: result.status,
    signal: result.signal,
    stdout: result.stdout ?? '',
    stderr: result.stderr ?? '',
    error: result.error,
  };
}

/**
 * Spawn and assert exit 0, returning trimmed stdout. Throws with captured
 * stdout/stderr on non-zero exit so test failures are debuggable.
 */
export function runZbrainOk(args: string[], opts: SpawnZbrainOptions = {}): string {
  const r = spawnZbrain(args, opts);
  if (r.status !== 0) {
    throw new Error(
      `zbrain ${args.join(' ')} exited ${r.status}` +
        `${r.signal ? ` (signal ${r.signal})` : ''}\n--- stdout ---\n${r.stdout}\n--- stderr ---\n${r.stderr}`,
    );
  }
  return r.stdout.trim();
}

/**
 * True when a zbrain binary is resolvable. Use as the condition for
 * `describe.skipIf(!binaryAvailable())` so the suite skips cleanly when no
 * binary is built locally, and runs under CI (which builds first).
 */
export function binaryAvailable(): boolean {
  return resolveZbrainBin() !== null;
}
