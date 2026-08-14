<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-14 17:10:58

[~][X+] 1. ZBrain G74/G76 eval+extract 命令 Rust 重实现
├── [~][X+] 1-1. G74 eval 族命令 Rust 重实现（19 命令）
│   ├── [x] 1-1-1. 修正 KNOWN-GAPS/COMMANDS_TEAR_DOWN 的 G74 失准描述
│   ├── [x] 1-1-2. 第一刀：zbrain eval 核心 verb（暴露已 port 的 run_eval）
│   ├── [x][X+] 1-1-3. 判废真空壳 + 重分类 2 非空壳 eval 命令（markdown-greenfield 判废；extract-atoms/synthesize-concepts 底层 cycle phase 已在 Rust）
│   ├── [x] 1-1-4. 非 LLM 但需新基建的 9 个 eval 命令（eval-brainstorm 复核为 LLM 已移 D 类）
│   └── [~][X+] 1-1-5. 真 LLM 的 5 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions / brainstorm）
└── [~][X+] 1-2. G76 extract 族命令 Rust 重实现
    ├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
    ├── [ ][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径（含 --by-mention）
    ├── [ ][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，blocked by G35）
    └── [ ][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb

### 已收口：1-1-5-5. eval-brainstorm（#63，完整 generator 移植：4 引擎方法 + orchestrator/domain-bank/judges 三模块 + search/embed）+ #298 CLI 接线

真实依赖面（2026-08-12 对账 git 历史 39e14cd5 修正）：
- TS runBrainstorm 是生成器（先生成 ideas 再打分），非对已有内容打分。
- 缺失引擎方法：list_prefix_sampled_pages / list_corpus_sample / get_embeddings_by_chunk_ids（×3），外加 search/ai 层 hybrid_search / embed_query（Q1 起 LSD 复用同 pipeline）。
- 需新建 3 模块：orchestrator / domain-bank / judges（+ checkpoint 占位、error-classify）。
- eval gate 3 轴：DISTANCE≥0.4 / USEFULNESS≥3.5 / GROUNDING=1.0（复用 is_brainstorm_output 标记检测，Rust 已有）。
- 体量：多会话量级（4 引擎方法 + 3 模块 + search/embed 集成），非一刀可收口。
- is_brainstorm_output 标记检测 Rust 已有（transcript_discovery.rs）。

进度（2026-08-14）：
- Q2 引擎方法三后端已实现 + 0033 migration（双方言）+ InMemory 单测（prefix 采样/connection tiebreak/stale-bias 排序/corpus 种子确定性/合成 chunk id 水合）；libsql 集成测试已补（7 项：临时 .db + connect + init_schema + put_page 播种 pages/links/content_chunks.embedding，验证 prefix 采样/representative_chunk_id 水合/embeddings 跳过未嵌入块）；Postgres 实现到位（= ANY($N::text[]) / = ANY($1::bigint[])）。**Q2 全绿：cargo test -p zbrain-core --lib domain_bank → 20 passed / 0 failed**（PowerShell 跑，lld-link 链接）。真实 bug 修复：libsql/postgres LEFT JOIN page_links→links（真实表名）；decode_f32_blob 补 Some 包裹；inmemory_chunk_id 合成 id 自洽；domain_bank_prefix 双段 slug 返回 Some（对齐 TS 正则 ^[^/]+/[^/]+）。
- 待办（已于 2026-08-14 #298 收口）：orchestrator（四阶段 + 双 profile + distance 打分 + --max-cost + checkpoint hook）、domain-bank（enumeratePrefixes/fetchFar/normalizedCosineDistance/INJECTION_PATTERNS/前缀缓存）、judges（runJudge + 两配置）、checkpoint 占位、error-classify、eval 命令 + CLI 接线（--max-cost/--profile brainstorm|lsd/--no-cache）。

收口（2026-08-14 #298 CLI 接线）：
- orchestrator 四阶段管线（hybrid_search 取 close-set → domain-bank fetch_far 取 far-set → 逐 (close×far) 交叉生成 → run_judge 评分）+ 双 profile（BRAINSTORM_PROFILE: K=4/M=6/ideas=3；LSD_PROFILE: K=2/M=12/ideas=4/stale_bias）+ distance 打分 + --max-cost 硬上限 + checkpoint hook 已落地。
- domain-bank 前缀分层采样（^[^/]+/[^/]+ 双段 prefix，cost guardrail max_far_set=max(m*4,50)）+ normalizedCosineDistance + INJECTION_PATTERNS + 前缀缓存已落地。
- judges（BRAINSTORM_JUDGE_CONFIG threshold 4.0 / LSD_JUDGE_CONFIG threshold 3.5 + reject_if_resistance_above:4.5，5 轴权重和=1.0）+ run_judge 分块（max_ideas_per_call 默认 100）+ chat seam 已落地。
- checkpoint（compute_run_id sha256 前 16 位；save/load/list_runs/gc/clear 为安全 no-op 占位，Q3 回放未接）。
- error-classify（SQLSTATE 57014 → brainstorm_timeout StructuredError 携带 hint；非 57014 透传）。
- eval 命令 + CLI 接线（zbrain brainstorm / zbrain lsd / zbrain eval-brainstorm 三动词 + clap 解析 + 无 key 优雅降级 + --json 输出 BrainstormResult + save 策略 default_save 区分 brainstorm=true/lsd=false）。
- **构建全绿：cargo check -p zbrain-cli 0 error；cargo test -p zbrain-cli brainstorm_cli_tests 4 passed / 0 failed（PowerShell + lld-link 链接）。CRLF 已核验（git diff --numstat 删除列全 0，无全文件重写）。**

**决策：**
- Q: Q1 是否一并移植 LSD 模式？ → 是，移植 LSD 模式——复用同一生成/打分 pipeline，增量成本极低，避免 brainstorm 与 lsd 两条路径长期分叉。 (LSD = 长程记忆评估模式；与 brainstorm 共享 orchestrator 四阶段骨架，仅 profile 配置与 stale-bias 默认值不同（BRAINSTORM_PROFILE vs LSD_PROFILE）。)
- Q: Q2 三引擎方法如何落地？ → 忠实 port 三个 BrainEngine 方法（list_prefix_sampled_pages / list_corpus_sample / get_embeddings_by_chunk_ids）跨 InMemory / Libsql / Postgres 三后端，并补单测。 (Rust content_chunks 缺 embedding 列（TS 设计依赖之），新增 0033 migration（双方言：sqlite BLOB / postgres BYTEA，f32-LE 编码，对齐 pages.embedding G24）。排名/选择/解码逻辑抽成共享 free helper（rank_domain_bank_prefix_sample / pick_corpus_sample / collect_domain_bank_raw / domain_bank_prefix），三后端调用同一实现防漂移。InMemory 无 chunk id 列，发明确定性合成 id（FNV-1a，inmemory_chunk_id）使 representative_chunk_id 与 get_embeddings_by_chunk_ids 自洽。)
- Q: Q3 MVP 是否实现 resume / checkpoint 回放？ → 否，MVP 不实现 resume/checkpoint 回放，仅留 TODO 骨架（checkpoint 模块占位）。 (TS checkpoint.ts 是断点续跑，首版聚焦端到端生成+打分闭环；回放可后续会话补。)
- Q: Q4 无 API key 时策略？ → 诚实 Err 返回 + provider-agnostic mock 单测，不伪造打分。 (judges 的真实 LLM 调用在无 key 时返回结构化 Err；单测用 mock provider 验证打分逻辑，不依赖真实 key。)
- Q: Q5 首版成本管控保留哪些？ → 首版仅保留 --max-cost 硬上限（BudgetTracker 若存在否则简单 USD 阈值）；TTY 交互式成本预览延后。 (MVP 只做硬上限熔断；交互式预览需 TTY 探测，留待后续。)
<!-- ROADMAP_SECTION_END -->
