<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part2-config-bootstrap.json` | 最后更新: 2026-07-03 00:16:08

[~][Y+] 1. ZBrain TS->Rust Part2: Config/Bootstrap/Package Entrypoint strict parity
├── [~][Y+] 1-1. init command strict TS flag parity
│   ├── [x][Y+] 1-1-1. Write init strict parity audit and test matrix
│   ├── [x][Y+] 1-1-2. Add init parser parity tests
│   ├── [ ][Y+] 1-1-3. Implement engine selection flags for init
│   ├── [ ][Y+] 1-1-4. Implement migrate-only init behavior
│   ├── [ ][Y+] 1-1-5. Implement thin-client MCP-only init behavior
│   ├── [ ][Y+] 1-1-6. Implement init embedding setup flags
│   └── [ ][Y+] 1-1-7. Validate init existing-config and JSON output behavior
├── [ ][Y+] 1-2. config command strict TS flag parity
├── [ ][Y+] 1-3. doctor command strict TS flag parity
├── [ ][Y+] 1-4. schema command strict TS flag parity
├── [ ][Y+] 1-5. package/bin entrypoint strict TS flag parity
├── [ ][Y+] 1-6. migration cleanup for TS remnants and documentation links
└── [ ][Y+] 1-7. final validation for Part2 config/bootstrap migration

### 当前施工：1-1. init command strict TS flag parity

**决策：**
- Q: Q1: init strict TS flag parity 下一刀粒度 → A: 先做 audit + 测试清单，不改生产逻辑 (先把 TS/Rust init flag、输出、配置写入、副作用差异整理成 Part2 子节点/issues，避免一刀过大。)

**当前子树：**
├── [x][Y+] 1-1-1. Write init strict parity audit and test matrix
├── [x][Y+] 1-1-2. Add init parser parity tests
├── [ ][Y+] 1-1-3. Implement engine selection flags for init
├── [ ][Y+] 1-1-4. Implement migrate-only init behavior
├── [ ][Y+] 1-1-5. Implement thin-client MCP-only init behavior
├── [ ][Y+] 1-1-6. Implement init embedding setup flags
└── [ ][Y+] 1-1-7. Validate init existing-config and JSON output behavior
<!-- ROADMAP_SECTION_END -->
