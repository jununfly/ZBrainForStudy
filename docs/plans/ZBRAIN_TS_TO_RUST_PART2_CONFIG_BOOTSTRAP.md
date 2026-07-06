<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part2-config-bootstrap.json` | 最后更新: 2026-07-06 13:28:55

[~][Y+] 1. ZBrain TS->Rust Part2: Config/Bootstrap/Package Entrypoint strict parity
├── [x][Y+] 1-1. init command strict TS flag parity
│   ├── [x][Y+] 1-1-1. Write init strict parity audit and test matrix
│   ├── [x][Y+] 1-1-2. Add init parser parity tests
│   ├── [x][Y+] 1-1-3. Implement engine selection flags for init
│   ├── [x][Y+] 1-1-4. Implement migrate-only init behavior
│   ├── [x][Y+] 1-1-5. Implement thin-client MCP-only init behavior
│   ├── [x][Y+] 1-1-6. Implement init embedding setup flags
│   └── [x][Y+] 1-1-7. Validate init existing-config and JSON output behavior
├── [~][Y+] 1-2. config command strict TS flag parity
│   ├── [x][Y+] 1-2-1. Write config strict parity audit and test matrix
│   ├── [x][Y+] 1-2-2. Enforce config set schema validation and unknown-key gating
│   └── [ ][Y+] 1-2-3. Align config get not-found exit code and redaction semantics
├── [ ][Y+] 1-3. doctor command strict TS flag parity
├── [ ][Y+] 1-4. schema command strict TS flag parity
├── [ ][Y+] 1-5. package/bin entrypoint strict TS flag parity
├── [ ][Y+] 1-6. migration cleanup for TS remnants and documentation links
└── [ ][Y+] 1-7. final validation for Part2 config/bootstrap migration

### 当前施工：1-2. config command strict TS flag parity

**决策：**
- Q: Q1: 1-2 总体策略 audit-first vs 直接开干 → A: 复用 1-1 模式，先建 1-2-1 audit + test matrix 再拆实现子节点 (1-1 audit-first 已被证明有效(minimizes rework)；子 agent 调查发现散在对话需落盘为 canonical audit；三个 parity gap 各自取舍(尤其存储平面 file vs DB)需单独 grill。)
- Q: Q2: parity 基线 - 是否复刻 TS 双存储平面(show读文件/getsetunset走DB) → A: 接受 Rust 单一文件平面为故意偏离, 不复刻 DB 平面, audit 显式记录 deviation+理由 (TS 双平面是历史包袱(show读文件、set写DB)；复刻会主动引入复杂度+engine依赖；迁移原则允许 explicitly document deliberate deviation；config 命令应能在无 DB 时工作。本刀聚焦文件平面上的行为 parity(校验/退出码/redaction)。)
- Q: Q3: config set 未知键门控是否复刻 KNOWN_CONFIG_KEYS+Levenshtein+--force → A: 复刻门控但用 Config schema round-trip 当白名单, 未知/拼错键拒绝+exit1, 保留 --force 逃生口, Levenshtein 列为可选 (Rust 强类型 Config struct 本身即权威白名单，无需重复维护 TS 弱类型时代的手写 KNOWN_CONFIG_KEYS 常量；现状裸 dot-path 写入会静默创建野字段(真实 bug)必须堵；--force 逃生口与 TS 语义一致。)
- Q: Q4: config get not-found 退出码 + get 是否 redaction → A: 退出码对齐 TS(not-found 非0); redaction 也对齐 TS(get 不脱敏, show 仍脱敏) (not-found 是真实失败, 脚本靠退出码判断, Rust 现状 exit0 是 bug；get<key> 是显式取单值(常用于脚本读回 secret), 脱敏破坏该用途——TS 故意不脱敏是对的, 仅 show(列全部)脱敏防 scrollback 泄漏。)
- Q: Q5: 1-2 拆成哪些实现子节点 → A: audit + 2 实现切片(共3节点), embedding DB-plane 校验在 audit 记为 TS-only 不迁移 (1-2-1 audit+test matrix(含 Q2 deviation)；1-2-2 config set schema round-trip 白名单+--force(Q3)；1-2-3 get not-found 非0退出+去脱敏(Q4)。embedding_model/dimensions 硬拒绝、覆盖率门槛依赖 DB 平面语义, 文件平面下无对应, 属 Q2 偏离自然结果, audit 记为 TS-only 不拆实现节点。)

**当前子树：**
├── [x][Y+] 1-2-1. Write config strict parity audit and test matrix
├── [x][Y+] 1-2-2. Enforce config set schema validation and unknown-key gating
└── [ ][Y+] 1-2-3. Align config get not-found exit code and redaction semantics
<!-- ROADMAP_SECTION_END -->
