/**
 * Tests for src/core/skillpack/harvest.ts — the host→zbrain lift.
 *
 * Pins:
 *   - happy path: skill files + paired sources land in zbrain's tree
 *   - openclaw.plugin.json updated (sorted, idempotent)
 *   - slug collision refused unless --overwrite-local
 *   - symlinks in host source rejected
 *   - canonical-path containment rejects traversal
 *   - privacy lint runs by default, rolls back on hit
 *   - --no-lint bypasses
 *   - dry-run reports plan, writes nothing
 *   - host SKILL.md missing → HarvestError(host_skill_missing)
 */

import { describe, expect, it, afterEach } from 'bun:test';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'fs';
import { join } from 'path';
import { tmpdir } from 'os';

import { HarvestError, runHarvest, addToBundleManifest } from '../../src/core/skillpack/harvest.ts';

const created: string[] = [];
afterEach(() => {
  while (created.length) {
    const p = created.pop()!;
    try {
      rmSync(p, { recursive: true, force: true });
    } catch {}
  }
});

function scratchHost(opts: { withPairedSource?: boolean; contaminated?: boolean } = {}): string {
  const root = mkdtempSync(join(tmpdir(), 'sp-h-host-'));
  created.push(root);
  mkdirSync(join(root, 'src', 'commands'), { recursive: true });
  mkdirSync(join(root, 'skills', 'my-fork-skill'), { recursive: true });

  const fm = opts.withPairedSource
    ? '---\nname: my-fork-skill\ntriggers:\n  - trigger\nsources:\n  - src/commands/my-fork-skill.ts\n---\n'
    : '---\nname: my-fork-skill\ntriggers:\n  - trigger\n---\n';
  const body = opts.contaminated
    ? '# my-fork-skill\n\nThis was lifted from Wintermute.\n'
    : '# my-fork-skill\n\nGeneric placeholder content.\n';
  writeFileSync(join(root, 'skills', 'my-fork-skill', 'SKILL.md'), fm + body);

  if (opts.withPairedSource) {
    writeFileSync(
      join(root, 'src', 'commands', 'my-fork-skill.ts'),
      '// real impl\nexport function run() { return 1; }\n',
    );
  }
  return root;
}

function scratchZbrain(): string {
  const root = mkdtempSync(join(tmpdir(), 'sp-h-zbrain-'));
  created.push(root);
  mkdirSync(join(root, 'src', 'commands'), { recursive: true });
  writeFileSync(join(root, 'src', 'cli.ts'), '// stub');
  mkdirSync(join(root, 'skills'), { recursive: true });
  writeFileSync(
    join(root, 'openclaw.plugin.json'),
    JSON.stringify(
      {
        name: 'zbrain',
        version: '0.33.0',
        skills: ['skills/existing-skill'],
        shared_deps: [],
      },
      null,
      2,
    ),
  );
  return root;
}

function emptyPatternsFile(): string {
  const dir = mkdtempSync(join(tmpdir(), 'sp-h-patterns-'));
  created.push(dir);
  const path = join(dir, 'patterns.txt');
  writeFileSync(path, '# only the user-supplied patterns. Defaults still apply.\n');
  return path;
}

describe('runHarvest — happy path', () => {
  it('copies a clean skill into zbrain, updates manifest', () => {
    const hostRoot = scratchHost();
    const zbrainRoot = scratchZbrain();

    const result = runHarvest({
      slug: 'my-fork-skill',
      hostRepoRoot: hostRoot,
      zbrainRoot,
    });

    expect(result.status).toBe('harvested');
    expect(existsSync(join(zbrainRoot, 'skills', 'my-fork-skill', 'SKILL.md'))).toBe(true);
    expect(result.manifestUpdated).toBe(true);

    const manifest = JSON.parse(readFileSync(join(zbrainRoot, 'openclaw.plugin.json'), 'utf-8'));
    expect(manifest.skills).toContain('skills/my-fork-skill');
    // sorted
    expect(manifest.skills).toEqual([...manifest.skills].sort());
  });

  it('copies paired source files declared in host frontmatter', () => {
    const hostRoot = scratchHost({ withPairedSource: true });
    const zbrainRoot = scratchZbrain();

    const result = runHarvest({
      slug: 'my-fork-skill',
      hostRepoRoot: hostRoot,
      zbrainRoot,
    });

    expect(result.pairedSources).toEqual(['src/commands/my-fork-skill.ts']);
    expect(existsSync(join(zbrainRoot, 'src', 'commands', 'my-fork-skill.ts'))).toBe(true);
  });
});

