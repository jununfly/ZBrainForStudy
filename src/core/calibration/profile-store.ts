/**
 * Calibration profile read helpers.
 *
 * Relocated from `src/commands/calibration.ts` (2026-08-05, G66) so that the
 * live `eval-longmemeval` TS feature can keep reading the latest calibration
 * profile without depending on the legacy TS `core/cycle` engine. This module
 * has NO dependency on `core/cycle` — it only needs `BrainEngine.executeRaw`.
 *
 * The `zbrain calibration` CLI command itself is now served by the Rust
 * binary; this file is purely the shared read path used by
 * `cross-brain` / `nudge` / `recall-footer` / `calibration-join` / `think`.
 */

import type { BrainEngine } from '../engine.ts';

export interface CalibrationProfileRow {
  id: number;
  source_id: string;
  holder: string;
  wave_version: string;
  generated_at: string;
  published: boolean;
  total_resolved: number;
  brier: number | null;
  accuracy: number | null;
  partial_rate: number | null;
  grade_completion: number;
  pattern_statements: string[];
  active_bias_tags: string[];
  voice_gate_passed: boolean;
  voice_gate_attempts: number;
  model_id: string;
}

/** Source-scoped read of the latest profile row for a holder. */
export async function getLatestProfile(
  engine: BrainEngine,
  opts: { holder: string; sourceId?: string; sourceIds?: string[] },
): Promise<CalibrationProfileRow | null> {
  let sql = `SELECT id, source_id, holder, wave_version, generated_at, published,
            total_resolved, brier, accuracy, partial_rate, grade_completion,
            pattern_statements, active_bias_tags,
            voice_gate_passed, voice_gate_attempts, model_id
       FROM calibration_profiles
       WHERE holder = $1`;
  const params: unknown[] = [opts.holder];

  if (opts.sourceIds && opts.sourceIds.length > 0) {
    sql += ` AND source_id = ANY($2::text[])`;
    params.push(opts.sourceIds);
  } else if (opts.sourceId) {
    sql += ` AND source_id = $2`;
    params.push(opts.sourceId);
  }

  sql += ` ORDER BY generated_at DESC LIMIT 1`;

  const rows = await engine.executeRaw<CalibrationProfileRow>(sql, params);
  return rows[0] ?? null;
}
