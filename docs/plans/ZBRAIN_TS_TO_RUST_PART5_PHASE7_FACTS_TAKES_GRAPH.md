<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part5-phase7-facts-takes-graph.json` | 最后更新: 2026-07-13 17:21:52

[x][Y+] 1. Phase 7 — Facts, Takes, Timeline, Salience, Graph
├── [x][Y+] 1-1. Phase 7A: Takes 引擎层 (DB schema + trait + fence parser + salience wiring + scorecard)
│   ├── [x][Y+] 1-1-1. DB migration: extend takes table to full TS schema
│   ├── [x][Y+] 1-1-2. Rust types: Take struct, TakeRow, TakeKind, fence types
│   ├── [x][Y+] 1-1-3. Fence parser/renderer: parse_takes_fence / render_takes_fence
│   ├── [x][Y+] 1-1-4. BrainEngine trait: add get_takes_for_page, add_takes_batch, resolve_take
│   ├── [x][Y+] 1-1-5. Backend impls: libsql + postgres + InMemory takes CRUD + scorecard
│   ├── [x][Y+] 1-1-6. Wire salience: update get_salience_scores to use real takes_count
│   └── [x][Y+] 1-1-7. Tests: fence round-trip, CRUD integration, scorecard, salience with takes
├── [x][Y+] 1-2. Phase 7B: Backlinks + Facts 引擎层
│   ├── [x][Y+] 1-2-1. 1-2-1: Rust types (Link, LinkBatchInput, GraphNode, GraphPath)
│   ├── [x][Y+] 1-2-2. 1-2-2: BrainEngine trait link methods (add_links_batch, remove_link, get_links, get_backlinks, get_backlink_counts, traverse_paths)
│   ├── [x][Y+] 1-2-3. 1-2-3: Backend implementations (InMemory + libsql + postgres links CRUD)
│   ├── [x][Y+] 1-2-4. 1-2-4: Links integration tests
│   ├── [x][Y+] 1-2-5. Facts DB migration: new facts table (~20+ columns)
│   ├── [x][Y+] 1-2-6. Rust types: FactRow, NewFact, FactKind, FactVisibility, ParsedFact, FactsHealth
│   ├── [x][Y+] 1-2-7. Facts fence parser/renderer: parse_facts_fence / render_facts_fence
│   ├── [x][Y+] 1-2-8. BrainEngine trait facts methods: insertFact(supersede), listFactsByEntity, getFactsHealth, expireFact
│   ├── [x][Y+] 1-2-9. Backend implementations: libsql + postgres facts CRUD
│   └── [x][Y+] 1-2-10. Facts integration tests: fence round-trip, CRUD, supersede, health
└── [x][Y+] 1-3. Phase 7C: Graph + Salience 收尾 + CLI 接线
    ├── [x][Y+] 1-3-1. 1-3-1: Graph traverse_paths 三后端实现
    ├── [x][Y+] 1-3-2. 1-3-2: Salience 方法（get_recent_salience + touch_salience trait + 三后端）
    ├── [x][Y+] 1-3-3. 1-3-3: CLI 接线 — facts/links/takes 命令（增删改查 + fence 交互）
    ├── [x][Y+] 1-3-4. 1-3-4: CLI 查询命令 — salience/orphans/backlinks/graph-query
    └── [x][Y+] 1-3-5. 1-3-5: Postgres 集成测试补全（links/facts/takes PG mirror）
<!-- ROADMAP_SECTION_END -->
