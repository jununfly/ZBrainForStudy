import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolveExitCode } from './zbrain-rs.js';

// 1-5: bin wrapper transparent pass-through correctness.
// resolveExitCode maps a spawnSync result ({status, signal, error}) to the
// exit code the wrapper should propagate. The core bug it fixes: the old
// `result.status || 0` reported signal-kills (status===null) as success (0).

test('normal exit status 0 is propagated as 0', () => {
  assert.equal(resolveExitCode({ status: 0, signal: null }), 0);
});

test('non-zero exit status is propagated verbatim', () => {
  assert.equal(resolveExitCode({ status: 3, signal: null }), 3);
});

test('signal-killed child (status null) becomes non-zero, not 0', () => {
  // The core bug: `status || 0` reported this as success.
  // Unix convention: 128 + signal number. SIGTERM = 15 -> 143.
  assert.equal(resolveExitCode({ status: null, signal: 'SIGTERM' }), 143);
});

test('spawn error (no status, no signal) becomes non-zero', () => {
  // spawnSync failed to launch the child (e.g. ENOENT / EACCES):
  // status and signal are both null but result.error is set.
  const code = resolveExitCode({ status: null, signal: null, error: new Error('ENOENT') });
  assert.notEqual(code, 0);
});
