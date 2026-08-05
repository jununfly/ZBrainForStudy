/**
 * E2E recompute-emotional-weight cycle phase — re-routed to the real binary (#143).
 *
 * Replaces the deleted tests/unit/e2e/cycle-recompute-emotional-weight-pglite
 * .test.ts, which called the TS `runPhaseRecomputeEmotionalWeight` in-process.
 * The phase now lives in the Rust binary; we drive `zbrain dream --phase
 * recompute-emotional-weight` and assert the single-phase dispatch + report.
 *
 * Deterministic: recompute-emotional-weight is pure (re-derives each page's
 * emotional weight from its content signals — no LLM, no embeddings). On an
 * empty brain it reports 0 pages recomputed with status ok; on a populated
 * brain it would recompute in `full` mode. We assert the dispatch contract.
 *
 * Gating: skips on Windows by default (libsql FFI read crash ~40%); set
 * ZBRAIN_E2E_ALLOW_WIN=1 to force a local Windows run. See binary-helpers.ts.
 *
 * Run: bun test tests/unit/e2e/cycle-recompute-emotional-weight.test.ts
 */

import { describe, it, expect } from 'bun:test';
import { binaryE2eGate, runDreamReport, type PgliteBrain } from './binary-helpers.ts';

describe.skipIf(binaryE2eGate)('zbrain cycle recompute-emotional-weight E2E (re-routed)', () => {
  it('--phase recompute-emotional-weight dispatches exactly that phase', () => {
    const { report, brain } = runDreamReport(['--phase', 'recompute-emotional-weight']);
    try {
      expect(report.phases.length).toBe(1);
      const phase = report.phases[0];
      expect(phase.phase).toBe('recompute-emotional-weight');
      expect(phase.status).toBe('ok');
      // mode full = a real recompute pass ran (not a no-op stub).
      expect(phase.details.mode).toBe('full');
    } finally {
      brain.cleanup();
    }
  });
});