describe('runHarvest — error paths', () => {
  it('host_skill_missing when --from points at the wrong place', () => {
    const hostRoot = scratchHost();
    const zbrainRoot = scratchZbrain();

    try {
      runHarvest({ slug: 'nonexistent', hostRepoRoot: hostRoot, zbrainRoot });
      throw new Error('expected throw');
    } catch (err) {
      expect(err).toBeInstanceOf(HarvestError);
      expect((err as HarvestError).code).toBe('host_skill_missing');
    }
  });

  it('slug_collision when zbrain already has skills/<slug>/ — without --overwrite-local', () => {
    const hostRoot = scratchHost();
    const zbrainRoot = scratchZbrain();
    mkdirSync(join(zbrainRoot, 'skills', 'my-fork-skill'), { recursive: true });
    writeFileSync(join(zbrainRoot, 'skills', 'my-fork-skill', 'SKILL.md'), '# already here\n');

    try {
      runHarvest({
        slug: 'my-fork-skill',
        hostRepoRoot: hostRoot,
        zbrainRoot,
      });
      throw new Error('expected throw');
    } catch (err) {
      expect(err).toBeInstanceOf(HarvestError);
      expect((err as HarvestError).code).toBe('slug_collision');
    }
  });

  it('symlinks in host skill dir are rejected (D13 security gate)', () => {
    const hostRoot = scratchHost();
    const zbrainRoot = scratchZbrain();

    // Plant a symlink inside the skill dir pointing outside.
    const outside = mkdtempSync(join(tmpdir(), 'sp-h-outside-'));
    created.push(outside);
    writeFileSync(join(outside, 'secret.txt'), 'PRIVATE\n');
    symlinkSync(
      join(outside, 'secret.txt'),
      join(hostRoot, 'skills', 'my-fork-skill', 'leaked.txt'),
    );

    try {
      runHarvest({
        slug: 'my-fork-skill',
        hostRepoRoot: hostRoot,
        zbrainRoot,
        noLint: true,
      });
      throw new Error('expected throw');
    } catch (err) {
      expect(err).toBeInstanceOf(HarvestError);
      expect(['symlink_rejected', 'path_traversal']).toContain((err as HarvestError).code);
    }
    // Nothing landed.
    expect(existsSync(join(zbrainRoot, 'skills', 'my-fork-skill'))).toBe(false);
  });
});

describe('runHarvest — privacy linter integration (T7)', () => {
  it('default Wintermute pattern triggers rollback (no manifest update)', () => {
    const hostRoot = scratchHost({ contaminated: true });
    const zbrainRoot = scratchZbrain();

    const result = runHarvest({
      slug: 'my-fork-skill',
      hostRepoRoot: hostRoot,
      zbrainRoot,
    });

    expect(result.status).toBe('lint_failed');
    expect(result.lintHits.length).toBeGreaterThan(0);
    expect(result.lintHits[0]).toContain('Wintermute');

    // Rollback: nothing in zbrain tree.
    expect(existsSync(join(zbrainRoot, 'skills', 'my-fork-skill'))).toBe(false);
    // Manifest NOT updated.
    const manifest = JSON.parse(readFileSync(join(zbrainRoot, 'openclaw.plugin.json'), 'utf-8'));
    expect(manifest.skills).not.toContain('skills/my-fork-skill');
  });

  it('--no-lint bypasses the linter (editorial workflow opt-out)', () => {
    const hostRoot = scratchHost({ contaminated: true });
    const zbrainRoot = scratchZbrain();

    const result = runHarvest({
      slug: 'my-fork-skill',
      hostRepoRoot: hostRoot,
      zbrainRoot,
      noLint: true,
    });

    expect(result.status).toBe('harvested');
    expect(existsSync(join(zbrainRoot, 'skills', 'my-fork-skill', 'SKILL.md'))).toBe(true);
  });
});

describe('runHarvest — dry-run', () => {
  it('reports plan, writes nothing', () => {
    const hostRoot = scratchHost();
    const zbrainRoot = scratchZbrain();

    const result = runHarvest({
      slug: 'my-fork-skill',
      hostRepoRoot: hostRoot,
      zbrainRoot,
      dryRun: true,
    });

    expect(result.status).toBe('harvested');
    expect(result.dryRun).toBe(true);
    expect(result.manifestUpdated).toBe(false);
    expect(existsSync(join(zbrainRoot, 'skills', 'my-fork-skill'))).toBe(false);
  });
});

describe('addToBundleManifest', () => {
  it('adds a new slug, sorts skills array, idempotent', () => {
    const zbrainRoot = scratchZbrain();

    expect(addToBundleManifest(zbrainRoot, 'new-skill')).toBe(true);
    expect(addToBundleManifest(zbrainRoot, 'new-skill')).toBe(false); // idempotent

    const manifest = JSON.parse(readFileSync(join(zbrainRoot, 'openclaw.plugin.json'), 'utf-8'));
    expect(manifest.skills).toContain('skills/new-skill');
    expect(manifest.skills).toEqual([...manifest.skills].sort());
  });
});
