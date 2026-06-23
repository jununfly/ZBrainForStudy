# Adopt ZBrain as the repository-wide product language

The repository is in a TypeScript-to-Rust migration, and the Rust rewrite line is the intended future of the product. We will use **ZBrain** as the canonical project and product name across docs, roadmap, user-facing language, CLI/package/env/dotfile surfaces, and new architectural decisions; there are currently no online users, so the first migration phase may make breaking renames instead of preserving GBrain compatibility aliases.

## Considered Options

- Keep GBrain as the product name while Rust remains an implementation detail.
- Split the language into GBrain for TypeScript and ZBrain for Rust.
- Move the whole repository language to ZBrain while staging GBrain compatibility.
- Move the whole repository language to ZBrain in one breaking first phase.

## Consequences

First-phase cleanup should rename GBrain to ZBrain across brand text and breaking user-facing surfaces, including executable names, package names, environment variables, dotfiles, config files, and public command examples. Historical GBrain changelog content is not meaningful for the unreleased ZBrain project and should be deleted/reset rather than preserved.
