import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolveExitCode, getBinaryCandidates } from './zbrain-rs.js';

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

// 1-1-3: the wrapper must find the Rust binary whether it was produced by
// `cargo build` (debug) or `cargo build --release` (release). Removing the
// hardcoded target from .cargo/config.toml means a plain `cargo build` now
// lands at target/debug/zbrain[.exe]; the previous release-only candidate
// list missed it (Part2 rc=127 footgun).

test('candidate list includes both release and debug for the mapped triple', () => {
  const candidates = getBinaryCandidates('win32', 'x64', '/repo');
  // Release must be searched before debug (prefer the shipped/optimized binary).
  const relIdx = candidates.findIndex((c) => c.includes('x86_64-pc-windows-msvc') && c.includes('release'));
  const dbgIdx = candidates.findIndex((c) => c.includes('x86_64-pc-windows-msvc') && c.includes('debug'));
  assert.ok(relIdx >= 0, 'expected a triple release candidate');
  assert.ok(dbgIdx >= 0, 'expected a triple debug candidate');
  assert.ok(relIdx < dbgIdx, 'release must be searched before debug');
});

test('candidate list covers plain target/debug for a bare cargo build', () => {
  const candidates = getBinaryCandidates('linux', 'x64', '/repo');
  const hasPlainDebug = candidates.some(
    (c) => /(?:^|[\\/])target[\\/]debug[\\/]zbrain(?:\.exe)?$/.test(c),
  );
  assert.ok(hasPlainDebug, 'expected a target/debug/zbrain candidate');
});

test('unknown platform still falls back to bare target/{release,debug}', () => {
  const candidates = getBinaryCandidates('sunos', 'sparc', '/repo');
  const hasRelease = candidates.some((c) => /target[\\/]release[\\/]zbrain/.test(c));
  const hasDebug = candidates.some((c) => /target[\\/]debug[\\/]zbrain/.test(c));
  assert.ok(hasRelease && hasDebug, 'fallback must include both release and debug under target/');
});
