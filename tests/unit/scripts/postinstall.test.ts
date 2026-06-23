import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { delimiter, join, resolve } from 'path';
import { spawnSync } from 'child_process';

const REPO_ROOT = resolve(import.meta.dir, '..', '..', '..');
const SCRIPT = resolve(REPO_ROOT, 'scripts/postinstall.ts');

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'zbrain-postinstall-test-'));
});

afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

function runPostinstall(pathEntries: string[], extraEnv: Record<string, string> = {}) {
  const env = {
    ...process.env,
    ...extraEnv,
    PATH: pathEntries.join(delimiter),
  } as Record<string, string>;
  const result = spawnSync(process.execPath, ['run', SCRIPT], {
    cwd: REPO_ROOT,
    env,
    encoding: 'utf-8',
  });

  return {
    status: result.status ?? 1,
    stdout: result.stdout?.toString?.() ?? '',
    stderr: result.stderr?.toString?.() ?? '',
  };
}

function writeFakeZbrain(exitCode = 0): string {
  const binDir = join(tmp, 'bin');
  const logPath = join(tmp, 'zbrain-args.log');
  const cmdPath = join(binDir, process.platform === 'win32' ? 'zbrain.cmd' : 'zbrain');
  Bun.spawnSync(['mkdir', '-p', binDir]);

  if (process.platform === 'win32') {
    writeFileSync(
      cmdPath,
      `@echo off\r\necho %* > "${logPath}"\r\nexit /b ${exitCode}\r\n`,
      { mode: 0o755 },
    );
  } else {
    writeFileSync(
      cmdPath,
      `#!/usr/bin/env sh\nprintf '%s\\n' "$*" > '${logPath}'\nexit ${exitCode}\n`,
    );
    chmodSync(cmdPath, 0o755);
  }

  return binDir;
}

describe('postinstall script', () => {
  test('skips successfully when zbrain is not on PATH', () => {
    const result = runPostinstall([join(tmp, 'empty-path')]);

    expect(result.status).toBe(0);
    expect(result.stderr).toContain('[zbrain] postinstall skipped');
    expect(result.stderr).toContain('zbrain apply-migrations --yes');
  });

  test('runs zbrain apply-migrations when zbrain is on PATH', () => {
    const binDir = writeFakeZbrain(0);
    const result = runPostinstall([binDir]);

    expect(result.status).toBe(0);
    expect(readFileSync(join(tmp, 'zbrain-args.log'), 'utf-8').trim()).toBe(
      'apply-migrations --yes --non-interactive',
    );
  });

  test('propagates zbrain apply-migrations failure status', () => {
    const binDir = writeFakeZbrain(7);
    const result = runPostinstall([binDir]);

    expect(result.status).toBe(7);
    expect(readFileSync(join(tmp, 'zbrain-args.log'), 'utf-8').trim()).toBe(
      'apply-migrations --yes --non-interactive',
    );
  });
});
