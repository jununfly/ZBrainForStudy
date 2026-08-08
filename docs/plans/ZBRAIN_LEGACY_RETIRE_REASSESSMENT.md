<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-legacy-retire-reassessment.json` | 最后更新: 2026-08-08 20:39:53

[~][X+] 1. Legacy GBrain 资产退役重评估
├── [ ][X+] 1-1. 范围盘点与分类（132 文件 by 目录）
├── [ ][X+] 1-2. 外部 ripple 影响评估（6 文件 ~50 引用）
├── [~][X+] 1-3. 可验证性重估（WSL+Rust cargo 已落地）
├── [ ][X+] 1-4. 决策选项 A/B/C/D 对比
└── [ ][X+] 1-5. 推荐路径与执行

### 当前施工：1-3. 可验证性重估（WSL+Rust cargo 已落地）

**决策：**
- Q: 本机能否本地 verify ripple 修复？ → 能。WSL2 Ubuntu + Rust 1.97.1 + cargo 已于 2026-08-08 落地，cargo build/test 全绿。 (原 pending doc 主 blocker（本机无 cargo 无法 verify）已解除；TS 侧 ripple（package.json/.github/bunfig/admin）仍依赖 bun/node，WSL 也可跑 node 部分验证，但 CI 全链路需 push 后 GitHub Actions 确认。)
<!-- ROADMAP_SECTION_END -->
