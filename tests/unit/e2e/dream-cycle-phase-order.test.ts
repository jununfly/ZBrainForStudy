/**
 * E2E dream-cycle phase-order test — re-routed to the real Rust binary (#143).
 *
 * Replaces the deleted tests/unit/e2e/dream-cycle-phase-order-pglite.test.ts,
 * which asserted the TS `runCycle` phase sequence. `src/core/cycle.ts` is gone;
 * the orchestration now lives in the Rust binary as `run_cycle`. We assert the
 * REAL binary emits its 23 phases in the canonical relative order, via
 * `dream --dry-run --json` (deterministic — LLM/embed phases self-skip, no
 * API keys, local PGLite).
 *
 * This is a regression guard on the cycle wiring: if a phase is added,
 * removed, or reordered in the Rust cycle, this test fails loudly. Update the
 * EXPECTED_PHASES array in lockstep with the Rust `execute_phase` match arms.
 *
 * Gating: skips on Windows by default (libsql FFI read crash ~40%); set
 * ZBRAIN_E2E_ALLOW_WIN=1 to force a local Windows run. See binary-helpers.ts.
 *
 * Run: bun test tests/unit/e2e/dream-cycle-phase-order.test.ts
 */

import { describe, it, expect } from 'bun:test';
import { binaryE2eGate, runDreamReport, type PgliteBrain } from './binary-helpers.ts';

// Canonical phase order produced by the Rust cycle (zbrain dream --dry-run).
// Mirrors the execute_phase dispatch order in the Rust core. Keep in sync.
const EXPECTED_PHASES = [
  'lint',
  'backlinks',
  'sync',
  'synthesize',
  'extract',
  'extract-facts',
  'extract-atoms',
  'extract-takes',
  'resolve-symbol-edges',
  'patterns',
  'auto-think',
  'synthesize-concepts',
  'recompute-emotional-weight',
  'consolidate',
  'propose-takes',
  'grade-takes',
  'calibration-profile',
  'conversation-facts-backfill',
  'embed',
  'orphans',
  'schema-suggest',
  'purge',
  'drift',
];

describe.skipIf(binaryE2eGate)('zbrain dream cycle phase order (re-routed to Rust artifact)', () => {
  it('emits all phases in the canonical orchestration order', () => {
    const { report, brain } = runDreamReport(['--dry-run']);
    try {
      const actual = report.phases.map((p: { phase: string }) => p.phase);
      expect(actual).toEqual(EXPECTED_PHASES);
    } finally {
      brain.cleanup();
    }
  });

  it('every phase reports a terminal status (ok | skipped | fail)', () => {
    const { report, brain } = runDreamReport(['--dry-run']);
    try {
      for (const p of report.phases) {
        expect(['ok', 'skipped', 'fail']).toContain(p.status);
      }
      // Without a chat/embedding provider, the LLM-heavy phases self-skip
      // rather than fail — the cycle degrades gracefully, it does not error out.
      expect(report.status).not.toBe('fail');
    } finally {
      brain.cleanup();
    }
  });
});
