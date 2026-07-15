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

<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part9 Phase11 — package 入口 cutover + src/core 删除

### 树形视图 (depth=2)

```
[~][X+] 1. Part9 Phase11 — package 入口 cutover + src/core 删除
├── [x][Y+] 1-1. package.json cutover(改 main/exports/scripts + 清 exports CI guard + 清悬空引用)
├── [x][Y+] 1-2. 重算 KEEP 种子 + 依赖闭包 D(cutover 后新种子集)
├── [~][X+] 1-3. 分批删 src/core 已迁 impl(tsc 基线 diff 验证,逐 slice)
└── [ ][X+] 1-4. final cutover 收尾(typecheck 脚本处置 + CHANGELOG + 残留 TS 决策)
```

### 🔨 当前施工: 1-3. 分批删 src/core 已迁 impl(tsc 基线 diff 验证,逐 slice)
**Status:** `in_progress` | **Mode:** `explore`

**决策记录:**
- Q: 1-3 删除来源/范围/验证 gate?
  A: 来源=Roadmap已完成节点反推(系统);本轮范围=最有把握1-2子系统(小步);gate=tsc基线diff+闭包dangle校验双保险
  > 用户拍板(2026-07-15): 候选来源从 Part1-7 已 completed 的 Rust 迁移节点反推对应 TS 子系统;本轮不铺开,先删 1-2 个 Rust 覆盖最硬且导入图叶子安全的子系统跑通完整流程;每个删除 slice 删前记 tsc 基线(76 errors)删后确保不新增,并用 ts_dep_closure 反向闭包验证候选不 dangle 任何保留 impl/kept-test。方法论修正: 删除按'导入图叶子'驱动而非子系统名——src/core 内部仍是互 import 网,cutover 只切外部入口,须从叶子往里逐层剥。
- Q: 1-3 执行形态?
  A: 选2: 先删无混合测试牵绊的子集试水
  > 用户拍板(2026-07-15): 全局量化=KEEP闭包外160 impl 整组删零dangle,216测试碰它(83纯删+133混合)。选形态2: 本轮先从160里挑一个'关联测试全是纯删(无混合测试牵扯保留代码)'的子系统删一轮,跑通 tsc基线diff(76)+ts_dep_closure dangle校验完整gate,验证无误再扩全集+混合测试。最贴合小步最有把握原则。
- Q: 首刀删哪些? 验证结果?
  A: 首刀=think/remote/salience 三已迁移死命令,tsc零新增(76→75)+cargo绿+三命令Rust存活
  > 形态2首刀落地(2026-07-15): 删 src/commands/{think,remote,salience}.ts + cli.ts 三处动态import dispatch分支(remote/salience/think)+CLI_ONLY Set移除三名。选址三满足: Rust已覆盖(zbrain --help 有三命令)+导入图安全(各仅被cli.ts动态import,零其它生产牵绊)+零测试债(零关联测试)。验证: tsc 76→75(零新增,反而清掉remote.ts原有的dangling doctor.ts import基线错误);零测试dangle;cargo build -p zbrain-cli 0.45s绿;think/remote/salience 在Rust CLI侧完好存活=删TS无损。autopilot链因被保留代码jobs.ts import不够干净,本轮跳过。其余6个已迁移命令(agent/capture/graph-query/jobs/sources+autopilot)有混合测试牵绊,留后续轮。
<!-- ⚠️ ROADMAP_SECTION_END -->
