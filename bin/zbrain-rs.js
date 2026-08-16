#!/usr/bin/env node
/**
 * ZBrain Rust CLI wrapper script.
 *
 * This script detects the current platform and executes the appropriate
 * Rust binary from the correct target directory.
 *
 * If no pre-built binary exists for the platform, it falls back to
 * building from source via `cargo build -p zbrain-cli`.
 *
 * This wrapper is a TRANSPARENT pass-through only: it forwards argv verbatim
 * and faithfully propagates the child's exit code / signal. It deliberately
 * does NOT parse or interpret any flags — global-flag semantics belong in the
 * Rust clap layer, not here.
 *
 * FUTURE(global-flag-parity): the TS entrypoint (src/core/cli-options.ts
 * parseGlobalFlags) exposed --quiet / --progress-json / --progress-interval /
 * --timeout / --explain as global flags. The Rust `Cli` struct
 * (crates/zbrain-cli/src/lib.rs) currently only has --config / --debug.
 * Investigation (roadmap 1-8) found NONE of the 5 flags have a working
 * consumer in Rust yet — they are blocked on three unported subsystems:
 * progress reporter, MCP per-call timeout, and search rerank/attribution.
 * They are deliberately NOT added as dead no-op flags. See the authoritative
 * flag -> subsystem -> code-anchor table in
 * docs/plans/MIGRATION.md. Do NOT re-implement flag
 * parsing in this wrapper.
 */

import { spawnSync } from 'child_process';
import { existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath, pathToFileURL } from 'url';
import { constants } from 'os';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const projectRoot = join(__dirname, '..');

// Map Node.js platform/arch to Rust target triples
const TARGET_MAP = {
  'win32-x64': 'x86_64-pc-windows-msvc',
  'win32-ia32': 'x86_64-pc-windows-msvc',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
};

/**
 * Build the ordered list of candidate binary paths for a given platform/arch.
 *
 * Pure + exported so the wrapper's test can exercise path resolution without
 * touching the real filesystem or the host's actual platform.
 *
 * Ordering rules:
 *   - Triple-specific paths before the bare `target/` paths (a cross-compiled
 *     or explicitly-targeted build is more specific than the default).
 *   - Within each location, RELEASE before DEBUG: prefer the optimized binary
 *     the release ships, but fall back to a plain `cargo build` (debug) so
 *     local dev works too. Removing the hardcoded target from
 *     .cargo/config.toml means a bare `cargo build` now lands at
 *     target/debug/zbrain[.exe]; the old release-only list missed it.
 *
 * @param {string} platform  process.platform value (e.g. 'win32')
 * @param {string} arch      process.arch value (e.g. 'x64')
 * @param {string} root      project root directory
 * @returns {string[]}
 */
export function getBinaryCandidates(platform, arch, root) {
  const target = TARGET_MAP[`${platform}-${arch}`];
  const names = ['zbrain', 'zbrain.exe'];
  const candidates = [];

  // Triple-specific locations (only when the platform/arch is mapped).
  if (target) {
    for (const profile of ['release', 'debug']) {
      for (const name of names) {
        candidates.push(join(root, 'target', target, profile, name));
      }
    }
  }

  // Bare target/ locations (host-default build, or unmapped platform).
  for (const profile of ['release', 'debug']) {
    for (const name of names) {
      candidates.push(join(root, 'target', profile, name));
    }
  }

  return candidates;
}

function getBinaryPath() {
  for (const candidate of getBinaryCandidates(process.platform, process.arch, projectRoot)) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }

  return null;
}

function buildFromSource() {
  console.error('Building zbrain CLI from source...');
  const result = spawnSync('cargo', ['build', '--release', '-p', 'zbrain-cli'], {
    stdio: 'inherit',
    cwd: projectRoot,
    shell: process.platform === 'win32',
  });

  if (result.status !== 0) {
    console.error('Failed to build zbrain CLI from source');
    process.exit(1);
  }
}

/**
 * Resolve the exit code the wrapper should propagate from a spawnSync result.
 *
 * The wrapper's job is to be a transparent pass-through: whatever the Rust
 * binary's fate was, propagate it faithfully. The previous
 * `result.status || 0` masked failures — a signal-killed child has
 * `status === null`, and `null || 0` reported it as success.
 *
 * @param {{status: number|null, signal: string|null, error?: Error}} result
 * @returns {number}
 */
export function resolveExitCode(result) {
  if (typeof result.status === 'number') {
    return result.status;
  }
  if (result.signal) {
    // Unix convention: a process terminated by signal N exits with 128 + N.
    const signalNumber = constants.signals[result.signal];
    return typeof signalNumber === 'number' ? 128 + signalNumber : 1;
  }
  if (result.error) {
    // spawnSync failed to launch the child (ENOENT/EACCES/...): never
    // report success. Propagate as a generic failure.
    return 1;
  }
  return 0;
}

// Main execution
function main() {
  let binaryPath = getBinaryPath();

  if (!binaryPath) {
    buildFromSource();
    binaryPath = getBinaryPath();
    if (!binaryPath) {
      console.error('Failed to find zbrain binary after build');
      process.exit(1);
    }
  }

  // Execute the Rust binary with all arguments passed through.
  //
  // NOTE: no `shell` option here. On Windows, spawning through cmd.exe
  // re-parses argv and mangles arguments containing spaces/quotes/`^&|`,
  // which would corrupt transparent pass-through. Passing the absolute
  // binary path to CreateProcess directly forwards argv verbatim.
  const result = spawnSync(binaryPath, process.argv.slice(2), {
    stdio: 'inherit',
  });

  process.exit(resolveExitCode(result));
}

// Only run when invoked as the entrypoint, not when imported by tests.
if (import.meta.url === `file://${process.argv[1]}` ||
    import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  main();
}
