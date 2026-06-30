<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-roadmap.json` | 最后更新: 2026-06-29 17:29:03

[~][X+] 1. ZBrain TS to Rust Migration
│   [x][Y+] 1-1. Roadmap and TypeScript runtime inventory
│   │   [x][Y+] 1-1-1. Restore canonical roadmap files
│   │   [x][Y+] 1-1-2. Expand complete TS to Rust PRD from codebase facts
│   │   [x][Y+] 1-1-3. Classify TypeScript runtime and frontend retention surfaces
│       [x][Y+] 1-1-4. Define per-slice deletion checklist and verification gates
│   [x][Y+] 1-2. Core storage parity closure
│   │   [x][Y+] 1-2-1. Finish Page contract parity across storage backends
│   │   │   [x][Y+] 1-2-1-1. Write Page contract parity audit plan
│   │   │   [x][Y+] 1-2-1-2. Align getPage no-source lookup semantics
│   │   │   [x][Y+] 1-2-1-3. Decide findDuplicatePage return shape parity
│   │   │   [x][Y+] 1-2-1-4. Align getPageTimestamps deleted-row visibility
│   │   │   [x][Y+] 1-2-1-5. Align getEffectiveDates effective_date fallback
│   │   │   [x][Y+] 1-2-1-6. Add Rust file storage contract parity
│   │       [x][Y+] 1-2-1-7. Implement resolveSlugs source and fuzzy parity
│   │   [x][Y+] 1-2-2. Port missing advanced Page writes to Rust
│   │   │   [x][Y+] 1-2-2-1. Write advanced Page writes audit plan
│   │   │   [x][Y+] 1-2-2-2. Add Rust advanced Page writes contract surface
│   │   │   [x][Y+] 1-2-2-3. Implement InMemory advanced Page writes behavior
│   │   │   [x][Y+] 1-2-2-4. Implement libsql advanced Page writes behavior
│   │   │   [x][Y+] 1-2-2-5. Implement Postgres advanced Page writes behavior
│   │       [x][Y+] 1-2-2-6. Validate and close advanced Page writes parity
│   │   [x][Y+] 1-2-3. Move schema migrations ownership to Rust
│   │   │   [x][X+] 1-2-3-1. Write schema migrations ownership audit plan
│   │   │   [x][Y+] 1-2-3-2. Add Rust Migration registry + runner foundation
│   │   │   [x][Y+] 1-2-3-3. Integrate Rust runner into libsql backend
│   │   │   [x][Y+] 1-2-3-4. Integrate Rust runner into Postgres backend
│   │   │   [x][Y+] 1-2-3-5. Build TS bridge + port handler/verify functions to Rust
│   │       [x][Y+] 1-2-3-6. Validate and close schema migrations ownership transfer
│   │   [x][X+] 1-2-4. Decide internal DB legacy identifier migration
│       [x][Y+] 1-2-5. Implement DB legacy identifier rename migration
│   [x][Y+] 1-3. Config bootstrap and package entrypoint cutover
│   │   [x][Y+] 1-3-1. Port config discovery loading and writing to Rust
│   │   [x][Y+] 1-3-2. Port init doctor config storage and schema commands
│   │   [x][Y+] 1-3-3. Cut package bin and install flow to Rust binary
│       [x][Y+] 1-3-4. Delete replaced TypeScript bootstrap command surface
│   [x][Y+] 1-4. Operations layer and trust boundary migration
│   │   [x][Y+] 1-4-1. Port operation definitions schemas and context
│   │   │   [x][Y+] 1-4-1-1. Slice #44 - First operation port: get_page end-to-end verification
│   │       [x][Y+] 1-4-1-2. Slice #45 - Port Pages CRUD operations (put_page, delete_page, restore_page, purge_deleted_pages)
│   │   [x][Y+] 1-4-2. Port local and remote trust boundary enforcement
│       [x][Y+] 1-4-3. Move shared CLI MCP dispatch to Rust operations
│   [~][Y+] 1-5. MCP server migration
│   │   [x][Y+] 1-5-1. Implement Rust MCP tool definitions and parameter validation (3 known gaps → 1-5-2)
│   │   [x][Y+] 1-5-2. Implement Rust MCP transports rate limiting and audit hooks
│       [ ][Y+] 1-5-3. Delete TypeScript MCP implementation after parity
│   [ ][Y+] 1-6. Web backend and admin API migration
│   │   [ ][Y+] 1-6-1. Implement Axum admin backend API
│   │   [ ][Y+] 1-6-2. Port auth session token request log jobs calibration and agents endpoints
│       [ ][X+] 1-6-3. Retain React TypeScript admin frontend by explicit decision
│   [ ][Y+] 1-7. Ingestion sources search and retrieval migration
│   │   [ ][Y+] 1-7-1. Port source management import capture extract and sync flows
│   │   [ ][Y+] 1-7-2. Port embeddings chunking hybrid search and reindex flows
│       [ ][Y+] 1-7-3. Delete replaced TypeScript ingestion search and source modules
│   [ ][Y+] 1-8. Facts takes timeline salience and graph migration
│   │   [ ][Y+] 1-8-1. Port facts takes timeline salience backlinks orphans and graph behavior
│       [ ][Y+] 1-8-2. Delete replaced TypeScript knowledge graph modules
│   [ ][Y+] 1-9. AI gateway providers models and routing migration
│   │   [ ][Y+] 1-9-1. Port provider config model capabilities pricing and routed gateway
│       [ ][Y+] 1-9-2. Preserve routed gateway and no direct provider guardrails
│   [ ][Y+] 1-10. Jobs agents minions autopilot and remote execution migration
│   │   [ ][Y+] 1-10-1. Port jobs lifecycle agent logs minions autopilot fanout and remote execution
│       [ ][Y+] 1-10-2. Preserve privacy PII and remote execution trust guardrails
│   [ ][Y+] 1-11. Evals benchmarks and developer tooling migration
│   │   [ ][X+] 1-11-1. Decide product critical evals and benchmarks
│       [ ][Y+] 1-11-2. Port or archive TypeScript eval and developer tooling
    [ ][Y+] 1-12. Final cutover and TypeScript runtime cleanup
    │   [ ][Y+] 1-12-1. Remove TypeScript runtime package exports and entrypoints
    │   [ ][Y+] 1-12-2. Add TypeScript runtime residue guard with frontend allowlist
    │   [ ][Y+] 1-12-3. Verify final Rust workspace and retained TypeScript surfaces
        [ ][Y+] 1-12-4. Update docs examples and release baseline for Rust first ZBrain

<!-- ROADMAP_SECTION_END -->
