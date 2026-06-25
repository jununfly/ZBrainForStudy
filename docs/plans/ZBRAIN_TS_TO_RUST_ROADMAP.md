<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-roadmap.json` | 最后更新: 2026-06-25 11:31:44

[~][X+] 1. ZBrain TS to Rust Migration
├── [x][Y+] 1-1. Roadmap and TypeScript runtime inventory
│   ├── [x][Y+] 1-1-1. Restore canonical roadmap files
│   ├── [x][Y+] 1-1-2. Expand complete TS to Rust PRD from codebase facts
│   ├── [x][Y+] 1-1-3. Classify TypeScript runtime and frontend retention surfaces
│   └── [x][Y+] 1-1-4. Define per-slice deletion checklist and verification gates
├── [ ][Y+] 1-2. Core storage parity closure
│   ├── [x][Y+] 1-2-1. Finish Page contract parity across storage backends
│   ├── [~][Y+] 1-2-2. Port missing advanced Page writes to Rust
│   ├── [ ][Y+] 1-2-3. Move schema migrations ownership to Rust
│   ├── [x][X+] 1-2-4. Decide internal DB legacy identifier migration
│   └── [x][Y+] 1-2-5. Implement DB legacy identifier rename migration
├── [ ][Y+] 1-3. Config bootstrap and package entrypoint cutover
│   ├── [ ][Y+] 1-3-1. Port config discovery loading and writing to Rust
│   ├── [ ][Y+] 1-3-2. Port init doctor config storage and schema commands
│   ├── [ ][Y+] 1-3-3. Cut package bin and install flow to Rust binary
│   └── [ ][Y+] 1-3-4. Delete replaced TypeScript bootstrap command surface
├── [ ][Y+] 1-4. Operations layer and trust boundary migration
│   ├── [ ][Y+] 1-4-1. Port operation definitions schemas and context
│   ├── [ ][Y+] 1-4-2. Port local and remote trust boundary enforcement
│   └── [ ][Y+] 1-4-3. Move shared CLI MCP dispatch to Rust operations
├── [ ][Y+] 1-5. MCP server migration
│   ├── [ ][Y+] 1-5-1. Implement Rust MCP tool definitions and parameter validation
│   ├── [ ][Y+] 1-5-2. Implement Rust MCP transports rate limiting and audit hooks
│   └── [ ][Y+] 1-5-3. Delete TypeScript MCP implementation after parity
├── [ ][Y+] 1-6. Web backend and admin API migration
│   ├── [ ][Y+] 1-6-1. Implement Axum admin backend API
│   ├── [ ][Y+] 1-6-2. Port auth session token request log jobs calibration and agents endpoints
│   └── [ ][X+] 1-6-3. Retain React TypeScript admin frontend by explicit decision
├── [ ][Y+] 1-7. Ingestion sources search and retrieval migration
│   ├── [ ][Y+] 1-7-1. Port source management import capture extract and sync flows
│   ├── [ ][Y+] 1-7-2. Port embeddings chunking hybrid search and reindex flows
│   └── [ ][Y+] 1-7-3. Delete replaced TypeScript ingestion search and source modules
├── [ ][Y+] 1-8. Facts takes timeline salience and graph migration
│   ├── [ ][Y+] 1-8-1. Port facts takes timeline salience backlinks orphans and graph behavior
│   └── [ ][Y+] 1-8-2. Delete replaced TypeScript knowledge graph modules
├── [ ][Y+] 1-9. AI gateway providers models and routing migration
│   ├── [ ][Y+] 1-9-1. Port provider config model capabilities pricing and routed gateway
│   └── [ ][Y+] 1-9-2. Preserve routed gateway and no direct provider guardrails
├── [ ][Y+] 1-10. Jobs agents minions autopilot and remote execution migration
│   ├── [ ][Y+] 1-10-1. Port jobs lifecycle agent logs minions autopilot fanout and remote execution
│   └── [ ][Y+] 1-10-2. Preserve privacy PII and remote execution trust guardrails
├── [ ][Y+] 1-11. Evals benchmarks and developer tooling migration
│   ├── [ ][X+] 1-11-1. Decide product critical evals and benchmarks
│   └── [ ][Y+] 1-11-2. Port or archive TypeScript eval and developer tooling
└── [ ][Y+] 1-12. Final cutover and TypeScript runtime cleanup
    ├── [ ][Y+] 1-12-1. Remove TypeScript runtime package exports and entrypoints
    ├── [ ][Y+] 1-12-2. Add TypeScript runtime residue guard with frontend allowlist
    ├── [ ][Y+] 1-12-3. Verify final Rust workspace and retained TypeScript surfaces
    └── [ ][Y+] 1-12-4. Update docs examples and release baseline for Rust first ZBrain
<!-- ROADMAP_SECTION_END -->

### 当前施工：1-2-2-2. Add Rust advanced Page writes contract surface

GitHub issue #19: Add Rust advanced Page writes contract surface. Compile-only slice: RawData/PageVersion types + 7 trait method signatures + 3 backend unimplemented!() stubs + object-safety test.

**决策：**
- Q: Does 1-2-2-2 add only compile-only contract surface? → Yes. Add only RawData/PageVersion types and 7 trait method signatures; no backend behavior implementation yet. (Follows the same slice pattern as file-storage parity: contract compile slice first, then InMemory, then libsql, then Postgres. Object safety and focused tests run in this slice; actual behavior ships in follow-up slices.)
- Q: What Rust type for timestamp fields? → String, String. Align with existing FileRow.created_at pattern. Keep as_string, no time/chrono date-time upgrade deferred to later unified serialization pass.
- Q: How to represent sourceId options? → Keep Option<&str> for all sourceId parameters. No separate options structs (RawDataOpts, PageVersionOpts, UpdateSlugOpts) in this parity slice. (Consistent with existing get_file/upsert_file style. Upgrade to structs only when field count grows to 3+.)
- Q: How to stub backend implementations? → Stub all 7 methods with unimplemented!() in InMemory, libsql, and Postgres backends. (This slice is compile-only. Actual behavior moves to 1-2-2-3 (InMemory), 1-2-2-4 (libsql), and 1-2-2-5 (Postgres). No-op rewriteLinks stays unimplemented until that behavior slice.)

### 当前施工：1-2-2-2. Add Rust advanced Page writes contract surface

Next implementation slice after audit: add Rust RawData/PageVersion public types and BrainEngine trait methods for raw data, page versions, and slug/link rewrite. Start with compile/object-safety tests before backend behavior.
