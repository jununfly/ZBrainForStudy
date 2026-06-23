/**
 * Tests for src/core/minions/spawn-helpers.ts — pure helpers that build the
 * (cmd, args) tuple for spawning the zbrain worker, optionally wrapped in
 * tini for zombie reaping.
 *
 * `buildSpawnInvocation` is a pure function — directly testable without any
 * mocking. `detectTini` shells out to `which tini`; the test asserts only
 * that it returns a string (presence depends on the test machine).
 */

import { describe, test, expect } from 'bun:test';
import { buildSpawnInvocation, detectTini } from '../../src/core/minions/spawn-helpers.ts';

describe('buildSpawnInvocation', () => {
  test('without tini: returns cliPath + raw args', () => {
    const result = buildSpawnInvocation('', '/bin/zbrain', ['jobs', 'work']);
    expect(result).toEqual({ cmd: '/bin/zbrain', args: ['jobs', 'work'] });
  });

  test('with tini: wraps cliPath with tini and "--" separator', () => {
    const result = buildSpawnInvocation('/usr/bin/tini', '/bin/zbrain', ['jobs', 'work']);
    expect(result).toEqual({
      cmd: '/usr/bin/tini',
      args: ['--', '/bin/zbrain', 'jobs', 'work'],
    });
  });

  test('empty args list is preserved on both branches', () => {
    expect(buildSpawnInvocation('', '/bin/zbrain', [])).toEqual({
      cmd: '/bin/zbrain',
      args: [],
    });
    expect(buildSpawnInvocation('/usr/bin/tini', '/bin/zbrain', [])).toEqual({
      cmd: '/usr/bin/tini',
      args: ['--', '/bin/zbrain'],
    });
  });
});

describe('detectTini', () => {
  test('returns a string (smoke test only — actual presence depends on machine)', () => {
    const result = detectTini();
    expect(typeof result).toBe('string');
    // Do NOT assert truthiness: tini may or may not be installed on the
    // test host. We only verify the function doesn't throw and returns
    // a defined string ('' when absent, path when present).
  });
});
