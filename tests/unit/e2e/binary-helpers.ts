/**
 * Shared helpers for the binary-driven E2E suites (#143 re-route).
 *
 * These build a throwaway PGLite brain (local libsql, no external DB server,
 * no network, no API keys) and drive the REAL shipped `zbrain` binary at it —
 * exactly the artifact users run. They deliberately import NOTHING from
 * `src/`: the whole point of the re-route is to stop exercising in-process TS
 * command functions and validate the binary's end-to-end wiring instead.
 *
 * `makePgliteBrain` creates:
 *   - a temp git repo seeded with markdown files (the "brain repo" sync syncs
 *     from),
 *   - a temp `zbrain.yml` that points `sync.default_repo` at that repo and
 *     tracks `concepts/`,
 *   - a real PGLite brain via `zbrain init --pglite --config <tmp>/zbrain.yml`
 *     (writes `brain.pglite` under the temp dir — never the user's ~/.zbrain;
 *     run-e2e.sh isolates HOME/ZBRAIN_HOME too).
 *
 * Everything lives under mkdtemp dirs and is removed by `cleanup()`.
 */

import { mkdtempSync, writeFileSync, rmSync, mkdirSync, existsSync, statSync } from 'fs';
import { tmpdir } from 'os';
import { join, dirname } from 'path';
import { execSync } from 'child_process';
import { binaryAvailable, runZbrainOk, spawnZbrain } from './spawn-zbrain.ts';

/**
 * Windows libsql/SQLite FFI crash gate.
 *
 * The native libsql FFI on Windows intermittently SIGSEGV (exit 139) when the
 * zbrain binary opens a PGLite brain — measured at ~40% of invocations in this
 * sandbox, and a crash can corrupt the brain file for later calls. The project
 * runs the E2E suite on Linux CI to dodge this (see working memory: "libsql FFI
 * flake … CI 跑 ubuntu-latest 避此崩溃"). So by DEFAULT these PGLite-backed
 * binary tests skip on Windows and run on Linux CI.
 *
 * Set ZBRAIN_E2E_ALLOW_WIN=1 to force them to run on Windows anyway — the
 * runDreamReport/runZbrainOkRetry helpers below spin up a FRESH brain per
 * attempt and retry through crashes, so a local dev box can still exercise
 * them (just slower, with occasional retries).
 */
export const SKIP_ON_WINDOWS =
  process.platform === 'win32' && !process.env.ZBRAIN_E2E_ALLOW_WIN;

/** Combined gate for the binary-driven E2E suites: need a binary AND not on a
 *  Windows box unless explicitly opted in. */
export const binaryE2eGate = !binaryAvailable() || SKIP_ON_WINDOWS;

export interface PgliteBrain {
  /** Temp root holding zbrain.yml + brain.pglite. */
  tmpDir: string;
  /** Temp git repo that sync pulls from. */
  repoDir: string;
  /** Absolute path to the zbrain.yml config (pass to --config). */
  cfgPath: string;
  /** Remove all temp artifacts. */
  cleanup(): void;
}

export interface MakeBrainOptions {
  /**
   * Seed files relative to the repo root. Defaults to an EMPTY repo: the
   * deterministic (dry-run / single-phase) suites assert "zero writes" and a
   * clean phase-status set, which only hold on an empty brain. The live-sync
   * and consolidate suites pass their own `files` to exercise the write path.
   */
  files?: Record<string, string>;
}

/**
 * Build a fresh PGLite brain + git repo and return handles for the tests.
 * Throws if no zbrain binary resolves (callers gate on binaryAvailable()).
 */
export function makePgliteBrain(opts: MakeBrainOptions = {}): PgliteBrain {
  const tmpDir = mkdtempSync(join(tmpdir(), 'zbrain-e2e-'));
  const repoDir = mkdtempSync(join(tmpdir(), 'zbrain-repo-'));

  execSync('git init', { cwd: repoDir, stdio: 'pipe' });
  execSync('git config user.email test@test.co', { cwd: repoDir, stdio: 'pipe' });
  execSync('git config user.name test', { cwd: repoDir, stdio: 'pipe' });

  const files = opts.files ?? {};
  for (const [rel, content] of Object.entries(files)) {
    const p = join(repoDir, rel);
    mkdirSync(dirname(p), { recursive: true });
    writeFileSync(p, content);
  }
  if (Object.keys(files).length > 0) {
    execSync('git add -A && git commit -qm init', { cwd: repoDir, stdio: 'pipe' });
  }

  const cfgPath = join(tmpDir, 'zbrain.yml');
  // Track concepts/ so the seeded page gets synced; keep the rest out of scope.
  writeFileSync(
    cfgPath,
    `storage:\n  db_tracked:\n    - concepts/\n  db_only:\n    - media/x/\n\nsync:\n  default_repo: ${repoDir}\n`,
  );

  // Real engine round-trip: the binary writes a PGLite brain to tmpDir.
  runZbrainOk(['init', '--config', cfgPath, '--pglite', '--force', '--non-interactive']);

  return {
    tmpDir,
    repoDir,
    cfgPath,
    cleanup() {
      rmSync(tmpDir, { recursive: true, force: true });
      rmSync(repoDir, { recursive: true, force: true });
    },
  };
}

