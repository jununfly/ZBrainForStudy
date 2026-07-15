<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part9-phase11-package-cutover.json` | 最后更新: 2026-07-15 08:49:52

[~][X+] 1. Part9 Phase11 — package 入口 cutover + src/core 删除
├── [ ][Y+] 1-1. package.json cutover(改 main/exports/scripts + 清 exports CI guard + 清悬空引用)
├── [ ][Y+] 1-2. 重算 KEEP 种子 + 依赖闭包 D(cutover 后新种子集)
├── [ ][X+] 1-3. 分批删 src/core 已迁 impl(tsc 基线 diff 验证,逐 slice)
└── [ ][X+] 1-4. final cutover 收尾(typecheck 脚本处置 + CHANGELOG + 残留 TS 决策)

### 当前施工：1. Part9 Phase11 — package 入口 cutover + src/core 删除

**决策：**
- Q: cutover 目标形态? → A — 彻底移除 JS 库身份,zbrain 只做 Rust CLI 分发包 (前提已核实零真实消费者(子代理 agent-78d088de):admin(name=gbrain-admin,不依赖 zbrain)/src/tests 均无实 import(仅 2 处 JSDoc 注释);未发布 npm(无 private/publishConfig/files,发布走 clawhub);openclaw.extensions 指向的 src/openclaw-context-engine.ts 已在 e643d86 删除=悬空;实际 openclaw.plugin.json 走 bin/zbrain-rs.js serve-mcp 不碰 TS。JS 库入口是迁移残留非产品需求,彻底移除是终局。)
- Q: 删除范围边界/施工节奏? → 分两刀:先 cutover(改配置零删除),再依赖闭包重算 D 分批删 src (cutover 与大规模删除风险性质不同(改配置 vs 删代码),分刀各自验证易回滚,符合 PRD 逐 slice 纪律。第一刀只改 package.json+清 exports guard+清悬空引用,不删任何 src。之后以缩减后的 KEEP 种子(仅 KNOWN-GAPS spec + Phase10 判 K 的 eval + doctor 子系统)重算 D,分批 git rm + tsc 基线 diff 验证(复用 e643d86 手法)。)

**当前子树：**
├── [ ][Y+] 1-1. package.json cutover(改 main/exports/scripts + 清 exports CI guard + 清悬空引用)
├── [ ][Y+] 1-2. 重算 KEEP 种子 + 依赖闭包 D(cutover 后新种子集)
├── [ ][X+] 1-3. 分批删 src/core 已迁 impl(tsc 基线 diff 验证,逐 slice)
└── [ ][X+] 1-4. final cutover 收尾(typecheck 脚本处置 + CHANGELOG + 残留 TS 决策)
<!-- ROADMAP_SECTION_END -->
