<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part9-phase11-package-cutover.json` | 最后更新: 2026-07-15 09:52:27

[~][X+] 1. Part9 Phase11 — package 入口 cutover + src/core 删除
├── [x][Y+] 1-1. package.json cutover(改 main/exports/scripts + 清 exports CI guard + 清悬空引用)
├── [x][Y+] 1-2. 重算 KEEP 种子 + 依赖闭包 D(cutover 后新种子集)
├── [~][X+] 1-3. 分批删 src/core 已迁 impl(tsc 基线 diff 验证,逐 slice)
└── [ ][X+] 1-4. final cutover 收尾(typecheck 脚本处置 + CHANGELOG + 残留 TS 决策)

### 当前施工：1-3. 分批删 src/core 已迁 impl(tsc 基线 diff 验证,逐 slice)

**决策：**
- Q: 1-3 删除来源/范围/验证 gate? → 来源=Roadmap已完成节点反推(系统);本轮范围=最有把握1-2子系统(小步);gate=tsc基线diff+闭包dangle校验双保险 (用户拍板(2026-07-15): 候选来源从 Part1-7 已 completed 的 Rust 迁移节点反推对应 TS 子系统;本轮不铺开,先删 1-2 个 Rust 覆盖最硬且导入图叶子安全的子系统跑通完整流程;每个删除 slice 删前记 tsc 基线(76 errors)删后确保不新增,并用 ts_dep_closure 反向闭包验证候选不 dangle 任何保留 impl/kept-test。方法论修正: 删除按'导入图叶子'驱动而非子系统名——src/core 内部仍是互 import 网,cutover 只切外部入口,须从叶子往里逐层剥。)
- Q: 1-3 执行形态? → 选2: 先删无混合测试牵绊的子集试水 (用户拍板(2026-07-15): 全局量化=KEEP闭包外160 impl 整组删零dangle,216测试碰它(83纯删+133混合)。选形态2: 本轮先从160里挑一个'关联测试全是纯删(无混合测试牵扯保留代码)'的子系统删一轮,跑通 tsc基线diff(76)+ts_dep_closure dangle校验完整gate,验证无误再扩全集+混合测试。最贴合小步最有把握原则。)
- Q: 首刀删哪些? 验证结果? → 首刀=think/remote/salience 三已迁移死命令,tsc零新增(76→75)+cargo绿+三命令Rust存活 (形态2首刀落地(2026-07-15): 删 src/commands/{think,remote,salience}.ts + cli.ts 三处动态import dispatch分支(remote/salience/think)+CLI_ONLY Set移除三名。选址三满足: Rust已覆盖(zbrain --help 有三命令)+导入图安全(各仅被cli.ts动态import,零其它生产牵绊)+零测试债(零关联测试)。验证: tsc 76→75(零新增,反而清掉remote.ts原有的dangling doctor.ts import基线错误);零测试dangle;cargo build -p zbrain-cli 0.45s绿;think/remote/salience 在Rust CLI侧完好存活=删TS无损。autopilot链因被保留代码jobs.ts import不够干净,本轮跳过。其余6个已迁移命令(agent/capture/graph-query/jobs/sources+autopilot)有混合测试牵绊,留后续轮。)
- Q: 第二轮删哪些? 如何切断保留代码依赖边? → 第二轮=autopilot子系统(autopilot.ts 1143行+autopilot-fanout.ts+4测试),先提取resolveGbrainCliPath到中性cli-path.ts切断jobs.ts依赖边再删,tsc零新增(75→75)+cargo绿+autopilot Rust存活 (第二轮落地(2026-07-15): 用户选①先切边再删。障碍=保留代码jobs.ts L996动态import autopilot.ts的resolveGbrainCliPath()。手法: (1)提取resolveGbrainCliPath(19行,仅依赖execSync)到新中性文件src/commands/cli-path.ts(保留); (2)jobs.ts L996改引./cli-path.ts; (3)autopilot-resolve-cli.test.ts→cli-path.test.ts重命名+改引(保留,测的是保留函数); 边切断后autopilot.ts importer仅剩cli.ts(死入口)+2纯删测试。删除清单: autopilot.ts+autopilot-fanout.ts(仅被autopilot.ts+2测试import,随删)+4测试(autopilot-install/reconnect-classifier纯删; autopilot-fanout纯删; e2e fanout-postgres虽import PostgresEngine/MinionQueue但100%测fanout专属逻辑,被测对象随fanout消失故整删,PostgresEngine/MinionQueue是KEEP核心有大量其它importer不dangle)。cli.ts删autopilot dispatch分支+CLI_ONLY移除+2处help文本。清理5处悬空文字引用(child-worker-supervisor.ts/.test.ts注释,supervisor.ts注释,questions.json两处expected_files,cli-path.ts自身注释)——均改自解释不指向已删路径。验证全gate: tsc 75→75零新增; ts_dep_closure importers(autopilot.ts)=空+importers(fanout)=空零dangle; cargo build 0.79s绿; zbrain.exe autopilot --help Rust侧完整存活(--install/uninstall/status/once)。净删2151行增10行。)
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
