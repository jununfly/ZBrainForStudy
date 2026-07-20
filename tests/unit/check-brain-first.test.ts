/**
 * tests/unit/check-brain-first.test.ts — end-to-end contract test for the
 * Rust `zbrain check-brain-first` command (roadmap 1-6-5-9-3).
 *
 * The analyzer itself migrated from `src/core/skill-brain-first.ts` to
 * `zbrain_core::skill_resolver::brain_first`; this test exercises the CLI
 * envelope + exit-code contract that `skillify-check` item 12 (1-6-5-9-4)
 * and the future doctor check depend on. Spawns the real binary via the
 * shared `resolveZbrainBin()` helper.
 */

import { afterEach, describe, expect, test } from 'bun:test';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, existsSync } from 'fs';
import { join } from 'path';
import { tmpdir } from 'os';
import { spawnSync } from 'child_process';

import { resolveZbrainBin } from '../../src/core/zbrain-bin.ts';

const BIN = resolveZbrainBin();
const created: string[] = [];

function scratchSkill(name: string, content: string): string {
  const dir = mkdtempSync(join(tmpdir(), `check-brain-first-${name}-`));
  created.push(dir);
  const skillDir = join(dir, 'myskill');
  mkdirSync(skillDir, { recursive: true });
  const p = join(skillDir, 'SKILL.md');
  writeFileSync(p, content);
  return p;
}

afterEach(() => {
  while (created.length) {
    const d = created.pop();
    if (d && existsSync(d)) rmSync(d, { recursive: true, force: true });
  }
});

function runCheck(path: string): { status: number | null; env: any } {
  const r = spawnSync(BIN, ['check-brain-first', path, '--json'], {
    encoding: 'utf-8',
    maxBuffer: 10 * 1024 * 1024,
  });
  expect(r.error).toBeUndefined();
  let env: any = null;
  try {
    env = JSON.parse(r.stdout);
  } catch {
    /* leave null */
  }
  return { status: r.status, env };
}

describe('zbrain check-brain-first (1-6-5-9-3)', () => {
  test('ok: no external pattern → exit 0, status ok', () => {
    const p = scratchSkill('ok', '---\nname: myskill\n---\n\nJust does local stuff.\n');
    const { status, env } = runCheck(p);
    expect(status).toBe(0);
    expect(env.ok).toBe(true);
    expect(env.status).toBe('ok');
    expect(env.reason).toBe('exempt_no_external');
    expect(env.summary_line).toBe('myskill: ok (exempt_no_external)');
  });

  test('ok: external pattern + Convention callout → exit 0', () => {
    const p = scratchSkill(
      'callout',
      '---\nname: myskill\n---\n\n> **Convention:** see conventions/brain-first.md for the lookup chain.\n\nUse web_search here.\n',
    );
    const { status, env } = runCheck(p);
    expect(status).toBe(0);
    expect(env.status).toBe('ok');
    expect(env.reason).toBe('compliant_callout');
  });

  test('warn: external pattern, no compliance → exit 1 + fix_hint', () => {
    const p = scratchSkill('warn', '---\nname: myskill\n---\n\nUse web_search to look things up.\n');
    const { status, env } = runCheck(p);
    expect(status).toBe(1);
    expect(env.ok).toBe(false);
    expect(env.status).toBe('warn');
    expect(env.reason).toBe('missing_brain_first');
    expect(env.summary_line).toContain('external lookup (web_search) without brain-first compliance');
    expect(env.fix_hint).toContain('Fix: add canonical Convention callout');
  });

  test('error: missing SKILL.md → exit 1 with error envelope', () => {
    const missing = join(tmpdir(), `check-brain-first-missing-${Date.now()}.md`);
    const { status, env } = runCheck(missing);
    expect(status).toBe(1);
    expect(env.error).toBe('no_skill_md');
    expect(env.status).toBeUndefined();
  });
});
