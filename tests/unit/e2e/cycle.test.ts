/**
 * E2E cycle tests — re-routed to the real Rust `zbrain` binary (#143).
 *
 * This replaces the deleted tests/unit/e2e/cycle.test.ts, which imported the
 * TS `runCycle` core function and drove it in-process against a real Postgres
 * engine (with `embedBatch` mocked). The TS `src/core/cycle.ts` module is now
 * deleted — the cycle lives in the Rust binary as `zbrain dream` (run_cycle).
 * So we drive the binary and assert on its CycleReport + side effects.
 *
 * Deterministic (no LLM / no external DB — local PGLite):
 *   - `dream --dry-run --json` runs the full phase sequence and writes ZERO
 *     pages (the dry-run regression guard).
 *   - The orchestration covers the expected phases (sync, recompute-emotional-
 *     weight, embed, orphans, purge, ...).
 *   - A live `dream` (gated to non-Windows) actually syncs the repo page in.
 *
 * NOT re-routed to the binary (deliberate scope decision):
 *   The deleted suite also asserted cycle-lock semantics against real Postgres
 *   by seeding `zbrain_cycle_locks` rows directly (concurrent cycle blocked →
 *   status:skipped; TTL-expired lock auto-claimed; --phase orphans skips the
 *   lock). Those tests reach INTO the database with raw SQL — which would mean
 *   the "binary E2E" poking the binary's private storage, defeating the
 *   discipline. The lock acquire/release/TTL logic is exercised by the Rust
 *   unit tests instead. We keep the binary E2E focused on what users observe:
 *   the report shape and the no-write guarantee.
 *
 * Gating: skips on Windows by default (libsql FFI read crash ~40%); set
 * ZBRAIN_E2E_ALLOW_WIN=1 to force a local Windows run. See binary-helpers.ts.
 *
 * Run: bun test tests/unit/e2e/cycle.test.ts
 */

import { describe, it, expect } from 'bun:test';
import { binaryE2eGate, runDreamReport, runZbrainOkRetry, type PgliteBrain } from './binary-helpers.ts';

describe.skipIf(binaryE2eGate)('zbrain cycle (dream) E2E (re-routed to Rust artifact)', () => {
  it('dry-run cycle runs the phase sequence and writes zero pages', () => {
    const { report, brain } = runDreamReport(['--dry-run']);
    try {
      expect(report.schemaVersion).toBe('1');
      expect(report).toHaveProperty('status');
      expect(Array.isArray(report.phases)).toBe(true);
      expect(report.phases.length).toBeGreaterThan(15); // full orchestration, not a stub

      // Core phases that must be present in the real binary's cycle.
      const names = report.phases.map((p: { phase: string }) => p.phase);
      for (const required of ['sync', 'recompute-emotional-weight', 'embed', 'orphans', 'purge']) {
        expect(names).toContain(required);
      }

      // Nothing was written: dry-run guarantees zero pages.
      const list = JSON.parse(runZbrainOkRetry(['list-pages', '--config', brain.cfgPath]));
      expect(list.pages.length).toBe(0);
    } finally {
      brain.cleanup();
    }
  });

  it.skipIf(process.platform === 'win32')('live cycle syncs the repo page into the real brain', () => {
    const { report, brain } = runDreamReport([]);
    try {
      expect(report.status).not.toBe('fail');

      const list = JSON.parse(runZbrainOkRetry(['list-pages', '--config', brain.cfgPath]));
      const slugs = list.pages.map((p: { slug: string }) => p.slug);
      expect(slugs).toContain('concepts/testing');

      // sync.last_commit bookmark is set after a live sync.
      const cfgOut = runZbrainOkRetry(['config', 'get', 'sync.last_commit', '--config', brain.cfgPath]);
      expect(cfgOut.trim().length).toBeGreaterThanOrEqual(7);
    } finally {
      brain.cleanup();
    }
  });
});
