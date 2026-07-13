<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part6-phase8-ai-gateway.json` | 最后更新: 2026-07-13 17:21:51

[x][X+] 1. ZBrain TS→Rust Part6: Phase 8 — AI Gateway / Providers / Models / Routing
├── [x][Y+] 1-1. Model registry + capabilities + pricing 数据层 (仿 embedding_pricing.rs static 表, 迁 recipes/capabilities/dims/types)
├── [x][Y+] 1-2. model-resolver + routing (parseModelId/resolveRecipe/tier-alias/assertTouchpoint, 依赖数据层)
│   └── [x][X+] 1-2-1. tier-routing 层 (model-config.ts resolveModel/TIER_DEFAULTS/enforceSubagentCapable/Anthropic, async+BrainEngine config 读取+capability 门控)
├── [x][Y+] 1-3. ChatProvider trait + 类型全保真 + OpenAI HTTP 实现 (独立 trait 非扩 LlmClient; 吸收原 1-4 chat 单调用 HTTP)
├── [x][Y+] 1-4. chat 剩余 provider (anthropic/google native) + BudgetTracker 接线 (chat trait/OpenAI HTTP 已在刀3)
│   ├── [x][Y+] 1-4-1. Anthropic native chat provider (build_body/serialize/parse 三段式, 照 subagent.ts:478 逐字对照 + cache token + tool_use stop_reason)
│   ├── [x][Y+] 1-4-2. Google/Gemini native chat provider (contents/parts/functionCall 格式, 照 Gemini 官方 REST, 无 TS 逐字样本)
│   └── [x][X+] 1-4-3. BudgetTracker 接线 (reserve/record + pricing 查表 + ISO 周审计 JSONL 复用 rerank_audit 先例, ambient-store 注入方式单独 grill)
├── [x][Y+] 1-5. toolLoop 工具循环 (provider-agnostic tool loop, 最重, 依赖 chat)
└── [x][Y+] 1-6. embed/rerank/expand 收口 (rerank 近完成去 mock, embed 去 MockProvider, expand 迁移; 可与数据层并行)
<!-- ROADMAP_SECTION_END -->
