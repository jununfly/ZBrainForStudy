<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-15 02:45:03


<!-- ROADMAP_TREE_START -->
<!-- 由 zj-roadmap-driven 自动生成，请勿手动编辑 -->
[~][X+] 1. ZBrain G74/G76 eval+extract 命令 Rust 重实现
├── [~][X+] 1-1. G74 eval 族命令 Rust 重实现（19 命令）
│   ├── [x] 1-1-1. 修正 KNOWN-GAPS/COMMANDS_TEAR_DOWN 的 G74 失准描述
│   ├── [x] 1-1-2. 第一刀：zbrain eval 核心 verb（暴露已 port 的 run_eval）
│   ├── [x][X+] 1-1-3. 判废真空壳 + 重分类 2 非空壳 eval 命令（markdown-greenfield 判废；extract-atoms/synthesize-concepts 底层 cycle phase 已在 Rust）
│   ├── [x] 1-1-4. 非 LLM 但需新基建的 9 个 eval 命令（eval-brainstorm 复核为 LLM 已移 D 类）
│   └── [x][X+] 1-1-5. 真 LLM 的 5 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions / brainstorm）
│       ├── [x] 1-1-5-1. eval-cross-modal（#59 全保真 port，core+CLI+e2e 全绿）
│       ├── [x][X+] 1-1-5-2. eval-longmemeval（#60，替换 G58 占位）
│       ├── [x][X+] 1-1-5-3. eval-takes-quality（#61，新建 EvalTakesQuality variant）
│       │   ├── [x][X+] 1-1-5-3-1. eval-takes-quality MVP: takes runner + CLI verb + honest Err(no key)
│       │   └── [x][X+] 1-1-5-3-2. eval-takes-quality harness: receipt/replay/regress/trend playback (356L TS)
│       ├── [x][X+] 1-1-5-4. eval-suspected-contradictions（#62，最大一族 judge×153）
│       ├── [x][Y+] 1-1-5-5. eval-brainstorm（#63，完整 generator 移植：4 引擎方法 + orchestrator/domain-bank/judges 三模块 + search/embed）
│       ├── [x][X+] 1-1-5-6. eval-suspected-contradictions trend 子命令（#62 延伸：write/loadContradictionsRun 引擎方法 + ASCII 图表）
│       ├── [x][X+] 1-1-5-7. eval-suspected-contradictions review 子命令（依赖 trend store report_json viewer）
│       ├── [x][Y+] 1-1-5-8. eval-suspected-contradictions JudgeCache 持久化 judge 缓存（独立性能优化，正交）
│       ├── [x][Y+] 1-1-5-9. eval-brainstorm 持久化 run store（save/list/gc，清 checkpoint stub）
│       ├── [x][Y+] 1-1-5-10. eval-brainstorm review 子命令（历史 run 查看 + grounding/pass 趋势）
│       └── [x][Y+] 1-1-5-11. eval-brainstorm checkpoint resume 续跑（resume playback 接入 orchestrator）
└── [~][X+] 1-2. G76 extract 族命令 Rust 重实现
    ├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
    ├── [x][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径（含 --by-mention）
    ├── [x][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，unblocked — Rust ConversationFactsBackfill 已就绪）
    └── [x][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb
<!-- ROADMAP_TREE_END -->

### 决策历史

| 节点 | 问题 | 答案 | 备注 |
|------|------|------|------|
| 1 | 下一步主线方向？ | ZBrain 收尾线：先清盘 Part13（已 commit 118dd853），再攻 G58/G74-G79 解锁评测/抽取特征 |  |
| 1 | G58 真实范围与下一步落点？ | G58 实为夜检探针 longmemeval/cross-modal eval 占位(honest error, flag off)；通用 LLM seam(ChatProvider+tool_loop)已存在。选定：攻 G74/G76 命令 Rust 重实现(用现有 seam)，而非做 G58 原生 port | ChatProvider(OpenAI/Anthropic/Gemini)+ai/tool_loop.rs 已 port 完(Phase8 slice3+5) |
| 1 | 1-1-5 与 1-2 并行还是串行？ | 串行：先收口 1-1-5 再动 1-2 | 1-2 有 G35 阻塞+1-2-4 未决，并行难独立验收 |
| 1 | 1-1-5 与 1-2 并行还是串行？ | 串行：先收口 1-1-5 再动 1-2 | 1-2 有 G35 阻塞+1-2-4 未决，并行难独立验收 |
| 1-1 | G74 标注的 blocked by G58（LLM seam）是否属实？ | 不属实。19 命令中真依赖 LLM 的只有 4 个（21%）：cross-modal、longmemeval、takes-quality、suspected-contradictions；其中前两个已由 G58 单列。其余 15 个不碰 LLM，G74 整体不应标 blocked。 | 审计法：git show 3c09a69f^ 取回 20 个 eval*.ts（含 notability-eval.ts，不在 G74 的 19 个之列），regex 扫 isAvailable('chat')|ChatProvider|anthropic|openai|.chat(|messages.create|gateway。三个疑似命中经逐行核实为假阳性：eval-replay.ts:132 的 OpenAI 只在注释里且指 embedding 非 chat；eval-schema-authoring.ts:12 注释明说走 stubbed gateway test seam；eval-suspected-contradictions.ts:253 的 anthropic 是 resolveModel 的 fallback 模型 ID 字符串——但该文件确实调 runContradictionProbe + judge model + 成本预算门，属真 LLM。 |
| 1-2 | G76 第一刀切哪个切片？ | extract 单命令端到端垂直切片：从 git 历史取回 extract.ts + extract-facts 算法，用 ChatProvider/tool_loop 重写抽取逻辑，接成 extract verb，TDD(red→green, MockChatProvider) 验证 seam 可行后，再批量扩 extract-conversation-facts + G74 的 19 命令 | 【已作废】此决策的前提（extract 依赖 LLM）经源码核实为假，见下一条决策 |
| 1-2 | 【前提推翻后重选】extract.ts 实为纯解析且大半已实现，第一刀改选哪个？ | 选 A：先修 SSOT（KNOWN-GAPS G76 拆为 G76a 非阻塞 / G76b 真 blocked by G35）+ 把已实现的 auto_fix::extract_timeline 暴露为独立 CLI verb。成本最低、顺带关掉 G76 一半、且先让真相源恢复准确。fs-source 与 conversation-facts 顺延为 1-2-2 / 1-2-3。 | 对账证据：extract.ts grep chat|llm|openai|anthropic|ChatProvider 零命中；用 extractPageLinks/parseTimelineEntries 纯解析 + engine.addLinksBatch/addTimelineEntriesBatch。Rust 侧 auto_fix.rs:179 extract_links 已由 CLI `links rebuild-md-links` 暴露(G77-1)，auto_fix.rs:319 extract_timeline 已实现但仅被 run_auto_fix(lib.rs:5153) 内部调用。真 LLM 的只有 extract-conversation-facts(isAvailable('chat')+分段+insertFacts+断点续跑审计行)。 |
| 1-2-1 | 只补一个 timeline verb，还是做完整 extract verb？ | 做完整顶层 `extract` verb 带 links|timeline|all 三子命令，一次性对齐 TS 命令形状并关掉 G76a-1/2/3。未来 --source fs 直接加 flag，不必再改命令树。 | clap 形状照抄 LinksAction/LinksRebuildMdLinksArgs 模板；命名不冲突（顶层无 Extract）。 |
| 1-1-5 | 保真度策略（5 族统一）？ | q-0 → 全保真 port（逐文件搬，最接近 TS 原貌） | 用户 2026-08-09 在「开始 1-1-5」时确认；TS 源从 git 历史 3c09a69f^ 取回 |
| 1-1-5 | 下一步优先级？ | 继续 1-1-5，收口 D 类 5 个真 LLM eval 命令 | G58 已关；剩余 3 项有界(259/399/427L TS 可恢复)；1-2 有 G35 阻塞+1-2-4 未决，不宜并行 |
| 1-1-5 | 3 个命令先后顺序？ | #61 takes-quality → #63 brainstorm → #62 suspected-contradictions | #61 最小且有 takes_scorecard Rust 杠杆；#63 需先 port brainstorm orchestrator 前置；#62 逻辑面最大(153 judge) 放最后 |
| 1-1-5 | 无 API key 时构建/验收策略？ | 沿用 G58：provider 无关 + mock 单测，无 key 也 cargo test 绿、无 provider 诚实 Err | 不为 API key 或 libsql Windows FFI flake 卡构建 |
| 1-1-5 | 下一步优先级重排（推翻 2026-08-09 的 #61→#63→#62）？ | 翻转为 #61 → #62 → #63（#62 优先） | 2026-08-12 对账 git 历史 39e14cd5：#63 实为完整 generator 移植（4 引擎方法缺失 + orchestrator/domain-bank/judges 三模块 + search/embed 集成，多会话量级），原「#63 只需 port 3 引擎方法」前提不成立。#62 复用 cross_modal judge 基础设施、零新引擎方法、无 generator，单会话可收口，故优先。 |
| 1-1-5 | 是否修正 1-1-5-5 过时描述并给 1-1-5-4 补收敛说明？ | 是 | 1-1-5-5：「3 引擎方法」→ 真实依赖面（4 引擎方法 + 三模块 + search/embed）；1-1-5-4：补「复用 cross_modal，收敛、无新引擎方法」。地图须反映真实范围，避免后续 agent 按低估前提误判。 |
| 1-1-5 | Q6 #62 之后优先级 | 先补 #62 延伸 trend(1-1-5-6)→review(1-1-5-7)，再 #63 brainstorm(1-1-5-5) | trend/review 是小延伸、直接完善 #62 价值；#63 是独立大块宜在 #62 完全收口后做 |
| 1-1-5-1 | 保真度策略？ | q-0 → 全保真 port（逐文件搬，最接近 TS 原貌） | TS 源 eval-cross-modal.ts 849 行；Rust 落在 crates/zbrain-core/src/eval/cross_modal.rs |
| 1-1-5-2 | 1-1-5-2 状态？ | completed — G58 实际已在 fa70b535 关闭，路线图登记滞后 | 遵循项目铁律：以 HEAD+编译为准，节点只作索引；地图系统性滞后 |
| 1-1-5-3 | 端口范围？ | 待定（round 2 烤问） | raw 体量 ~889L 为三者最大；judge 复用 cross_modal，scorecard 复用 calibration_queries |
| 1-1-5-3 | 顺序重确认（#61 体量已校正为最大）？ | 维持 #61→#63→#62：#61 复用度最高(judge+scorecard 已就位)+最自包含 | raw 体量 #61~889L>#62 427L>#63 399L，但 #61 新增集中且 judge/scorecard 大块已免做；#62 153-judge 矩阵、#63 orchestrator 前置为更差开局点 |
| 1-1-5-3 | 端口范围？ | MVP 先行：takes runner + 复用 cross_modal::run_eval judge + takes_scorecard 数学 + CLI verb；诚实 Err 无 key | 与增量交付/诚实降级哲学一致；receipt/replay/regress/trend(356L) 拆后续子节点 |
| 1-1-5-3 | judge 复用？ | 复用 cross_modal::run_eval（三模型 panel），不重 port DEFAULT_MODEL_PANEL | 同一套并行 judge 范式，重 port 违反 DRY 且 verdict 汇总语义分叉 |
| 1-1-5-4 | Q1 #62 收口标准 | 拆出 trend/review 为独立跟踪节点，#62 本体(run+retrieval)标记 completed | 因路线图父子状态自动同步：父有未完成子节点会被强制降级 in_progress，故 trend/review 建为 1-1-5 下平级节点(1-1-5-6/7)而非 1-1-5-4 子节点，否则 #62 无法真正收口 |
| 1-1-5-4 | Q2 judge 架构是否终态 | one-call-one-pair judge 为终态 | TS 仅 1 个 judge 函数 judgeContradiction，标题 judge×153 是调用次数(pairs×runs)非 153 个 judge；复用 Rust facts/classify+calibration.rs 已够，除非评测漏判率高否则不另 port calibration-join/severity-classify |
| 1-1-5-4 | Q3 JudgeCache 是否纳入 #62 | 不纳入 #62，单独建节点(1-1-5-8) | JudgeCache 是 judge 持久化缓存(30d TTL, 顺序无关 key)，性能优化且与 trend/review 正交；run 已可无缓存工作 |
| 1-1-5-5 | Q1 是否一并移植 LSD 模式？ | 是，移植 LSD 模式——复用同一生成/打分 pipeline，增量成本极低，避免 brainstorm 与 lsd 两条路径长期分叉。 | LSD = 长程记忆评估模式；与 brainstorm 共享 orchestrator 四阶段骨架，仅 profile 配置与 stale-bias 默认值不同（BRAINSTORM_PROFILE vs LSD_PROFILE）。 |
| 1-1-5-5 | Q2 三引擎方法如何落地？ | 忠实 port 三个 BrainEngine 方法（list_prefix_sampled_pages / list_corpus_sample / get_embeddings_by_chunk_ids）跨 InMemory / Libsql / Postgres 三后端，并补单测。 | Rust content_chunks 缺 embedding 列（TS 设计依赖之），新增 0033 migration（双方言：sqlite BLOB / postgres BYTEA，f32-LE 编码，对齐 pages.embedding G24）。排名/选择/解码逻辑抽成共享 free helper（rank_domain_bank_prefix_sample / pick_corpus_sample / collect_domain_bank_raw / domain_bank_prefix），三后端调用同一实现防漂移。InMemory 无 chunk id 列，发明确定性合成 id（FNV-1a，inmemory_chunk_id）使 representative_chunk_id 与 get_embeddings_by_chunk_ids 自洽。 |
| 1-1-5-5 | Q3 MVP 是否实现 resume / checkpoint 回放？ | 否，MVP 不实现 resume/checkpoint 回放，仅留 TODO 骨架（checkpoint 模块占位）。 | TS checkpoint.ts 是断点续跑，首版聚焦端到端生成+打分闭环；回放可后续会话补。 |
| 1-1-5-5 | Q4 无 API key 时策略？ | 诚实 Err 返回 + provider-agnostic mock 单测，不伪造打分。 | judges 的真实 LLM 调用在无 key 时返回结构化 Err；单测用 mock provider 验证打分逻辑，不依赖真实 key。 |
| 1-1-5-5 | Q5 首版成本管控保留哪些？ | 首版仅保留 --max-cost 硬上限（BudgetTracker 若存在否则简单 USD 阈值）；TTY 交互式成本预览延后。 | MVP 只做硬上限熔断；交互式预览需 TTY 探测，留待后续。 |
| 1-1-5-6 | Q4 trend 实现策略 | 忠实 port：三后端各加 writeContradictionsRun/loadContradictionsTrend 引擎方法 + contradictions_runs 表 migration（存 TrendRow + report_json）+ 移植 renderTrendChart ASCII 图表 | TS trends.ts 仅 118 行薄封装，依赖 engine.writeContradictionsRun + engine.loadContradictionsTrend 两方法 + ASCII 图表；'trends 子系统'=2 引擎方法+1 图表渲染器，自包含中等工作量，非大子系统 |
| 1-1-5-7 | Q5 review 范围 | review = trend store 的 report_json viewer（加载最近一次 run 的 report_json 美化打印 verdict_breakdown/配对明细） | TS review 依赖 trend store 已存在，仅读最近 run 的 report_json(ProbeReport blob) 展示；不膨胀为交互式标注队列 |
| 1-1-5-7 | A 决策：review 数据范围（per-pair findings 从哪来） | 选 A：不是 roadmap scope creep——TS ProbeReport 带 per_query→contradictions(per-pair findings)，review 读 report_json.per_query.flatMap 取全部 findings；Rust 侧 ContradictionsResult.findings 运行时已有。初版 1-1-5-6 把 report_json 少存了 findings(忠实 port 缺口)，先补 1-1-5-6 把 findings 塞回 report_json，再忠实 port review | severity/since 是 TS 合法忠实 flag，不是死参，保留；不需要新 migration(列已存在)；review 镜像 trend 臂 DB 连接样板 |
| 1-1-5-8 | 是否全量忠实 port JudgeCache（0032 表 + 三后端方法 + 顺序无关 buildCacheKey + in-process hit/miss 计数 + 30d ttl + --no-cache）？ | 是，全量忠实 port（采纳推荐） | 不偷工减料；cache 与 run() 解耦，run() 重构接收 JudgeVerdict 而非在 chat 层塞缓存 |
| 1-1-5-8 | 缓存主键与 upsert 语义？ | 复合 PK 五元组 (chunk_a_hash, chunk_b_hash, model_id, prompt_version, truncation_policy)，ON CONFLICT DO UPDATE 刷新 expires_at | chunk_a/b 在 buildCacheKey 阶段已排序，保证 (a,b)/(b,a) 命中同一行 |
| 1-1-5-8 | run() 如何处理缓存命中？ | 命中则跳过 judge、tally verdict、按 is_finding 发射 finding；未命中 judge 后 store；命中/未命中分别计入 report | 与 TS runner.ts 一致 |
| 1-1-5-8 | 是否加 --no-cache 开关？ | 是，加 --no-cache（默认 false；disabled 时 lookup 直接 miss、store 跳过） | 对应 TS RunnerOpts.noCache |
| 1-1-5-8 | ttl 与 sweep 策略？ | 默认 ttl 30 天；sweep 不在每次 run 自动调用（MVP 不接周期任务），提供 sweepContradictionCache 方法供显式调用 | 与 TS 一致（sweep 由 cache.ts 周期调用，Rust MVP 先不接） |
| 1-1-5-9 | 方向：brainstorm 延伸 = 1-1-5 下一组节点 | 镜像 suspected-contradictions 的 trend/review 模式 + 填 checkpoint.rs 的 Q3 MVP stub（save/list/gc/load/resume） | 1-1-5-5 刚收口，checkpoint.rs 仅 compute_run_id 实装、其余为 no-op；sc 已验证 trend/review/JudgeCache 延伸路径可行，可作为模板 |
| 1-1-5-10 | CLI 形态：review/trend 如何挂到 brainstorm/eval-brainstorm？ | 扁平 flag 扩展（沿用 1-1-5-9 的 --list-runs/--gc/--store-dir）：--list-runs 表加 pass-rate% + mean-grounding 列；新增 --review-run <run_id>（全文复用 format_brainstorm_markdown + 元信息头，--json 出原始 row）、--trend [--days N] ASCII 双轴图；三者连引擎前早退 | 与 1-1-5-9 扁平决策一致，零 clap 重构 |
| 1-1-5-10 | 趋势图绘哪几条线？ | 仅 pass-rate（n_passed/n_ideas）+ mean grounding（concrete_grounding 1-5）两轴 | 节点名点名的 grounding/pass 趋势；cost 已在 --list-runs 表显示，不进图 |
| 1-1-5-10 | review-run RUN_ID 指定时打印什么？ | 复用 format_brainstorm_markdown(result) 出与线上 run 一致的全文报告 + 顶部元信息头（run_id/saved_at/profile/cost/store 路径）；--json 改打原始 BrainstormRunRow JSON | 最高保真，复用 formatter |
| 1-1-5-10 | review/trend/list 作用范围（双命令还是仅 brainstorm）？ | brainstorm 与 eval-brainstorm 双命令都挂（共享 store 目录，eval fixture run 也存进去） | 共享 store 不分裂视图 |
| 1-1-5-10 | trend 时间窗（--days 默认 30 还是全量）？ | 时间窗按 --days N（默认 30）截断；--list-runs 始终全量；review 默认最新一条，除非 --review-run 指定 | 与 sc --days 一致 |
| 1-1-5-11 | Resume 主轴语义（--resume 填实后做什么） | 重跑再生：加载存储 run 的 question+profile+close/far 输入，重新执行 run_brainstorm 重新生成 ideas；无需 partial 持久化 | store 只落完整 run，原 crashed-run 增量续跑不可行，故选重跑再生贴合续跑/复现 |
| 1-1-5-11 | 本节点是否一并给 eval-brainstorm 变体接 resume | 两者同接：brainstorm 与 eval-brainstorm 命令都接 --resume/--force-resume（与 review_run/trend 对称） | 用户显选 B，scope 扩到 eval 变体 |
| 1-1-5-11 | Staleness 闸门：--force-resume 是否实现 7 天闸门？ | 实现 7 天闸门：resume 默认拒 7 天前 run（与 STALE_MS/GC 窗口一致），--force-resume 绕过 | 让既有 dead flag 产生真实语义 |
| 1-1-5-11 | Resume 的 run_id 与保存策略：重跑后如何处理 run_id 撞原 run？ | 新 run_id + 默认保存：重跑后把 result.run_id 改写为 <原>~r<ts> 再落盘；原 run 保留，trend 可对比续跑 vs 原跑。 | 避覆盖，契合 1-1-5-10 trend |
| 1-1-5-11 | Resume 续跑的 close/far 页面保真度？ | 按 question 重新发现：调 run_brainstorm 用存储 question+profile 重跑 Phase 1-2，close/far 重新 hybrid-search 发现（DB 变化可能不同）；不改 orchestrator。 | exploit 取最小改动；BrainstormResult 不含正文须重取 |
| 1-1-5-11 | Resume 节点的测试策略？ | 单测 + CLI 冒烟：单测覆盖 resume 分支（mock load+orchestrator，验证 加载→重建 options→新 run_id 保存）；CLI 用隔离 config 真库冒烟 --resume 不崩、复用 question、产生新 run_id。 | 与 1-1-5-9/10 测试风格一致 |
| 1-1-5-3-2 | Q1-final 归宿（harness 四件套 receipt/replay/regress/trend）？ | 选 A：保真 port TS 四件套全写（~356L + 测试），自包含全 parity；不复用 eval/replay.rs + compare.rs + gate.rs 的 regression 能力 | 事实核查(2026-08-15 grill，用户 catch)：replay.rs::CapturedRow 是检索重放模型{tool_name,query,retrieved_slugs,latency_ms}、RowResult 量 jaccard/top1_match/latency_delta、ReplaySummary 报 rows_over_2x_latency——与 takes-quality 的 judge 打分 takes(insight/accuracy/clarity/actionability) 数据模型完全正交；compare.rs::compare_reports 硬耦合 RunAllReport、gate.rs 的 RegressionGateOutput 是 run-all 正确性 gate 流(cli lib.rs:8985/9066/9783/9795)，三者均 run-all 私有 machinery，takes-quality 只 emit TakesQualityResult+judge receipts 不产 RunAllReport，无法直接复用（除非方枘圆凿硬塞 RunAllReport）。唯一可复用中性辅助=replay.rs::jaccard_slugs(slug 集相似度)；brainstorm 的 summarize(store.rs:130) 同名不同型自写、未复用 replay.rs → 全仓无跨 eval 模块共享 regression 原语。故 A vs B-min 真实轴=4 件 vs 2 件领域代码（非复用 vs 不复用）；A 自包含干净全 parity，修订后推荐 A。 |

<!-- ROADMAP_SECTION_END -->
