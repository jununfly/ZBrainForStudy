<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part2-config-bootstrap.json` | 最后更新: 2026-07-06 18:15:52

[x][Y+] 1. ZBrain TS->Rust Part2: Config/Bootstrap/Package Entrypoint strict parity
├── [x][Y+] 1-1. init command strict TS flag parity
│   ├── [x][Y+] 1-1-1. Write init strict parity audit and test matrix
│   ├── [x][Y+] 1-1-2. Add init parser parity tests
│   ├── [x][Y+] 1-1-3. Implement engine selection flags for init
│   ├── [x][Y+] 1-1-4. Implement migrate-only init behavior
│   ├── [x][Y+] 1-1-5. Implement thin-client MCP-only init behavior
│   ├── [x][Y+] 1-1-6. Implement init embedding setup flags
│   └── [x][Y+] 1-1-7. Validate init existing-config and JSON output behavior
├── [x][Y+] 1-2. config command strict TS flag parity
│   ├── [x][Y+] 1-2-1. Write config strict parity audit and test matrix
│   ├── [x][Y+] 1-2-2. Enforce config set schema validation and unknown-key gating
│   └── [x][Y+] 1-2-3. Align config get not-found exit code and redaction semantics
├── [x][Y+] 1-3. doctor command strict TS flag parity
├── [x][Y+] 1-4. rename schema DDL dumper to schema-sql and trace unmigrated schema-pack
├── [x][Y+] 1-5. bin wrapper transparent pass-through correctness (argv + exit-code/signal)
├── [x][Y+] 1-6. migration cleanup for TS remnants and documentation links
├── [x][Y+] 1-7. Part2 slice deliverable validation (1-1~1-6 parity + wrapper passthrough + cleanup no-regression; excludes part3 subsystems)
└── [x][Y+] 1-8. global flag gap audit & subsystem hand-off (progress reporter / MCP timeout / search attribution)
<!-- ROADMAP_SECTION_END -->
