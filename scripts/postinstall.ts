import { spawnSync } from 'node:child_process';
import { accessSync, constants, existsSync } from 'node:fs';
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

function runMigrations(executable: string, args: string[]): number {
  const result = spawnSync(executable, args, {
    stdio: 'inherit',
    cwd: projectRoot,
    shell: false,
  });
  return result.status ?? 1;
}

// Main postinstall flow
const rustWrapper = join(projectRoot, 'bin', 'zbrain-rs.js');
const tsCli = join(projectRoot, 'src', 'cli.ts');
const bun = findCommand('bun');

// Try Rust first
if (hasRustToolchain()) {
  if (buildRustBinary()) {
    console.log('[zbrain] Running migrations with Rust binary...');
    const exitCode = runMigrations(process.execPath, [rustWrapper, 'apply-migrations', '--yes', '--non-interactive']);
    if (exitCode === 0) {
      console.log('[zbrain] postinstall completed successfully.');
      process.exit(0);
    }
    console.log('[zbrain] Rust binary migration failed, falling back to TypeScript...');
  }
}

// Fallback to TypeScript
if (bun && existsSync(tsCli)) {
  console.log('[zbrain] Running migrations with TypeScript fallback...');
  const exitCode = runMigrations(bun, [tsCli, 'apply-migrations', '--yes', '--non-interactive']);
  if (exitCode === 0) {
    console.log('[zbrain] postinstall completed successfully.');
    process.exit(0);
  }
}

console.log('[zbrain] postinstall: automatic migration skipped. Run `zbrain doctor` and `zbrain apply-migrations --yes` manually.');
process.exit(0);
