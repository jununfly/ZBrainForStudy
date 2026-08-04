<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part9-phase11-package-cutover.json` | 最后更新: 2026-07-15 11:29:49

[x][X+] 1. Part9 Phase11 — package 入口 cutover + src/core 删除
├── [x][Y+] 1-1. package.json cutover(改 main/exports/scripts + 清 exports CI guard + 清悬空引用)
├── [x][Y+] 1-2. 重算 KEEP 种子 + 依赖闭包 D(cutover 后新种子集)
├── [x][X+] 1-3. 分批删 src/core 已迁 impl(tsc 基线 diff 验证,逐 slice)
└── [x][X+] 1-4. final cutover 收尾(typecheck 脚本处置 + CHANGELOG + 残留 TS 决策)
<!-- ROADMAP_SECTION_END -->
