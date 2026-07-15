/**
 * Resolve the zbrain CLI entrypoint for spawning worker child processes.
 *
 * Extracted during the Phase 11 TS→Rust deletion: the autopilot command
 * itself has been ported to Rust, but `jobs.ts` (a still-live TS command)
 * needs this resolver to locate the executable it supervises. Kept in a
 * neutral module so removing the ported autopilot code does not take it down.
 *
 * A .ts source path is never a valid spawn target — spawning it fails with
 * EACCES because TypeScript source isn't executable. The canonical install
 * puts a shim at `/usr/local/bin/zbrain` (or wherever `which zbrain`
 * resolves to) that already wraps the right runtime+entrypoint; prefer it.
 *
 * Order of resolution:
 *   1. `which zbrain` — the shim on PATH, canonical for installed builds.
 *   2. process.execPath if it ends with /zbrain (compiled binary, no shim).
 *   3. argv[1] if it ends with /zbrain (e.g., direct invocation of compiled
 *      binary without PATH). Never .ts source paths.
 *   4. Throw with a clear install hint.
 */
import { execSync } from 'child_process';

export function resolveGbrainCliPath(): string {
  try {
    const which = execSync('which zbrain', { encoding: 'utf-8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
    if (which) return which;
  } catch { /* not on $PATH — fall through */ }

  const exec = process.execPath ?? '';
  if (exec.endsWith('/zbrain') || exec.endsWith('\\zbrain.exe')) {
    return exec;
  }

  const arg1 = process.argv[1] ?? '';
  if (arg1.endsWith('/zbrain') || arg1.endsWith('\\zbrain.exe')) {
    return arg1;
  }

  throw new Error('Could not resolve the zbrain CLI path. Install zbrain so it is on $PATH (e.g. /usr/local/bin/zbrain), or run autopilot from the compiled binary directly.');
}
