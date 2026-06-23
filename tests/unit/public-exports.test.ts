/**
 * Public exports contract test (v0.21.0 — Lane 2 / R2).
 *
 * Reads package.json "exports" at runtime, imports each subpath via the
 * package name ("zbrain/<subpath>") so it actually exercises the
 * resolver — then asserts each resolves AND has at least one canary
 * symbol. Importing from relative paths (e.g. "../src/core/engine.ts")
 * would bypass the exports map and miss resolver/wiring breakage.
 *
 * The canary symbols are concrete values each module re-exports today.
 * If a refactor renames or removes one, this test fails in CI so the
 * downstream consumer (zbrain-evals) doesn't silently break first.
 *
 * To add a new public export: extend `EXPECTED_EXPORTS` below. To
 * REMOVE one: bump zbrain's minor (breaking-interface per CLAUDE.md
 * "Removing any of these is a breaking change going forward").
 */

import { readFileSync } from 'fs';
import { resolve } from 'path';
import { describe, expect, test } from 'bun:test';

interface ExpectedExport {
  /** Subpath key as it appears in package.json exports. */
  subpath: string;
  /** At least one named export that MUST exist at runtime. Chosen from the
   *  module's current surface; if it goes away, that's a breaking change. */
  canary: string[];
}

/**
 * Canary symbols pinned to the v0.21.0 contract. Changes to this list
 * are intentional breaking changes to the public exports surface.
 */
const EXPECTED_EXPORTS: ExpectedExport[] = [
  { subpath: 'zbrain', canary: [] }, // root "." export; no single canary — just require import success
  { subpath: 'zbrain/engine', canary: ['clampSearchLimit', 'MAX_SEARCH_LIMIT'] },
  { subpath: 'zbrain/types', canary: ['ZBrainError'] },
  { subpath: 'zbrain/operations', canary: ['operations', 'OperationError'] },
  { subpath: 'zbrain/minions', canary: [] }, // barrel module; re-exports many names
  { subpath: 'zbrain/engine-factory', canary: [] }, // factory exports a default creator
  { subpath: 'zbrain/pglite-engine', canary: ['PGLiteEngine'] },
  { subpath: 'zbrain/link-extraction', canary: ['extractEntityRefs', 'extractPageLinks'] },
  { subpath: 'zbrain/import-file', canary: ['importFromContent'] },
  { subpath: 'zbrain/transcription', canary: [] },
  { subpath: 'zbrain/embedding', canary: ['embed'] },
  { subpath: 'zbrain/config', canary: ['loadConfig'] },
  { subpath: 'zbrain/markdown', canary: ['splitBody', 'parseMarkdown', 'serializeMarkdown'] },
  { subpath: 'zbrain/backoff', canary: [] },
  { subpath: 'zbrain/search/hybrid', canary: ['hybridSearch', 'rrfFusion'] },
  { subpath: 'zbrain/search/expansion', canary: ['expandQuery'] },
  { subpath: 'zbrain/ai/gateway', canary: ['configureGateway', 'embed'] },
  { subpath: 'zbrain/extract', canary: [] },
  { subpath: 'zbrain/ingestion', canary: ['INGESTION_SOURCE_API_VERSION', 'validateIngestionEvent', 'computeContentHash'] },
  { subpath: 'zbrain/ingestion/test-harness', canary: ['IngestionTestHarness', 'expectEvent'] },
];

function readPackageExports(): Record<string, string> {
  const pkgPath = resolve(import.meta.dir, '..', 'package.json');
  const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
  return pkg.exports as Record<string, string>;
}

describe('public exports — package.json exports map', () => {
  test('has the expected number of subpaths (v0.21.0 locks the surface)', () => {
    const exports = readPackageExports();
    const count = Object.keys(exports).length;
    // Adding new exports: increment this + add to EXPECTED_EXPORTS below.
    // Removing exports: see CLAUDE.md "Removing any of these is a
    // breaking change going forward" — bump minor and update this count.
    expect(count).toBe(20);
  });

  test('EXPECTED_EXPORTS list matches the exports map exactly (no drift)', () => {
    const exports = readPackageExports();
    const exportedSubpaths = Object.keys(exports).map(k => (k === '.' ? 'zbrain' : `zbrain${k.slice(1)}`)).sort();
    const expectedSubpaths = EXPECTED_EXPORTS.map(e => e.subpath).sort();
    expect(expectedSubpaths).toEqual(exportedSubpaths);
  });
});

describe('public exports — every subpath resolves via package name', () => {
  for (const entry of EXPECTED_EXPORTS) {
    test(`${entry.subpath} imports without throwing`, async () => {
      // Package-path import goes through the exports map — bypassing a
      // broken/removed subpath surfaces here. Importing "../src/..."
      // would resolve via filesystem and miss the contract.
      const mod = await import(entry.subpath);
      expect(mod).toBeDefined();
      expect(typeof mod).toBe('object');
    });

    if (entry.canary.length > 0) {
      test(`${entry.subpath} exports canary symbols: ${entry.canary.join(', ')}`, async () => {
        const mod = await import(entry.subpath);
        for (const name of entry.canary) {
          expect(mod).toHaveProperty(name);
          // Must be something truthy — value, class, or function. Not
          // just a TypeScript type (those don't exist at runtime).
          expect(mod[name]).toBeDefined();
        }
      });
    }
  }
});
