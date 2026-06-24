import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { MIGRATIONS } from '../../src/core/migrate.ts';
import { PGLiteEngine } from '../../src/core/pglite-engine.ts';

describe('DB legacy identifier rename', () => {
  test('fresh schema uses ZBrain table and column names for new databases', () => {
    const schema = readFileSync(resolve(import.meta.dir, '../../src/schema.sql'), 'utf8');
    expect(schema).toContain('CREATE TABLE IF NOT EXISTS zbrain_cycle_locks');
    expect(schema).toContain('zbrain_tool_use_id  UUID');
    expect(schema).not.toContain('CREATE TABLE IF NOT EXISTS gbrain_cycle_locks');
    expect(schema).not.toContain('gbrain_tool_use_id  UUID');
  });

  test('migration v98 renames old DB identifiers to ZBrain names', () => {
    const v98 = MIGRATIONS.find(m => m.version === 98);
    expect(v98).toBeDefined();
    expect(v98?.name).toBe('db_legacy_identifier_rename');
    const sql = v98!.sql || '';
    expect(sql).toContain('ALTER TABLE IF EXISTS gbrain_cycle_locks RENAME TO zbrain_cycle_locks');
    expect(sql).toContain('ALTER TABLE subagent_tool_executions RENAME COLUMN gbrain_tool_use_id TO zbrain_tool_use_id');
    expect(sql).toContain("to_regclass('public.zbrain_cycle_locks') IS NULL");
    expect(sql).toContain('column_name = \'zbrain_tool_use_id\'');
  });

  test('new PGLite databases expose only ZBrain identifiers after initSchema', async () => {
    const engine = new PGLiteEngine();
    await engine.connect({});
    try {
      await engine.initSchema();
      const tables = await engine.executeRaw<{ tablename: string }>(
        `SELECT tablename FROM pg_tables WHERE schemaname = 'public'`,
      );
      const tableNames = tables.map(r => r.tablename);
      expect(tableNames).toContain('zbrain_cycle_locks');
      expect(tableNames).not.toContain('gbrain_cycle_locks');

      const columns = await engine.executeRaw<{ column_name: string }>(
        `SELECT column_name FROM information_schema.columns
          WHERE table_name = 'subagent_tool_executions'`,
      );
      const columnNames = columns.map(r => r.column_name);
      expect(columnNames).toContain('zbrain_tool_use_id');
      expect(columnNames).not.toContain('gbrain_tool_use_id');
    } finally {
      await engine.disconnect();
    }
  });
});
