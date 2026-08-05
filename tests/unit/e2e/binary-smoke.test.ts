/**
 * Binary smoke E2E — re-routed to the real Rust `zbrain` artifact (#143).
 *
 * These tests do NOT import any TS command function. They spawn the actual
 * shipped binary (resolved by ./spawn-zbrain.ts) and assert on its stdout /
 * exit code / side effects. This is the E2E discipline going forward: validate
 * the binary users run, not in-process TS calls that will be deleted.
 *
 * Two layers:
 *   1. CLI-surface checks (--version, --help, schema-sql) — deterministic, no
 *      DB or API key required. Prove the migrated `dream`/`calibration`
 *      subcommands are wired into the real binary.
 *   2. A REAL engine round-trip: `init --pglite --config <tmp>/zbrain.yml`
 *      writes a brain DB, and `list-pages --config <tmp>/zbrain.yml` reads it
 *      back as JSON. No network / LLM needed — pure local libsql/pglite path.
 *
 * Gating: skipped when no binary resolves (local dev without a build). CI
 * builds the binary into target/ first, so it runs there. The test writes
 * everything under a mkdtemp dir + uses an explicit --config, so it never
 * touches the user's real ~/.zbrain (and run-e2e.sh isolates HOME anyway).
 */

import { describe, it, expect, beforeAll, afterAll } from 'bun:test';
import { mkdtempSync, existsSync, rmSync, writeFileSync, statSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { binaryAvailable, runZbrainOk } from './spawn-zbrain.ts';

describe.skipIf(!binaryAvailable())('zbrain binary E2E (re-routed to Rust artifact)', () => {
  let tmpDir: string;

  beforeAll(() => {
    tmpDir = mkdtempSync(join(tmpdir(), 'zbrain-binary-e2e-'));
  });

  afterAll(() => {
    if (tmpDir) rmSync(tmpDir, { recursive: true, force: true });
  });

  it('exposes a semver version string', () => {
    const out = runZbrainOk(['--version']);
    expect(out).toMatch(/zbrain\s+\d+\.\d+\.\d+/);
  });

  it('lists the migrated dream + calibration subcommands in top-level help', () => {
    const out = runZbrainOk(['--help']);
    expect(out).toContain('dream');
    expect(out).toContain('calibration');
  });

  it('renders dream subcommand help with its phase/dry-run flags', () => {
    const out = runZbrainOk(['dream', '--help']);
    // Flags added when the dream command was ported to Rust (1-6-8).
    expect(out).toContain('--dry-run');
    expect(out).toContain('--phase');
  });

  it('renders calibration subcommand help with its mode flags', () => {
    const out = runZbrainOk(['calibration', '--help']);
    // Flags added when the calibration command was ported to Rust (1-6-8).
    expect(out).toContain('--regenerate');
    expect(out).toContain('--undo-wave');
  });

  it('prints the libsql/SQLite schema DDL (no DB required)', () => {
    const out = runZbrainOk(['schema-sql']);
    expect(out).toContain('ZBrain libsql/SQLite Schema');
  });

  it('initializes a pglite brain and reads it back via list-pages', () => {
    const cfgPath = join(tmpDir, 'zbrain.yml');
    writeFileSync(
      cfgPath,
      'storage:\n  db_tracked:\n    - concepts/\n  db_only:\n    - media/x/\n',
    );

    // Real engine round-trip: the binary writes a brain DB to the temp dir.
    runZbrainOk(['init', '--config', cfgPath, '--pglite', '--force', '--non-interactive']);

    const dbPath = join(tmpDir, 'brain.pglite');
    expect(existsSync(dbPath)).toBe(true);
    // The DB file must actually have bytes (not a zero-length stub).
    expect(statSync(dbPath).size).toBeGreaterThan(0);

    // And the binary reads it back as JSON — proving init + list-pages both
    // talk to the same local libsql/pglite backend end-to-end.
    const listOut = runZbrainOk(['list-pages', '--config', cfgPath]);
    const parsed = JSON.parse(listOut);
    expect(parsed).toHaveProperty('pages');
    expect(Array.isArray(parsed.pages)).toBe(true);
    expect(parsed.pages.length).toBe(0);
  });
});
