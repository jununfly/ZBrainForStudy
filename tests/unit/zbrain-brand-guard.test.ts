import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';

const REPO_ROOT = resolve(import.meta.dir, '..', '..');

const SCAN_ROOTS = [
  'src',
  'scripts',
  'tests/unit',
  'tests/heavy',
  'docker-compose.ci.yml',
  'docker-compose.test.yml',
  '.gitleaks.toml',
] as const;

const FORBIDDEN = /GBRAIN|GBrain|\.gbrain|gbrain\.yml|\bgbrain\b|gbrain[-_]/g;

const GLOBAL_ALLOWLIST: readonly RegExp[] = [];

const ALLOWLIST: Record<string, RegExp[]> = {
  'src/schema.sql': [
    /gbrain-owned stable IDs/,
    /Legacy rows/,
    /gbrain extract links/,
    /gbrain eval replay/,
    /gbrain think --ab/,
    /gbrain sources rename/,
    /gbrain lsd/,
    /gbrain dream CLI/,
    /gbrain doctor/,
    /used by gbrain via pooler/,
    /ZBrain Postgres \+ pgvector schema/,
  ],
  'src/core/migrate.ts': [/gbrain_cycle_locks/, /gbrain_tool_use_id/],
  'src/commands/migrations/v0_18_1.ts': [/gbrain_cycle_locks/],
  'src/eval/longmemeval/extract.ts': [/gbrain-allow-direct-insert/],
  'src/core/artifact/index.ts': [/\.zbrain-/, /gbrain-.*-v1/],
  'tests/unit/db-legacy-identifier-rename.test.ts': [/gbrain_cycle_locks/, /gbrain_tool_use_id/],
  'tests/unit/migrate.test.ts': [/gbrain_cycle_locks/],
  'scripts/test-weights.json': [/gbrain-home-isolation\.test\.ts/, /zbrain-base-equivalence\.test\.ts/],
};

function shouldSkipPath(path: string): boolean {
  return (
    path.endsWith(`${sep}tests${sep}unit${sep}zbrain-brand-guard.test.ts`) ||
    path.includes(`${sep}tests${sep}heavy${sep}fixtures${sep}`) ||
    path.includes(`${sep}node_modules${sep}`) ||
    path.includes(`${sep}.git${sep}`)
  );
}

function listFiles(path: string): string[] {
  const absolute = resolve(REPO_ROOT, path);
  if (shouldSkipPath(absolute)) return [];
  const stats = statSync(absolute);
  if (stats.isFile()) return [absolute];
  return readdirSync(absolute).flatMap((entry) => listFiles(join(path, entry)));
}

function isAllowed(relativePath: string, line: string): boolean {
  return (
    GLOBAL_ALLOWLIST.some((pattern) => pattern.test(line)) ||
    (ALLOWLIST[relativePath]?.some((pattern) => pattern.test(line)) ?? false)
  );
}

describe('ZBrain brand naming guard', () => {
  test('source, tests, scripts, and CI config do not expose legacy ZBrain names', () => {
    const violations: string[] = [];

    for (const root of SCAN_ROOTS) {
      for (const file of listFiles(root)) {
        const relativePath = relative(REPO_ROOT, file).replaceAll('\\', '/');
        const content = readFileSync(file, 'utf-8');
        content.split(/\r?\n/).forEach((line, index) => {
          FORBIDDEN.lastIndex = 0;
          if (FORBIDDEN.test(line) && !isAllowed(relativePath, line)) {
            violations.push(`${relativePath}:${index + 1}: ${line.trim()}`);
          }
        });
      }
    }

    expect(violations).toEqual([]);
  });
});
