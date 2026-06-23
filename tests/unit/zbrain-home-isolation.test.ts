/**
 * Hermeticity test: every site that writes under `~/.zbrain` must honor
 * `ZBRAIN_HOME=<tmp>` and write under `<tmp>/.zbrain` instead of the developer's
 * real home.
 *
 * Why this exists: `src/core/config.ts::configDir()` already supports
 * `ZBRAIN_HOME` as a parent-dir override (returns `<override>/.zbrain`), but
 * historically many call sites built paths from `os.homedir()` directly,
 * bypassing the override. The hermeticity migration migrated every write-side
 * caller to `zbrainPath(...)`. This test is the regression gate.
 *
 * Scope: write-isolation only. Read-side host detection in
 * `src/commands/init.ts` (reading `~/.claude`, `~/.openclaw`, etc. for module
 * fingerprinting) is the documented v1 caveat and is NOT asserted here.
 */

import { describe, test, expect } from 'bun:test';
import { mkdtempSync, existsSync, readdirSync, statSync, rmSync } from 'fs';
import { homedir, tmpdir } from 'os';
import { join } from 'path';

// Save original env so we don't leak between tests.
const ORIG_ZBRAIN_HOME = process.env.ZBRAIN_HOME;

function fresh(): string {
  return mkdtempSync(join(tmpdir(), 'zbrain-home-isolation-'));
}

describe('ZBRAIN_HOME write-side isolation', () => {
  test('configDir() returns <ZBRAIN_HOME>/.zbrain when override is set', async () => {
    const tmp = fresh();
    process.env.ZBRAIN_HOME = tmp;
    try {
      const { configDir, zbrainPath } = await import('../../src/core/config.ts');
      expect(configDir()).toBe(join(tmp, '.zbrain'));
      expect(zbrainPath('foo', 'bar.json')).toBe(join(tmp, '.zbrain', 'foo', 'bar.json'));
    } finally {
      process.env.ZBRAIN_HOME = ORIG_ZBRAIN_HOME;
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  test('configDir() falls back to homedir when ZBRAIN_HOME unset', async () => {
    delete process.env.ZBRAIN_HOME;
    try {
      const { configDir } = await import('../../src/core/config.ts');
      // Contract: when ZBRAIN_HOME is unset, configDir() === os.homedir()/.zbrain.
      // Asserting against os.homedir() (rather than a "not /tmp/" sentinel) keeps
      // this test correct under safety wrappers that redirect HOME=/tmp/... — the
      // behavior we care about is that the fallback path equals homedir().
      expect(configDir()).toBe(join(homedir(), '.zbrain'));
    } finally {
      if (ORIG_ZBRAIN_HOME !== undefined) process.env.ZBRAIN_HOME = ORIG_ZBRAIN_HOME;
    }
  });

  test('rejects relative ZBRAIN_HOME', async () => {
    process.env.ZBRAIN_HOME = 'relative/path';
    try {
      const { configDir } = await import('../../src/core/config.ts');
      expect(() => configDir()).toThrow(/absolute path/);
    } finally {
      process.env.ZBRAIN_HOME = ORIG_ZBRAIN_HOME;
    }
  });

  test("rejects ZBRAIN_HOME containing '..' segments", async () => {
    process.env.ZBRAIN_HOME = '/tmp/foo/../bar';
    try {
      const { configDir } = await import('../../src/core/config.ts');
      expect(() => configDir()).toThrow(/'\.\.' segments/);
    } finally {
      process.env.ZBRAIN_HOME = ORIG_ZBRAIN_HOME;
    }
  });

  test('saveConfig/loadConfig honor ZBRAIN_HOME', async () => {
    const tmp = fresh();
    process.env.ZBRAIN_HOME = tmp;
    try {
      const { saveConfig, loadConfig } = await import('../../src/core/config.ts');
      const cfg = { engine: 'pglite' as const, database_path: join(tmp, '.zbrain', 'brain.pglite') };
      saveConfig(cfg);
      // Config file should exist under the override, NOT under real ~/.zbrain.
      expect(existsSync(join(tmp, '.zbrain', 'config.json'))).toBe(true);

      // Round-trip: loadConfig() finds it back via the override.
      const loaded = loadConfig();
      expect(loaded?.engine).toBe('pglite');
      expect(loaded?.database_path).toBe(cfg.database_path);
    } finally {
      process.env.ZBRAIN_HOME = ORIG_ZBRAIN_HOME;
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  test('integrity, sync-failures, integrations heartbeat resolve under ZBRAIN_HOME', async () => {
    const tmp = fresh();
    process.env.ZBRAIN_HOME = tmp;
    try {
      const { zbrainPath } = await import('../../src/core/config.ts');
      // Spot-check a representative set of paths used across the migrated sites.
      const paths = [
        zbrainPath('integrity-review.md'),                       // src/commands/integrity.ts
        zbrainPath('sync-failures.jsonl'),                       // src/core/sync.ts
        zbrainPath('integrations', 'recipe-x'),                  // src/commands/integrations.ts
        zbrainPath('migrate-manifest.json'),                     // src/commands/migrate-engine.ts
        zbrainPath('import-checkpoint.json'),                    // src/commands/import.ts
        zbrainPath('migrations', 'v0_13_1-rollback.jsonl'),      // src/commands/migrations/v0_13_1.ts
        zbrainPath('migrations', 'pending-host-work.jsonl'),     // src/commands/migrations/v0_14_0.ts
        zbrainPath('audit'),                                     // shell-audit / backpressure-audit
        zbrainPath('cycle.lock'),                                // src/core/cycle.ts
        zbrainPath('fail-improve'),                              // src/core/fail-improve.ts
        zbrainPath('validator-lint.jsonl'),                      // src/core/output/post-write.ts
        zbrainPath('brain.pglite'),                              // init pglite default
      ];
      for (const p of paths) {
        expect(p.startsWith(join(tmp, '.zbrain'))).toBe(true);
      }
    } finally {
      process.env.ZBRAIN_HOME = ORIG_ZBRAIN_HOME;
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  test('ZBRAIN_AUDIT_DIR override still wins over ZBRAIN_HOME', async () => {
    const tmp = fresh();
    const auditTmp = fresh();
    process.env.ZBRAIN_HOME = tmp;
    process.env.ZBRAIN_AUDIT_DIR = auditTmp;
    try {
      const { resolveAuditDir } = await import('../../src/core/minions/handlers/shell-audit.ts');
      // Per the docstring: ZBRAIN_AUDIT_DIR is the explicit override and wins.
      expect(resolveAuditDir()).toBe(auditTmp);
    } finally {
      process.env.ZBRAIN_HOME = ORIG_ZBRAIN_HOME;
      delete process.env.ZBRAIN_AUDIT_DIR;
      rmSync(tmp, { recursive: true, force: true });
      rmSync(auditTmp, { recursive: true, force: true });
    }
  });
});
