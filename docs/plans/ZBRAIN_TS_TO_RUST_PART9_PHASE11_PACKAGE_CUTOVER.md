<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part9-phase11-package-cutover.json` | 最后更新: 2026-07-15 10:41:13

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
- Q: 第三轮删哪些? 混合测试如何认定随删? → 第三轮=graph-query+capture+agent(含agent-logs)三子系统,共4命令文件+5测试全删,tsc零新增(75→75)+cargo绿+三命令Rust存活 (第三轮落地(2026-07-15): 删 graph-query.ts+capture.ts+agent.ts+agent-logs.ts(agent-logs仅被agent.ts+agent-cli.test引用,随agent删)+5测试。关键认定(承接autopilot e2e同逻辑): 5测试虽import保留impl(PGLiteEngine/MinionQueue/computeContentHash/protected-names)但那些只搭fixture台子,真正被测对象(runGraphQuery/runCapture/agent)随命令删除消失,故整测随删不算'改写混合测试';保留impl都是KEEP核心有大量其它importer不dangle。测试清单: graph-query.test/capture-build-content/capture-runcapture/commands/capture/agent-cli 全删。cli.ts改动=删3处dispatch(graph-query/agent/capture)+capture特有的--help pre-engine早退分支(L1112)+CLI_ONLY_SELF_HELP的'capture'条目+CLI_ONLY移除三名+3处help文本(graph-query 2行/capture section头改名+3行)。全gate绿: tsc 75→75零新增; ts_dep_closure四文件importer全空零dangle; eval questions.json无残留expected_files; cargo build 0.50s绿; zbrain.exe三命令--help Rust侧存活。净删2477行增2行。)
- Q: 第四轮 jobs 删除? 混合测试如何区分整删/改写? cli-path 回收? → 第四轮=jobs子系统(jobs.ts 1575行)+回收上轮cli-path.ts。3测试整删+4测试改写(精准摘jobs块保留其余)。tsc零新增(75→75)+cargo绿+jobs Rust存活。净删2564行 (第四轮落地(2026-07-15): jobs.ts已迁Rust(crates有jobs命令+register_builtin_handlers@registry.rs)。**cli-path.ts回收**: 上轮为切jobs→autopilot边而提取的中性文件,jobs删后它只剩自身测试引用,存在理由消失→连test一起删(Rust有自己CLI path解析)。**混合测试两类精准区分(本轮核心方法论)**: (A)被测对象随jobs删消失→整删: autopilot-cycle-handler/handlers(测registerBuiltinHandlers)+minions-shell-pglite(e2e测经registerBuiltinHandlers编排的shell路径;shellHandler另有minions-shell.test独立覆盖不丢); (B)主体测保留代码仅局部触jobs→改写摘除jobs块: minions.test(摘2个describe块resolveWorkerConcurrency/parseMaxWaitingFlag,保留backpressure/db等)、cycle-abort(摘1个test读jobs源码,保留worker/cycle两test)、exit-classification(SITES摘jobs站点+删jobs专项it,保留classifyWorkerExit+doctor/supervisor站点)、doctor.test(摘jobs supervisor status源码test,保留doctor test)。**运行时源码读取陷阱(dep_closure抓不到)**: 4测试用Bun.file/readFileSync读jobs.ts源码做字符串断言(非import),删文件会运行时崩,必须一并摘除。cli.ts删jobs dispatch+CLI_ONLY+整个JOBS help section(保留L1599 warning因jobs Rust侧还在)。清理questions.json 3处expected_files+4处保留代码注释(supervisor/shell-validate/shell/worker,改'ported to Rust')。全gate绿: tsc75→75零新增; importers(jobs)+importers(cli-path)双空; cargo 0.44s绿; zbrain jobs --help Rust存活。)
- Q: 第五轮: 如何删 sources 子系统(9混合测试)? → git rm sources.ts(命令层)+3整删测试(sources/sources-set-cr-mode/repos-alias,被测对象=runSources本身)。改写6个B类: 4个fixture-only(drift/sync-sole/tx/pfs)把runSources(['add'..])换addSource(engine,{id,localPath?,federated?}); integration+e2e删federate/unfederate/rename测试块(命令层独有无sources-ops等价)+add/remove换addSource/removeSource。cli.ts删3处dispatch+CLI_ONLY移除sources&repos+THIN_CLIENT set&hint+timeout special-case+help。清理3处注释(改'ported to Rust')+leak白名单。sources-ops.ts是保留底层API(CLI+MCP共用)。gate: tsc75->75/零dangle/cargo绿/zbrain sources --help存活 (第五轮删除,净删约?行)
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
