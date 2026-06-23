# PRD: Complete TS -> Rust

## Problem Statement

ZBrain is moving from the TypeScript legacy line to the Rust rewrite line. The repository currently still contains a large TypeScript implementation, TypeScript-oriented docs, tests, scripts, package metadata, and historical GBrain naming. Directly deleting the TypeScript side would be unsafe because the Rust rewrite does not yet cover every behavior and some TypeScript code still serves as the reference implementation during migration.

The project needs a clear product requirement for completing the TS -> Rust migration without creating a big-bang rewrite trap: migrate behavior into Rust slice by slice, delete the corresponding TypeScript surface when each slice is proven replaced, and explicitly discuss any TypeScript residue that cannot be deleted cleanly.

## Solution

Complete ZBrain by making Rust the canonical implementation and shrinking the TypeScript legacy line naturally as Rust slices land. TS code is not mechanically removed at the start of this PRD. Instead, each migration slice must define the Rust replacement, prove behavior through tests or parity checks, then delete the matching TypeScript code, tests, scripts, and docs that are no longer needed.

If a TypeScript component cannot be deleted after its apparent Rust replacement lands, the team must stop and make a decision: keep it temporarily with a named reason, port the missing behavior, or redesign the boundary.

## User Stories

1. As a ZBrain maintainer, I want Rust to become the canonical implementation, so that the repository no longer splits attention between two product lines.
2. As a ZBrain maintainer, I want TypeScript code to remain until its behavior has a Rust replacement, so that migration does not delete useful reference behavior too early.
3. As a ZBrain maintainer, I want each migrated slice to delete the corresponding TypeScript surface, so that the legacy line shrinks continuously instead of accumulating dead code.
4. As a ZBrain maintainer, I want unclear deletion cases to become explicit decisions, so that ambiguous TypeScript residue does not linger by accident.
5. As a ZBrain maintainer, I want the Rust crates to own the core brain contract, so that operations, storage engines, CLI, and MCP behavior converge on one runtime.
6. As a CLI user, I want the final command surface to be ZBrain-native, so that commands, examples, configuration, and help text use one product language.
7. As an agent caller, I want MCP and operation trust boundaries to survive the migration, so that remote agent callers remain confined while trusted local callers keep local capabilities.
8. As a documentation reader, I want docs to describe the Rust-first ZBrain architecture, so that I do not have to reverse-engineer which parts are legacy.
9. As a contributor, I want migration slices to be independently reviewable, so that each PR has a clear scope and does not mix unrelated deletions.
10. As a contributor, I want tests to move with behavior, so that deleting TypeScript code does not delete the only coverage for a feature.
11. As a maintainer, I want package, bin, env, dotfile, and config names to become ZBrain-native, so that the project language is consistent after the rename.
12. As a maintainer, I want historical GBrain release notes to stay reset, so that ZBrain starts from a clean unreleased baseline.
13. As a maintainer, I want the TypeScript admin/frontend surface to be evaluated separately from core runtime code, so that reusable UI assets are not destroyed before their Rust-backed replacement is clear.
14. As a maintainer, I want migration leftovers to be visible, so that `src/`, `tests/unit/`, docs, recipes, and scripts do not silently become archaeological layers.
15. As a maintainer, I want final verification to prove that ZBrain builds and tests without depending on deleted TypeScript code, so that the migration closes cleanly.

## Implementation Decisions

- ZBrain is the canonical product language. GBrain naming is not preserved for compatibility because there are no online users.
- TypeScript code is not directly deleted at the beginning of the PRD.
- Migration proceeds slice by slice: port behavior to Rust, prove it, then delete the corresponding TypeScript implementation and obsolete tests/docs/scripts.
- TypeScript residue that cannot be deleted naturally must trigger a decision before being kept.
- The Rust rewrite line owns the target architecture. Existing Rust crates already use ZBrain naming and remain the destination structure.
- Brand/interface rename is still in scope for the broader repository cleanup: package name, CLI bin, env vars, dotfiles, config files, command examples, and docs should become ZBrain-native.
- `brain` remains a domain term and should not be replaced just because `gbrain` is being renamed.
- Historical GBrain changelog content is deleted/reset. ZBrain release history starts from the first ZBrain release.
- Plans and tests cleanup should prefer consolidation over mechanical deletion: reusable decisions become canonical docs; unreusable process files are removed after their content is distilled.
- The `tests/unit/` to `tests/` migration is part of repository cleanup, but test movement must preserve coverage and should happen alongside path/reference updates.

## Testing Decisions

- Test at behavior seams, not implementation details.
- For each migrated slice, use the highest available external seam: CLI behavior for command surfaces, operation contract tests for shared CLI/MCP operations, engine contract tests for storage behavior, and integration tests for cross-component flows.
- Existing TypeScript tests are reference material until Rust parity exists.
- A migrated slice is not complete until the Rust behavior is covered and the obsolete TypeScript tests/unit/code surface is removed or explicitly retained by decision.
- Final migration verification should include Rust workspace checks and repository-wide reference scans for stale GBrain/TypeScript legacy assumptions.
- Path cleanup must include checks for stale `tests/unit/` references after migration to `tests/`.

## Out of Scope

- Big-bang deletion of the entire TypeScript implementation before Rust parity exists.
- Preserving GBrain compatibility aliases for hypothetical online users.
- Rewriting unrelated product behavior during migration cleanup.
- Treating historical GBrain release notes as a required compatibility artifact.

## Further Notes

This PRD intentionally treats the TypeScript implementation as a shrinking reference layer, not as a coequal long-term product line. The rule is simple: when Rust successfully replaces a slice, delete the corresponding TypeScript slice. When deletion is not obvious, make the decision explicit instead of letting legacy code survive by inertia.
