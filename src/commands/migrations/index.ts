/**
 * TS migration registry — metadata only.
 *
 * The orchestrator RUNNERS (the 15 `v0_*.ts` modules) were migrated to Rust
 * in `crates/zbrain-cli/src/apply_migrations.rs` (roadmap 1-6-4-14, C'
 * pragmatic port). This file now keeps only the static metadata the
 * post-upgrade pitch path (`upgrade.ts`) and the thin-client version check
 * (`thin-client-upgrade-prompt.ts`) need: the version list + feature pitches,
 * plus `compareVersions`. The Rust `zbrain apply-migrations` binary owns the
 * actual run loop and ledger.
 */

import type { FeaturePitch } from './types.ts';

/** Version + feature pitch, without the (now-Rust) orchestrator function. */
export interface MigrationMeta {
  version: string;
  featurePitch: FeaturePitch;
}

export const migrations: MigrationMeta[] = [
  { version: '0.11.0', featurePitch: { headline: 'ZBrain Minions — durable background agents' } },
  { version: '0.12.0', featurePitch: { headline: 'Knowledge Graph wires itself — every page write extracts typed links automatically' } },
  { version: '0.12.2', featurePitch: { headline: 'Postgres frontmatter queries now work — JSONB double-encode bug fixed and existing rows auto-repaired' } },
  { version: '0.13.0', featurePitch: { headline: 'Frontmatter becomes a graph — company, investors, attendees now create typed edges automatically' } },
  { version: '0.13.1', featurePitch: { headline: 'BrainWriter integrity + grandfather protection for existing pages.' } },
  { version: '0.14.0', featurePitch: { headline: 'Shell jobs + autopilot cooperative handler + max_stalled default bump.' } },
  { version: '0.16.0', featurePitch: { headline: 'Durable LLM agents land in the brain — survive crashes, sleeps, and worker restarts.' } },
  { version: '0.18.0', featurePitch: { headline: 'Multi-source brains: one database, many knowledge repos. Federation flag keeps them from polluting each other.' } },
  { version: '0.18.1', featurePitch: { headline: 'Row Level Security hardened on all public tables + escape hatch.' } },
  { version: '0.21.0', featurePitch: { headline: 'Code Cathedral II — chunk-grain FTS, qualified symbols, structural edges, 165-language lazy-load' } },
  { version: '0.22.4', featurePitch: { headline: "Frontmatter-guard ships — broken brain pages can't hide" } },
  { version: '0.28.0', featurePitch: { headline: "Takes ship — your brain finally captures what you BELIEVE, not just what's true" } },
  { version: '0.29.1', featurePitch: { headline: 'Recency + salience as two opt-in axes — agent in charge of when to use each' } },
  { version: '0.31.0', featurePitch: { headline: 'Hot memory ships — your brain remembers what you said today, across sessions' } },
  { version: '0.32.2', featurePitch: { headline: 'Facts join the system-of-record — your hot memory now lives in markdown, indexed by the DB' } },
];

/** Look up a migration by exact version string. */
export function getMigration(version: string): MigrationMeta | null {
  return migrations.find(m => m.version === version) ?? null;
}

export type { FeaturePitch } from './types.ts';

/**
 * Compare two semver strings (MAJOR.MINOR.PATCH). Returns -1 / 0 / 1.
 * Extracted from src/commands/upgrade.ts#isNewerThan for shared use across
 * the migration runner + post-upgrade pitch path.
 */
export function compareVersions(a: string, b: string): -1 | 0 | 1 {
  const va = a.split('.').map(n => parseInt(n, 10) || 0);
  const vb = b.split('.').map(n => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    const da = va[i] ?? 0;
    const db = vb[i] ?? 0;
    if (da > db) return 1;
    if (da < db) return -1;
  }
  return 0;
}