/**
 * True when the generated brain DB file exists and is non-empty (a write
 * actually happened), used to assert the cycle's write path fired.
 */
export function brainDbExists(brain: PgliteBrain): boolean {
  const dbPath = join(brain.tmpDir, 'brain.pglite');
  return existsSync(dbPath) && statSync(dbPath).size > 0;
}

/**
 * Run `zbrain dream <args>` against a FRESH PGLite brain and return its parsed
 * JSON CycleReport. Retries on the Windows libsql FFI crash (exit 139 / empty
 * output) by spinning up a new brain each attempt, so intermittent segfaults
 * self-heal instead of failing the test. On Linux CI it passes on the first
 * try. Caller owns the returned `brain` (call brain.cleanup()).
 */
export function runDreamReport(
  args: string[],
  opts: { attempts?: number; files?: Record<string, string> } = {},
): { report: any; brain: PgliteBrain } {
  const attempts = opts.attempts ?? 8;
  let last: unknown = null;
  for (let i = 0; i < attempts; i++) {
    let brain: PgliteBrain | null = null;
    try {
      brain = makePgliteBrain({ files: opts.files });
      const out = spawnZbrain(['dream', ...args, '--config', brain.cfgPath, '--dir', brain.repoDir, '--json']);
      if (out.status === 0 && out.stdout.trimStart().startsWith('{')) {
        return { report: JSON.parse(out.stdout), brain };
      }
      last = new Error(
        `zbrain dream exited ${out.status ?? 'null'}` +
          `${out.signal ? ` (signal ${out.signal})` : ''}\n` +
          `--- stderr ---\n${out.stderr.slice(0, 800)}\n` +
          `--- stdout ---\n${out.stdout.slice(0, 400)}`,
      );
    } catch (e) {
      last = e;
    }
    if (brain) brain.cleanup();
  }
  throw last instanceof Error ? last : new Error('runDreamReport: exhausted attempts');
}

/**
 * Run `zbrain dream <args>` on an EXISTING brain (returned by runDreamReport)
 * and return its parsed JSON CycleReport. Retries through the Windows libsql
 * FFI crash by re-issuing against the same brain. The brain is owned by the
 * caller (not cleaned up here).
 */
export function runDreamOnBrain(args: string[], brain: PgliteBrain, attempts = 8): any {
  let last: unknown = null;
  for (let i = 0; i < attempts; i++) {
    const out = spawnZbrain(['dream', ...args, '--config', brain.cfgPath, '--dir', brain.repoDir, '--json']);
    if (out.status === 0 && out.stdout.trimStart().startsWith('{')) {
      return JSON.parse(out.stdout);
    }
    last = new Error(
      `zbrain dream exited ${out.status ?? 'null'}` +
        `${out.signal ? ` (signal ${out.signal})` : ''}\n` +
        `--- stderr ---\n${out.stderr.slice(0, 800)}\n` +
        `--- stdout ---\n${out.stdout.slice(0, 400)}`,
    );
  }
  throw last instanceof Error ? last : new Error('runDreamOnBrain: exhausted attempts');
}

/**
 * Like runZbrainOk but retries through the Windows libsql FFI crash (non-zero
 * exit with no useful output) so read-only post-run assertions (list-pages,
 * config get) stay stable on a flaky Windows box.
 */
export function runZbrainOkRetry(args: string[], attempts = 8): string {
  let last: unknown = null;
  for (let i = 0; i < attempts; i++) {
    try {
      return runZbrainOk(args);
    } catch (e) {
      last = e;
    }
  }
  throw last instanceof Error ? last : new Error('runZbrainOkRetry: exhausted attempts');
}
