/**
 * E2E consolidate cycle phase — re-routed to the real binary (#143), EMBED-GATED.
 *
 * Replaces the deleted tests/unit/e2e/cycle-consolidate-postgres.test.ts, which
 * called the TS `runPhaseConsolidate` in-process (with `embedBatch` mocked) and
 * exercised the Postgres-engine code paths. The phase now lives in the Rust
 * binary as part of `zbrain dream`.
 *
 * WHY GATED: consolidate clusters near-duplicate pages via cosine similarity
 * and writes semantic facts — it REQUIRES a configured embedding client. With
 * no embeddings the binary self-skips it (`engine_unsupported_no_raw` / no
 * embed client). So this suite only runs where embeddings are provisioned
 * (set ZBRAIN_E2E_EMBED=1 and point ZBRAIN_BIN at a binary built against an
 * embedding-configured brain). Locally it is a clean skip; a CI that provisions
 * embeddings exercises the real consolidate pass end-to-end.
 *
 * This is the explicit LLM/embed slice of #143 that was deferred: the rest of
 * the cycle/dream e2e suites are deterministic (no embeddings) and run
 * ungated. Kept here so the re-route is complete and the harness exists for a
 * properly-provisioned run.
 *
 * Gating: skips unless BOTH (a) embeddings are enabled via ZBRAIN_E2E_EMBED=1
 * and (b) a binary is available and we're not on the default-skipped Windows
 * path. Set ZBRAIN_E2E_ALLOW_WIN=1 as well to force a Windows run.
 *
 * Run: ZBRAIN_E2E_EMBED=1 bun test tests/unit/e2e/cycle-consolidate.test.ts
 */

import { describe, it, expect } from 'bun:test';
import {
  binaryE2eGate,
  runDreamReport,
  runDreamOnBrain,
  runZbrainOkRetry,
  type PgliteBrain,
} from './binary-helpers.ts';

const EMBED_ENABLED = process.env.ZBRAIN_E2E_EMBED === '1';
const describeE2E = !EMBED_ENABLED || binaryE2eGate ? describe.skip : describe;

describeE2E('zbrain cycle consolidate E2E (re-routed, embed-gated)', () => {
  it('live cycle syncs pages, then consolidate clusters + writes facts', () => {
    // Two near-duplicate concept pages so consolidate has something to cluster.
    const { report: live, brain } = runDreamReport([], {
      files: {
        'concepts/alpha.md':
          '---\ntype: concept\ntitle: Alpha\n---\n\nThe quick brown fox jumps over the lazy dog near the riverbank.\n',
        'concepts/beta.md':
          '---\ntype: concept\ntitle: Beta\n---\n\nThe quick brown fox jumps over the lazy dog near the riverbank at dusk.\n',
      },
    });
    try {
      expect(live.status).not.toBe('fail');

      const list = JSON.parse(runZbrainOkRetry(['list-pages', '--config', brain.cfgPath]));
      expect(list.pages.length).toBe(2);

      // Run just the consolidate phase on the same (synced) brain.
      const report = runDreamOnBrain(['--phase', 'consolidate'], brain);
      const phase = report.phases[0];
      expect(phase.phase).toBe('consolidate');
      // With embeddings active, consolidate must actually run (not skip).
      expect(phase.status).toBe('ok');
      expect(typeof report.totals.factsConsolidated).toBe('number');
    } finally {
      brain.cleanup();
    }
  });
});
