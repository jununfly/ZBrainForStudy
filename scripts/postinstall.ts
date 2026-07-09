import { spawnSync } from 'node:child_process';
import { accessSync, constants } from 'node:fs';
import { delimiter, join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const projectRoot = join(__dirname, '..');

function findCommand(command: string): string | null {
  const path = process.env.PATH ?? '';
  const extensions = process.platform === 'win32'
    ? (process.env.PATHEXT ?? '.EXE;.CMD;.BAT;.COM')
      .split(';')
      .filter(Boolean)
    : [''];

  for (const dir of path.split(delimiter).filter(Boolean)) {
    for (const ext of extensions) {
      const candidate = join(dir, process.platform === 'win32' ? `${command}${ext.toLowerCase()}` : command);
      try {
        accessSync(candidate, constants.X_OK);
        return candidate;
      } catch {
        // keep searching
      }
      if (process.platform === 'win32') {
        const upperCandidate = join(dir, `${command}${ext.toUpperCase()}`);
        try {
          accessSync(upperCandidate, constants.X_OK);
          return upperCandidate;
        } catch {
          // keep searching
        }
      }
    }
  }

  return null;
}

function hasRustToolchain(): boolean {
  return findCommand('cargo') !== null;
}

function buildRustBinary(): boolean {
  console.log('[zbrain] Building Rust CLI binary...');
  const result = spawnSync('cargo', ['build', '--release', '-p', 'zbrain-cli'], {
    stdio: 'inherit',
    cwd: projectRoot,
    shell: process.platform === 'win32',
  });
  return result.status === 0;
}

function runZbrain(args: string[]): number {
  const wrapper = join(projectRoot, 'bin', 'zbrain-rs.js');
  const result = spawnSync(process.execPath, [wrapper, ...args], {
    stdio: 'inherit',
    cwd: projectRoot,
    shell: false,
  });
  return result.status ?? 1;
}

// Main postinstall flow — Rust-only, no TypeScript fallback.
//
// G21 (registered in docs/plans/KNOWN-GAPS.md):
//   Rust has no `apply-migrations` command (version tracking + migration
//   scripts). Only DDL-level `init --migrate-only` is available. Developers
//   needing full migrations should use `bun src/cli.ts apply-migrations`.
if (!hasRustToolchain()) {
  console.log('[zbrain] No Rust toolchain found. Skipping CLI build.');
  console.log('[zbrain] Run `zbrain init` after installing Rust to set up the brain.');
  process.exit(0);
}

if (!buildRustBinary()) {
  console.log('[zbrain] Rust CLI build failed. Run `cargo build --release -p zbrain-cli` manually.');
  process.exit(1);
}

// Try DDL migration. Rust CLI uses its own config discovery
// (~/.zbrain/config, ./zbrain.yml, etc.). If no config exists,
// --migrate-only will fail with a clear message — that's fine.
console.log('[zbrain] Running DDL migration (--migrate-only)...');
const exitCode = runZbrain(['init', '--migrate-only']);
if (exitCode !== 0) {
  console.log('[zbrain] DDL migration skipped (no existing config, or migration failed).');
  console.log('[zbrain] Run `zbrain init` to set up a new brain, or `zbrain init --migrate-only` to migrate an existing one.');
}

console.log('[zbrain] postinstall completed.');
process.exit(0);
