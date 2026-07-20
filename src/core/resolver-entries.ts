/**
 * resolver-entries.ts — Standalone parser for RESOLVER.md / AGENTS.md.
 *
 * Extracted from the former `src/core/check-resolvable.ts` during the
 * TS→Rust migration (roadmap 1-6-5-9). `parseResolverEntries` is a shared
 * primitive consumed by living TS modules (`mounts-cache.ts`,
 * `skill-trigger-index.ts`); it is NOT part of the resolvable-validation
 * stack that moved to Rust `zbrain_core::skill_resolver`, so it lives here
 * on its own.
 *
 * The Rust port of the resolver trigger index (`skill_trigger_index.rs`)
 * reimplements this parsing independently; this TS copy remains only for
 * the TS-side consumers until they migrate.
 */

export interface ResolverEntry {
  trigger: string;
  skillPath: string; // e.g., 'skills/query/SKILL.md'
  isGStack: boolean; // GStack: X entries (external, skip file check)
  section: string; // e.g., 'Brain operations'
}

/**
 * Parse RESOLVER.md / AGENTS.md into structured entries. Supports two formats
 * that can mix in one file:
 *
 *   Format 1 (table) — original zbrain shape:
 *     | trigger phrase | `skills/<name>/SKILL.md` |
 *
 *   Format 2 (compact list, v0.41.7.0) — OpenClaw-native shape:
 *     - **skill-name**: trigger1 | trigger2 | trigger3
 *     - skill-name: trigger1 | trigger2
 *
 * List-format constraints (v0.41.7.0):
 *   - Skill name MUST be kebab-lowercase (`[a-z][a-z0-9-]+`). Bold names
 *     like `**Note**`, `**Convention**`, `**TODO**` are deliberately
 *     skipped so prose bullets in real-world AGENTS.md files don't get
 *     mis-parsed as skill rows.
 *   - `skillPath` is ALWAYS derived as `skills/<name>/SKILL.md`. An
 *     optional `→ \`skills/path\`` (or ASCII `->`) suffix is stripped from
 *     the trigger string but NOT honored as the path: downstream consumers
 *     both assume the convention. For non-conventional paths, use the table
 *     format.
 *   - Multiple triggers fan out to one entry per trigger, all sharing the
 *     same `skillPath`. Downstream dedupes by `skillPath`, so the
 *     integration reachability count counts each skill once.
 */
export function parseResolverEntries(resolverContent: string): ResolverEntry[] {
  const entries: ResolverEntry[] = [];
  let currentSection = '';

  for (const line of resolverContent.split('\n')) {
    // Track section headings
    const headingMatch = line.match(/^##\s+(.+)/);
    if (headingMatch) {
      currentSection = headingMatch[1].trim();
      continue;
    }

    // ── Format 1: Markdown table rows ──
    if (line.startsWith('|') && !line.includes('---')) {
      const cols = line.split('|').map((c) => c.trim()).filter(Boolean);
      if (cols.length < 2) continue;

      const trigger = cols[0];
      const skillCol = cols[1];

      // Skip header rows
      if (trigger.toLowerCase() === 'trigger' || trigger.toLowerCase() === 'skill') continue;

      // GStack / external references (Check `ACCESS_POLICY.md`, Read X, GStack: Y)
      if (
        skillCol.startsWith('GStack:') ||
        skillCol.startsWith('Check ') ||
        skillCol.startsWith('Read ')
      ) {
        entries.push({ trigger, skillPath: skillCol, isGStack: true, section: currentSection });
        continue;
      }

      // Backtick-wrapped skill path
      const pathMatch = skillCol.match(/`(skills\/[^`]+\/SKILL\.md)`/);
      if (pathMatch) {
        entries.push({ trigger, skillPath: pathMatch[1], isGStack: false, section: currentSection });
      }
      continue;
    }

    // ── Format 2: Compact list rows (v0.41.7.0) ──
    // Bold form preferred: `- **skill-name**: trigger1 | trigger2`
    // Plain fallback:     `- skill-name: trigger1 | trigger2`
    // Name regex is kebab-lowercase only so prose bullets like `- **Note**: …`
    // don't false-match as skill rows (codex F2 / D4).
    const listBold = line.match(/^-\s+\*\*([a-z][a-z0-9-]+)\*\*\s*:\s*(.+)$/);
    const listPlain = listBold ? null : line.match(/^-\s+([a-z][a-z0-9-]+)\s*:\s*(.+)$/);
    const listMatch = listBold ?? listPlain;
    if (listMatch) {
      const skillName = listMatch[1];
      const triggersRaw = listMatch[2].trim();
      // Strip optional explicit path suffix (D3: stripped, NOT captured).
      // Both Unicode → and ASCII -> accepted; skillPath is always derived.
      const cleaned = triggersRaw.replace(/\s*(?:→|->)\s*`skills\/[^`]+`\s*$/, '');
      // Split on |, drop empty pieces and the literal `...` placeholder.
      const triggers = cleaned
        .split('|')
        .map((t) => t.trim())
        .filter((t) => t.length > 0 && t !== '...');
      const skillPath = `skills/${skillName}/SKILL.md`;
      // Multiple entries share skillPath; downstream dedupes.
      for (const trigger of triggers) {
        entries.push({ trigger, skillPath, isGStack: false, section: currentSection });
      }
    }
  }

  return entries;
}
