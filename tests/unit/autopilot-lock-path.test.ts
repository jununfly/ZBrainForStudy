/**
 * v0.37.7.0 #1226 regression test.
 *
 * The autopilot lockfile was hardcoded at `~/.zbrain/autopilot.lock`
 * (via `process.env.HOME`), bypassing ZBRAIN_HOME. Two brains pointed
 * at different ZBRAIN_HOME directories would still write to the same
 * global lockfile; one would silently take over the other on each
 * restart.
 *
 * Fix: route through `zbrainPath('autopilot.lock')` which honors
 * ZBRAIN_HOME. This file pins the contract via the canonical helper
 * directly, since the autopilot daemon's lifecycle is heavy to drive
 * in a unit test.
 */

import { describe, test, expect } from 'bun:test';
import { withEnv } from './helpers/with-env.ts';
import { mkdtempSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { zbrainPath } from '../../src/core/config.ts';

describe('autopilot lock path scoped to ZBRAIN_HOME (#1226)', () => {
  test('one ZBRAIN_HOME produces one canonical lock path', async () => {
    const home = mkdtempSync(join(tmpdir(), 'zbrain-autopilot-lock-'));
    await withEnv({ ZBRAIN_HOME: home }, async () => {
      const lockPath = zbrainPath('autopilot.lock');
      // Lockfile MUST live inside the per-brain ZBRAIN_HOME, not under
      // process.env.HOME — that was the pre-fix bug.
      expect(lockPath.startsWith(home)).toBe(true);
      expect(lockPath.endsWith('autopilot.lock')).toBe(true);
    });
  });

  test('two ZBRAIN_HOME values produce two distinct lockfiles', async () => {
    const homeA = mkdtempSync(join(tmpdir(), 'zbrain-autopilot-A-'));
    const homeB = mkdtempSync(join(tmpdir(), 'zbrain-autopilot-B-'));

    let lockA = '';
    let lockB = '';
    await withEnv({ ZBRAIN_HOME: homeA }, async () => {
      lockA = zbrainPath('autopilot.lock');
    });
    await withEnv({ ZBRAIN_HOME: homeB }, async () => {
      lockB = zbrainPath('autopilot.lock');
    });

    // The contract that prevents two brains from silently colliding:
    // distinct ZBRAIN_HOME values MUST produce distinct lockfile paths.
    expect(lockA).not.toBe(lockB);
    expect(lockA.startsWith(homeA)).toBe(true);
    expect(lockB.startsWith(homeB)).toBe(true);
  });

  test('default (no ZBRAIN_HOME override) still produces a valid path', async () => {
    // When ZBRAIN_HOME is unset, zbrainPath falls through to its
    // default (`~/.zbrain`). The path must still exist as a string
    // and end with the expected filename — we don't assert the exact
    // home dir since that varies by environment.
    await withEnv({ ZBRAIN_HOME: undefined }, async () => {
      const lockPath = zbrainPath('autopilot.lock');
      expect(typeof lockPath).toBe('string');
      expect(lockPath.endsWith('autopilot.lock')).toBe(true);
      expect(lockPath.length).toBeGreaterThan('autopilot.lock'.length);
    });
  });
});
