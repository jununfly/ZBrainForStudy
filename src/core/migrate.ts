/**
 * Schema migrations bridge to Rust backend.
 *
 * HARD CUTOVER: TypeScript is no longer the source of truth for migrations.
 * All SQL migration logic now lives in:
 *   - crates/zbrain-core/src/libsql.rs (SQLite/libsql backend)
 *   - crates/zbrain-core/src/postgres.rs (Postgres backend)
 *   - crates/zbrain-core/src/migration.rs (shared trait)
 *   - crates/zbrain-core/migrations/ and migrations-sqlite/ (SQL files)
 *
 * This file exists ONLY as a bridge for existing callers. It delegates
 * 100% to engine.initSchema(). All migration decision logic lives in Rust.
 */

import type { BrainEngine } from './engine.ts';

/**
 * Run all pending schema migrations against the given engine.
 * Delegates directly to engine.initSchema().
 * Returns { applied, current } for backward compatibility with callers.
 */
export async function runMigrations(
  engine: BrainEngine,
  _enableVerify?: boolean,
): Promise<{ applied: number; current: number; status: string; error?: Error }> {
  await engine.initSchema();
  return { applied: 0, current: LATEST_VERSION, status: 'success' };
}

/**
 * Latest schema version. Always matches the highest version number in
 * the Rust migration registry. For diagnostics and health checks only.
 */
export const LATEST_VERSION = 9;

/**
 * Legacy stub: removed. Returns empty array.
 */
export function getIdleBlockers(_engine: BrainEngine): Promise<IdleBlocker[]> {
  return Promise.resolve([]);
}

/**
 * Legacy stub: always returns success with latest version.
 */
export async function tryRunPendingMigrations(
  _engine: BrainEngine,
  _enableVerify?: boolean,
): Promise<{ applied: number; current: number; status: string; error?: Error }> {
  return { applied: 0, current: LATEST_VERSION, status: 'success', error: undefined };
}

// Legacy types for backward compatibility with code that imports them.
// These do nothing but satisfy TypeScript type-checking.
export interface Migration {
  version: number;
  name: string;
  sql: string;
  sqlFor?: { postgres?: string; pglite?: string };
  transaction?: boolean;
  handler?: (engine: BrainEngine) => Promise<void>;
  idempotent?: boolean;
  verify?: (engine: BrainEngine) => Promise<boolean>;
}

export interface IdleBlocker {
  pid: number;
  query: string;
  duration: string;
  query_start?: string;
  state?: string;
}

export class MigrationDriftError extends Error {
  constructor(
    public readonly version: number,
    public readonly migrationName: string,
    public readonly hint: string,
  ) {
    super(`Migration v${version} (${migrationName}) verify failed: ${hint}`);
    this.name = 'MigrationDriftError';
  }
}

export class MigrationRetryExhausted extends Error {
  constructor(
    public readonly version: number,
    public readonly migrationName: string,
    public readonly attempts: number,
    public readonly lastBlockers: IdleBlocker[],
    public readonly lastError: Error,
  ) {
    const lastB = lastBlockers[0];
    super(
      `Migration v${version} (${migrationName}) failed after ${attempts} attempts. ${lastB ? `Blocked by PID ${lastB.pid}.` : ''}. Original: ${lastError.message}`,
    );
    this.name = 'MigrationRetryExhausted';
  }
}

/**
 * Legacy stub: always returns true.
 */
export function isMigrationIdempotent(_m: Migration): boolean {
  return true;
}

/**
 * Legacy stub: returns false.
 */
export function isDeadlockError(_e: Error): boolean {
  return false;
}

/**
 * Legacy stub: always returns success.
 */
export async function hasPendingMigrations(_engine: BrainEngine): Promise<boolean> {
  return false;
}

/**
 * Legacy type stub.
 */
export type TryRunPendingMigrationsResult = {
  applied: number;
  current: number;
  status: string;
  error?: Error;
  attempts?: number;
};

/**
 * Legacy stub: empty array for code that still references MIGRATIONS.
 * All actual migrations now live in Rust (crates/zbrain-core/migrations/).
 */
export const MIGRATIONS: Migration[] = [];

// HARD CUTOVER: MIGRATIONS array and all embedded SQL constants removed.
// Rust is now the single source of truth for all schema migrations.
//
// If you're looking for MIGRATIONS array or MIGRATION_* SQL constants:
//   → they no longer exist in TypeScript.
//   → look in crates/zbrain-core/migrations/ instead.
//   → look in crates/zbrain-core/src/libsql.rs for LIBQL_MIGRATIONS registry.
//   → look in crates/zbrain-core/src/postgres.rs for POSTGRES_MIGRATIONS registry.
