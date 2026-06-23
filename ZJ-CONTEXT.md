# ZBrain

ZBrain is a Rust-first personal knowledge brain for agents. This repository is migrating from the old TypeScript GBrain line to the ZBrain Rust rewrite, and project language should use ZBrain as the canonical product name.

## Language

**ZBrain**:
The canonical product name for this repository and its Rust-first future. Use this for docs, roadmap, user-facing language, CLI/package/env/dotfile surfaces, and new architecture decisions.
_Avoid_: GBrain, gbrain.

**Rust rewrite line**:
The migration track that replaces the old TypeScript implementation with Rust crates and a Rust CLI/MCP surface.
_Avoid_: side project, fork, experiment.

**TypeScript legacy line**:
The existing TypeScript implementation kept as reference and compatibility surface during migration.
_Avoid_: main product, canonical implementation.

**Brain**:
A database-backed knowledge space. A brain answers which database is being queried or mutated.
_Avoid_: repo, source, workspace.

**Source**:
A content/repository scope inside a brain. A source answers which collection within the database a query or mutation belongs to.
_Avoid_: brain, database.

**Engine**:
The storage backend implementation behind the brain contract, such as PGLite, Postgres, or libsql.
_Avoid_: database when discussing the code abstraction.

**Operation**:
A named capability exposed through shared CLI/MCP contracts.
_Avoid_: endpoint, command when referring to the shared contract itself.

**Trusted local caller**:
A local CLI execution context allowed to perform filesystem-sensitive operations with local-user trust.
_Avoid_: admin user.

**Remote agent caller**:
An agent-facing execution context that must be treated as untrusted and confined by stricter operation rules.
_Avoid_: local caller, trusted caller.

**Skill**:
A markdown-driven agent capability package with resolver metadata, tests, and optional scripts.
_Avoid_: plugin when referring specifically to agent skills.
