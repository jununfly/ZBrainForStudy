import { spawnSync } from 'node:child_process';
import { accessSync, constants } from 'node:fs';
import { delimiter, join } from 'node:path';

const SKIP_MESSAGE =
  '[zbrain] postinstall skipped. If installed via bun install -g github:...: run `zbrain doctor` and `zbrain apply-migrations --yes` manually.';

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

const zbrain = findCommand('zbrain');

if (!zbrain) {
  console.error(SKIP_MESSAGE);
  process.exit(0);
}

const result = spawnSync(zbrain, ['apply-migrations', '--yes', '--non-interactive'], {
  stdio: 'inherit',
  shell: false,
});

if (result.error) {
  console.error(`[zbrain] postinstall failed: ${result.error.message}`);
  process.exit(1);
}

process.exit(result.status ?? 1);
