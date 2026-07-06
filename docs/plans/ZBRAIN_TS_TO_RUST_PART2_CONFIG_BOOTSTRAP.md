<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part2-config-bootstrap.json` | 最后更新: 2026-07-06 12:04:42

[~][Y+] 1. ZBrain TS->Rust Part2: Config/Bootstrap/Package Entrypoint strict parity
├── [x][Y+] 1-1. init command strict TS flag parity
│   ├── [x][Y+] 1-1-1. Write init strict parity audit and test matrix
│   ├── [x][Y+] 1-1-2. Add init parser parity tests
│   ├── [x][Y+] 1-1-3. Implement engine selection flags for init
│   ├── [x][Y+] 1-1-4. Implement migrate-only init behavior
│   ├── [x][Y+] 1-1-5. Implement thin-client MCP-only init behavior
│   ├── [x][Y+] 1-1-6. Implement init embedding setup flags
│   └── [x][Y+] 1-1-7. Validate init existing-config and JSON output behavior
├── [ ][Y+] 1-2. config command strict TS flag parity
├── [ ][Y+] 1-3. doctor command strict TS flag parity
├── [ ][Y+] 1-4. schema command strict TS flag parity
├── [ ][Y+] 1-5. package/bin entrypoint strict TS flag parity
├── [ ][Y+] 1-6. migration cleanup for TS remnants and documentation links
└── [ ][Y+] 1-7. final validation for Part2 config/bootstrap migration

### 当前施工：1. ZBrain TS->Rust Part2: Config/Bootstrap/Package Entrypoint strict parity

Part2 承接 Phase 2 Config/Bootstrap/Package Entrypoint strict TS flag parity；包含 init/config/doctor/schema/package-bin 实施流，以及 migration cleanup 与 final validation。其他未完成 TS->Rust 工作暂归 Part3。

**决策：**
- Q: Part2 roadmap 初始结构怎么建？ → 按实施流建结构：Phase 2 strict parity -> init/config/doctor/schema/package-bin -> migration cleanup/final validation (从旧 roadmap 拆分迁移而来；Part2 只承接 Config/Bootstrap/Package Entrypoint strict TS flag parity 与迁移收尾，其他未完成 TS->Rust 工作暂归 Part3。)
- Q: Part1/Part2 的切分边界按哪个粒度？ → 按完成状态切：已 completed 的全部进入 Part1；未 completed 的 Phase 2 进入 Part2 (从旧 roadmap 迁移的核心决策；Part1 作为 completed archive，Part2 承接 Config/Bootstrap/Package Entrypoint strict parity。)
- Q: Part2 新 roadmap 的范围怎么定义？ → Phase 2 + 迁移收尾工作；所有其他未完成的 TS->Rust 工作暂时归入 Part3 (Part2 包含 Part1 清理后发现的 TS cleanup、文档断链、最终验证；不承接其他未来未完成 TS->Rust 工作。)

**当前子树：**
├── [x][Y+] 1-1. init command strict TS flag parity
│   ... 7 more child nodes; run tree 1-1 --depth 2 for full view
├── [ ][Y+] 1-2. config command strict TS flag parity
├── [ ][Y+] 1-3. doctor command strict TS flag parity
├── [ ][Y+] 1-4. schema command strict TS flag parity
├── [ ][Y+] 1-5. package/bin entrypoint strict TS flag parity
├── [ ][Y+] 1-6. migration cleanup for TS remnants and documentation links
└── [ ][Y+] 1-7. final validation for Part2 config/bootstrap migration
<!-- ROADMAP_SECTION_END -->
