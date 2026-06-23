/**
 * Tests for the v0.40.3.0 doctor check `contextual_retrieval_coverage`.
 * Pins the SQL predicates + audit-summary integration end-to-end via
 * PGLite + the audit JSONL filesystem path.
 */

import { describe, test, expect, beforeAll, afterAll, beforeEach } from 'bun:test';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { PGLiteEngine } from '../../src/core/pglite-engine.ts';
import { checkContextualRetrievalCoverage } from '../../src/commands/doctor.ts';
import { MARKDOWN_CHUNKER_VERSION } from '../../src/core/chunkers/recursive.ts';

let engine: PGLiteEngine;
let tmpDir: string;
const originalEnv = process.env.ZBRAIN_AUDIT_DIR;

beforeAll(async () => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'zbrain-doctor-cr-test-'));
  process.env.ZBRAIN_AUDIT_DIR = tmpDir;
  engine = new PGLiteEngine();
  await engine.connect({});
  await engine.initSchema();
});

afterAll(async () => {
  if (originalEnv === undefined) delete process.env.ZBRAIN_AUDIT_DIR;
  else process.env.ZBRAIN_AUDIT_DIR = originalEnv;
  fs.rmSync(tmpDir, { recursive: true, force: true });
  await engine.disconnect();
});

beforeEach(async () => {
  // Wipe pages between tests so each starts from a known state.
  await engine.executeRaw(`DELETE FROM pages WHERE slug LIKE 'tests/unit/%'`);
  // Wipe audit files too.
  for (const f of fs.readdirSync(tmpDir)) {
    fs.unlinkSync(path.join(tmpDir, f));
  }
});

describe('contextual_retrieval_coverage doctor check', () => {
  test('empty brain reports ok', async () => {
    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.name).toBe('contextual_retrieval_coverage');
    expect(result.status).toBe('ok');
  });

  test('fully aligned brain reports ok', async () => {
    // Insert a page at current chunker version with mode stamped.
    await engine.executeRaw(
      `INSERT INTO pages (source_id, slug, type, title, compiled_truth, chunker_version, contextual_retrieval_mode)
       VALUES ('default', $1, 'concept', 'Aligned', 'body', $2, 'title')`,
      ['tests/unit/aligned', MARKDOWN_CHUNKER_VERSION],
    );
    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.status).toBe('ok');
    expect(result.message).toContain('aligned');
  });

  test('chunker_version drift is flagged with fix hint', async () => {
    await engine.executeRaw(
      `INSERT INTO pages (source_id, slug, type, title, compiled_truth, chunker_version, contextual_retrieval_mode)
       VALUES ('default', $1, 'concept', 'Old chunker', 'body', 2, 'title')`,
      ['tests/unit/old-chunker'],
    );
    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.status).toBe('warn');
    expect(result.message).toContain('older chunker_version');
    expect(result.message).toContain('zbrain reindex --markdown');
  });

  test('NULL mode column is flagged separately', async () => {
    await engine.executeRaw(
      `INSERT INTO pages (source_id, slug, type, title, compiled_truth, chunker_version, contextual_retrieval_mode)
       VALUES ('default', $1, 'concept', 'No mode', 'body', $2, NULL)`,
      ['tests/unit/no-mode', MARKDOWN_CHUNKER_VERSION],
    );
    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.status).toBe('warn');
    expect(result.message).toContain('never evaluated against CR ladder');
  });

  test('both drift conditions surface together', async () => {
    await engine.executeRaw(
      `INSERT INTO pages (source_id, slug, type, title, compiled_truth, chunker_version, contextual_retrieval_mode)
       VALUES ('default', $1, 'concept', 'Both', 'body', 2, NULL),
              ('default', $2, 'concept', 'Old', 'body', 2, 'title'),
              ('default', $3, 'concept', 'Null mode', 'body', $4, NULL)`,
      ['tests/unit/both-drift', 'tests/unit/old-only', 'tests/unit/null-only', MARKDOWN_CHUNKER_VERSION],
    );
    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.status).toBe('warn');
    expect(result.message).toContain('older chunker_version');
    expect(result.message).toContain('never evaluated against CR ladder');
  });

  test('soft-deleted pages are not counted', async () => {
    await engine.executeRaw(
      `INSERT INTO pages (source_id, slug, type, title, compiled_truth, chunker_version, contextual_retrieval_mode, deleted_at)
       VALUES ('default', $1, 'concept', 'Soft-deleted', 'body', 2, NULL, now())`,
      ['tests/unit/deleted-old'],
    );
    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.status).toBe('ok');
  });

  test('non-markdown pages (code) are not counted', async () => {
    await engine.executeRaw(
      `INSERT INTO pages (source_id, slug, type, title, compiled_truth, chunker_version, contextual_retrieval_mode, page_kind)
       VALUES ('default', $1, 'code', 'Code page', 'body', 2, NULL, 'code')`,
      ['tests/unit/code-page'],
    );
    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.status).toBe('ok');
  });

  test('audit summary line surfaces recent synopsis failures', async () => {
    // Drop a failure event into the audit JSONL.
    const { computeSynopsisAuditFilename } = await import('../../src/core/audit-synopsis.ts');
    const filename = computeSynopsisAuditFilename();
    const event = {
      ts: new Date().toISOString(),
      page_slug: 'tests/unit/some-page',
      source_id: 'default',
      chunk_index: 0,
      kind: 'refusal' as const,
      detail: 'stop_reason=content_filter',
      page_level_fallback: true,
      severity: 'warn' as const,
    };
    fs.writeFileSync(path.join(tmpDir, filename), JSON.stringify(event) + '\n');

    const result = await checkContextualRetrievalCoverage(engine);
    expect(result.message).toContain('synopsis failure');
    expect(result.message).toContain('1 triggered page-level fall-back');
  });
});
