<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part8-phase10-eval-disposition.json` | 最后更新: 2026-07-15 08:37:13

[x][X+] 1. Part8 Phase10 — eval 子系统处置决策
├── [x][X+] 1-1. longmemeval 处置(有 Rust seam run_long_mem_eval + 专用 CI job)
├── [x][X+] 1-2. cross-modal-eval 处置(有 Rust seam run_cross_modal_batch + 写 receipt)
├── [x][X+] 1-3. eval-contradictions 处置(3040 行最大块, DB 表+calibration)
├── [x][X+] 1-4. takes-quality-eval 处置(1398 行, 写 receipt/regress)
├── [x][X+] 1-5. code-retrieval 处置(753 行, 自带 questions.json)
├── [x][X+] 1-6. core/eval 共享层处置(drift-watch/search-eval/json-repair, 唯一被生产 import)
└── [x][X+] 1-7. eval CLI 命令层处置(4260 行 16 文件, eval-gate 做 CI gate)
<!-- ROADMAP_SECTION_END -->
