# zbrain 项目记忆

- 用户明确要求：当前在 `zbrain` 项目内工作时，不要与其他项目或跨项目路线图混淆；继续工作应优先使用本仓库上下文、`.workbuddy/memory/` 和项目文件，而不是套用其他项目任务。
- 项目方向：当前处于 TS -> Rust 迁移过程，选择“Rust 重写线 ZBrain”；整个仓库语言统一迁到 ZBrain，所有 GBrain 品牌都改名为 ZBrain。
- 品牌迁移策略：目前没有线上用户，第一阶段可以包含破坏性接口改名（CLI/bin/package/env/dotfile/config/public examples 等），不需要默认保留 GBrain 兼容别名；历史 GBrain CHANGELOG 内容意义不大，直接删除/reset，从 ZBrain 首个 release 重新记录。
- 命名迁移范围：连文件名一起改。第一阶段彻底迁移 `gbrain.yml -> zbrain.yml`、`docs/GBRAIN_*.md -> docs/ZBRAIN_*.md`、package name/bin/env/dotfile/path/docs/test 引用等；TS 代码部分先不直接动，随“完成 TS -> Rust”PRD 迁移成功一部分就删除一部分。执行分层：配置/包名/bin；env/dotfile/path；docs 文件名与引用；测试脚本引用；最后验证断链。
- 配置兼容策略：`gbrain.yml`、`.gbrain*`、`GBRAIN_*`、`~/.gbrain` 全部迁到 ZBrain 命名，不保留 alias/fallback/兼容读取；`brain` 和 `source` 作为领域词不改。
- 下个 PRD：`docs/prd/complete-ts-to-rust.md`。核心原则：TS 代码先不直接动；Rust 迁移成功一部分就对应删除一部分 TS；不适合直接删的内容到时讨论并记录决策。
- 测试目录迁移：整体物理迁移 `test/` 到 `tests/unit/`，这只是目录规范迁移，不等于删除 TS 测试；现有 `tests/heavy/` 保持，scripts 测试 glob 和文档引用同步改。当前已完成物理迁移与 runner/config 改动，阻塞在本机无 `bun`，无法执行 typecheck/test 分片验证。
- Plans 清理：将 `docs/plans/20260526/` 提炼为 canonical 文档 `docs/plans/20260526-rust-rewrite-plan.md`（目标范围、slice 列表、已确认决策、当前状态/后续切片、废弃/关闭调查结论），提炼后删除连续过程文件；唯一且仍有效的决策必须先提炼再删。
- Roadmap 拆 plan 约定：若审计或拆解时发现与当前节点目标存在语义偏差，且有跟进价值，应拆成 sub-node 跟踪，而不是吸收到当前 plan 中。
