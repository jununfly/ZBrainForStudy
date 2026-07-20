/**
 * tests/unit/audit-skill-brain-first.test.ts — unit tests for the
 * snapshot+diff audit trail module (`src/core/audit-skill-brain-first.ts`).
 *
 * Split out of the former `skill-brain-first.test.ts` when the brain-first
 * *analyzer* migrated to Rust (`zbrain check-brain-first`, roadmap 1-6-5-9).
 * The audit trail is a separate, retained module — only the analyzer half
 * moved, so its tests move here rather than being dropped.
 */

import { describe, expect, test } from 'bun:test';
import { existsSync, mkdirSync, rmSync } from 'fs';
import { join } from 'path';
import { tmpdir } from 'os';

import { withEnv } from './helpers/with-env.ts';
import {
  diffAgainstSnapshot,
  loadSnapshot,
  writeSnapshotAtomically,
  computeBrainFirstAuditFilename,
  logBrainFirstEvent,
  readRecentBrainFirstEvents,
  appendAuditEventsForTransitions,
  _resetWarnedSetForTests,
} from '../../src/core/audit-skill-brain-first.ts';

/**
 * Helper: provision an isolated audit tempdir for one test body and tear
 * it down via try/finally. Wraps the body in `withEnv()` so the
 * ZBRAIN_AUDIT_DIR mutation is scoped to this test only — cross-test-
 * safe (no leak to other tests in the same shard) per the test-
 * isolation lint (R1).
 */
async function withAuditDir<T>(fn: (auditDir: string) => Promise<T> | T): Promise<T> {
  _resetWarnedSetForTests();
  const auditDir = join(
    tmpdir(),
    `brain-first-audit-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
  );
  mkdirSync(auditDir, { recursive: true });
  try {
    return await withEnv({ ZBRAIN_AUDIT_DIR: auditDir }, () => fn(auditDir));
  } finally {
    try { rmSync(auditDir, { recursive: true, force: true }); } catch { /* ignore */ }
  }
}

describe('audit-skill-brain-first (snapshot+diff)', () => {
  test('loadSnapshot returns present:false when file missing', async () => {
    await withAuditDir(() => {
      const r = loadSnapshot();
      expect(r.present).toBe(false);
      expect(r.violators.size).toBe(0);
    });
  });

  test('writeSnapshotAtomically + loadSnapshot round-trip', async () => {
    await withAuditDir(() => {
      writeSnapshotAtomically(new Set(['a', 'b', 'c']));
      const r = loadSnapshot();
      expect(r.present).toBe(true);
      expect(r.violators.has('a')).toBe(true);
      expect(r.violators.has('b')).toBe(true);
      expect(r.violators.has('c')).toBe(true);
    });
  });

  test('diffAgainstSnapshot detects added/removed/unchanged', () => {
    // Pure function — no audit dir needed.
    const prev = new Set(['a', 'b', 'c']);
    const curr = new Set(['b', 'c', 'd']);
    const diff = diffAgainstSnapshot(curr, prev);
    expect(diff.added).toEqual(['d']);
    expect(diff.removed).toEqual(['a']);
    expect(diff.unchanged).toEqual(['b', 'c']);
  });

  test('diff result is sorted for determinism', () => {
    const prev = new Set(['c', 'a', 'd']);
    const curr = new Set(['a', 'b', 'e']);
    const diff = diffAgainstSnapshot(curr, prev);
    expect(diff.added).toEqual(['b', 'e']);
    expect(diff.removed).toEqual(['c', 'd']);
    expect(diff.unchanged).toEqual(['a']);
  });

  test('corrupt snapshot JSON treated as missing with warn-once', async () => {
    await withAuditDir(auditDir => {
      const file = join(auditDir, 'skill-brain-first-snapshot.json');
      require('fs').writeFileSync(file, 'not-json-at-all');
      const r = loadSnapshot();
      expect(r.present).toBe(false);
      expect(r.violators.size).toBe(0);
    });
  });

  test('appendAuditEventsForTransitions writes one line per added/removed', async () => {
    await withAuditDir(() => {
      const diff = { added: ['skill-a'], removed: ['skill-b'], unchanged: ['skill-c'] };
      const patterns = new Map([['skill-a', ['web_search']]]);
      appendAuditEventsForTransitions(diff, patterns, 'test-run-1');
      const events = readRecentBrainFirstEvents(7);
      expect(events.length).toBe(2);
      const detected = events.find(e => e.event === 'detected');
      const resolved = events.find(e => e.event === 'resolved');
      expect(detected?.skill).toBe('skill-a');
      expect(detected?.external_patterns).toEqual(['web_search']);
      expect(resolved?.skill).toBe('skill-b');
    });
  });

  test('no-transition diff produces zero audit writes (A2 contract)', async () => {
    await withAuditDir(() => {
      const diff = { added: [], removed: [], unchanged: ['skill-a', 'skill-b'] };
      appendAuditEventsForTransitions(diff, new Map(), 'test-run-2');
      const events = readRecentBrainFirstEvents(7);
      expect(events.length).toBe(0);
    });
  });

  test('logBrainFirstEvent writes a fixed event', async () => {
    await withAuditDir(() => {
      logBrainFirstEvent({ event: 'fixed', skill: 'browser' });
      const events = readRecentBrainFirstEvents(7);
      expect(events.length).toBe(1);
      expect(events[0].event).toBe('fixed');
      expect(events[0].skill).toBe('browser');
      expect(events[0].code).toBe('SKILL_BRAIN_FIRST');
      expect(events[0].severity).toBe('info');
    });
  });

  test('computeBrainFirstAuditFilename produces ISO-week format', () => {
    const name = computeBrainFirstAuditFilename(new Date('2026-05-19T10:00:00Z'));
    expect(name).toMatch(/^skill-brain-first-2026-W\d{2}\.jsonl$/);
  });
});
