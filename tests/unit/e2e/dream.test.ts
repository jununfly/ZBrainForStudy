/**
 * E2E dream tests — re-routed to the real Rust `zbrain` binary (#143).
 *
 * This replaces the deleted tests/unit/e2e/dream.test.ts, which imported the
 * TS `runDream` command function and invoked it in-process against a real
 * Postgres engine. As the TS command is deleted and the Rust binary is the
 * single shipped artifact, we now drive `zbrain dream` as a subprocess and
 * assert on its JSON CycleReport + side effects — validating the real CLI
 * surface users run, not in-process TS calls.
 *
 * The binary's `dream` runs the full maintenance cycle. LLM/embedding-heavy
 * phases (synthesize, embed, propose-takes, calibration-profile, ...) self-skip
 * when no provider is configured, so the deterministic assertions below need
 * NO API keys. We use a local PGLite brain (no external DB server).
 *
 * What we assert deterministically (no LLM):
 *   - `dream --dry-run --json` emits a valid CycleReport (camelCase keys) and
 *     writes ZERO pages to the brain.
 *   - `dream --phase orphans --json` runs exactly one phase (orphans) and is
 *     read-only.
 *
 * Live-write assertion (gated to non-Windows):
 *   - A live `dream` (no --dry-run) actually syncs the repo's page into the
 *     brain. The Windows libsql/SQLite FFI write path is intermittently
 *     SIGSEGV-prone (project-known flake; CI runs on Linux to dodge it), so the
 *     live-write assertion is skipped on win32 and run on Linux CI.
 *
 * Gating: the whole suite skips on Windows by default (SKIP_ON_WINDOWS) because
 * the libsql FFI read path also crashes ~40% of the time there; set
 * ZBRAIN_E2E_ALLOW_WIN=1 to force a local Windows run (the runDreamReport
 * helper retries through crashes).
 *
 * Run: bun test tests/unit/e2e/dream.test.ts
 */

import { describe, it, expect } from 'bun:test';
import { binaryE2eGate, runDreamReport, runZbrainOkRetry, type PgliteBrain } from './binary-helpers.ts';

describe.skipIf(binaryE2eGate)('zbrain dream E2E (re-routed to Rust artifact)', () => {
  it('dream --dry-run --json emits a valid CycleReport and writes zero pages', () => {
    const { report, brain } = runDreamReport(['--dry-run']);
    try {
      // CycleReport shape (camelCase — the binary serializes JSON, not snake_case).
      expect(report).toHaveProperty('schemaVersion');
      expect(report.schemaVersion).toBe('1');
      expect(report).toHaveProperty('status');
      expect(report).toHaveProperty('phases');
      expect(Array.isArray(report.phases)).toBe(true);
      expect(report).toHaveProperty('totals');
      expect(report.brainDir).toBe(brain.repoDir);

      // Dry-run must not mutate the brain: no pages land in the DB.
      const list = JSON.parse(runZbrainOkRetry(['list-pages', '--config', brain.cfgPath]));
      expect(list.pages.length).toBe(0);
    } finally {
      brain.cleanup();
    }
  });

  it('dream --phase orphans dispatches exactly the orphans phase (read-only)', () => {
    const { report, brain } = runDreamReport(['--phase', 'orphans']);
    try {
      expect(report.phases.length).toBe(1);
      expect(report.phases[0].phase).toBe('orphans');
      expect(report.phases[0].status).toBe('ok');
    } finally {
      brain.cleanup();
    }
  });

  it.skipIf(process.platform === 'win32')(
    'live dream syncs the repo page into the real brain (no --dry-run)',
    () => {
      const { report, brain } = runDreamReport([]); // no --dry-run → live cycle
      try {
        expect(report.status).not.toBe('fail');

        // The brain DB file must exist + be non-empty (the write path fired).
        const list = JSON.parse(runZbrainOkRetry(['list-pages', '--config', brain.cfgPath]));
        const slugs = list.pages.map((p: { slug: string }) => p.slug);
        expect(slugs).toContain('concepts/testing');
      } finally {
        brain.cleanup();
      }
    },
  );
});
