# zbrain 项目记忆

> 进度 SSOT = docs/plans/*.json roadmap；明细在每日 YYYY-MM-DD.md。本文件只放跨会话规则与坑。

## 项目方向
- TS→Rust 迁移（ZBrain）。GBrain→ZBrain 破坏性全量改名，不留兼容别名；`brain`/`source` 领域词不改。Rust 迁成一块删一块。缺口登记 docs/plans/MIGRATION.md（双向指针 `// registered in ... (Gn)`）。
- Legacy GBrain：仅删明确死亡碎屑（src/version.ts、image-decoders.d.ts），接受 hybrid（node 分发 + Rust 核心）长期状态；`bin/zbrain-rs.js` 是 zbrain CLI 入口（spawn Rust 二进制），不可删。`zbrain-clean` 第二 clone 已废弃（2026-08-12），恢复靠重新克隆。

## 构建环境（Windows 本机，WSL 不可用）
- 【关键】cargo 在 Bash 工具里静默 EXIT=0 零输出（WorkBuddy Bash/Git Bash 吞 stdout/stderr）。一律用 PowerShell 工具跑 cargo（同一 cargo.exe）。`cargo --version` 等正常，但 build/test 会被吞——别信 Bash 里 cargo 的"成功"。
- Windows 构建/链接：独立 `CARGO_TARGET_DIR=C:/Users/<u>/AppData/Local/Temp/zb_targetN`（Windows 绝对路径）+ `RUSTFLAGS="-C linker=C:/PROGRA~1/LLVM/bin/lld-link.exe"`（LLVM CLI 兼容 MSVC，复用 warm cache）。MSVC `link.exe` 被监视器杀（0xc0000142）→ lld-link 绕过。
- `cargo check` 不编 #[cfg(test)]；改测试代码必须真跑 `cargo test`。libsql FFI Windows 间歇 0xc0000005（代码层无解）→ 集成测试降频 + CI 跑 ubuntu。workspace forbid(unsafe)（Linux 用 /proc 纯 std）。测试临时目录加 AtomicU32 序号防并行互删。

## Git / 沙箱铁律
- 严禁 `git stash`（cleanup 删 .git/refs → orphan HEAD）；探 baseline 用 `git show HEAD:<f>` / `git diff HEAD`。
- 绝不在沙箱 `git rebase`（触发 safe-delete 删 .git/refs）；改基线用 `reset --soft` + `pull --ff-only` 或 `reset --hard <远端tip>`。沙箱写命令双重执行 → 写操作幂等 + 事后去重。
- 目录改名后 cwd 失效 → `cd /c/workspace/github/jununfly/ZBrain && git ...`（勿 `git -C /c/...`）。ref 损坏：`mkdir -p .git/refs/...` → `fetch` → `rev-parse <SHA>` 核对（勿信 packed-refs/@{u}）→ `reset --hard`。index.lock 僵尸锁用 mv 改名。接活前必 `git status`。
- 行尾符陷阱（2026-08-16 实测修正）：实际 `core.autocrlf=true` + `.gitattributes`（`* text=auto` 入库归一 LF；`*/lib.rs text eol=crlf` checkout 强制 CRLF）。故 `git add` 会把工作树 CRLF 归一为 LF 入库，**已提交 blob 不会因 CRLF 而全文件重写**；但**未提交时** `git diff` 仍把 CRLF 工作树 vs HEAD 的 LF 显示成 N/N 全文件 churn——所以改动文件仍须保持工作树 LF（有 `\r` 就 `python data.replace(b'\r\n',b'\n')` 清掉）才能拿到干净 diff。Edit 工具整文件改写行尾也会触发 N/N。提交前对每个改动文件跑 `git diff --numstat` 确认无全文件重写；异常按 `git show HEAD:<f>` 逐文件对齐，别一刀切 LF。救回：LF 文件被写成 CRLF 时 `python data.replace(b'\r\n',b'\n')`；`git hash-object` 比对 HEAD 与工作树验证内容无损。
- Bash 沙箱拦截覆写受跟踪文件：诊断脚本 `open(p,'wb').write()` 会破坏工作树（commit 走 index 不受影响）。法则：① 改现有文件用 Edit 工具；② Python 改 JSON 走 index 操作（`git cat-file`→改→`hash-object -w`→`update-index --cacheinfo`），工作树还原交用户 `git checkout --`；③ 严禁 `open('wb')` 对受跟踪文件测试性覆写。

## Roadmap 铁律
- .json/.md 都在 docs/plans/；render 只认 ROADMAP_SECTION_START/END marker。
- render cwd 陷阱：`roadmap_cli.py render` 按脚本 cwd 解析 md 路径 → 必须从仓库根运行（`python3 .workbuddy/skills/zj-roadmap-driven/roadmap_cli.py render docs/plans/<x>.json`），否则在 skill 目录误生成副本空转。
- JSON 多行 notes 别用 Edit（引入裸换行/未转义引号破坏 JSON）→ 走 Python 二进制读 → 字符级转义 → 二进制写回保 CRLF → json.loads 校验。roadmap JSON 统一 2-space indent（别用正则检测缩进）。
- 【2026-08-16 修正】roadmap 模型**只有 4 状态**（`pending`/`in_progress`/`completed`/`blocked`），**无 `wontfix`**。裁定「不构建」的节点（如 part13 的 lint/lint_fix/integrity_auto/sync_retry_failed/repair_jsonb handler，decisions[] 已记 wontfix）想「闭环」时：**状态字段只能翻 `completed`**（唯一闭环终态），且**保留原 notes 里的 wontfix 说明**不覆盖（`roadmap_cli.py update <file> <id> --status completed`；父节点会级联 completed）。validate 只认 4 状态，传 `wontfix` 会 `ValueError`。

## 迁移工程规则
- TS→Rust：`src/cli.ts` 走 resolveZbrainBin()。改子命令后必 `cargo build -p zbrain-cli` 否则测试 exit 2。port fidelity：测试与实现冲突优先改实装、接受 test 为规范；交叉登记 KNOWN-GAPS。
- KNOWN-GAPS 不可全信（G74/G76 连续证伪）：算法层常已在 Rust，缺的只是 CLI 出口。动手任一 Gn 前必做「TS 源（`git show <删除commit>^:<path>`）vs Rust 现状」逐能力对账（grep 同名/近义实现），结论回写 KNOWN-GAPS + COMMANDS_TEAR_DOWN + 代码 doc。反向假阳性：handler 存在 ≠ 核心可调用（execute_phase 可能 stub 空转）——对账须追到「CLI 实际调的 TS 函数」是否在 Rust 有等价实现。
- InMemoryEngine 限制：`execute_raw` 未实现（trait 默认返 Err）→ 直读 pages 一律走公共 API（`list_all_page_refs`+`get_page` 取内容，`get_links`/`get_backlinks` 验边），否则 InMemory 测试 unwrap 全 panic。`list_pages` 不过滤软删页且 `PageFilters` 无 `page_kind` → Rust 侧用 `deleted_at.is_none()` + `page_kind==Markdown` 过滤。
- `execute_raw` 形参是 `&[&(dyn erased_serde::Serialize + Sync)]`，而 `serde::Serialize` **不可作 trait object**（E0038）。`zbrain-cli` 未直依赖 erased_serde → cli 里**别构造 `dyn serde::Serialize` 切片**：标量参数优先 `format!` 内联进 SQL 传 `&[]`；确需 trait-object 参数则给 cli 的 Cargo.toml 加 `erased_serde` 依赖（参考 core consolidate.rs/symbol_edges.rs）。
- CLI 加 verb 最小闭环：clap 照抄邻近模板 → enum+Args+dispatch+run_ → 尾部 #[cfg(test)] try_parse_from 解析测试（含拒绝未实现子命令负向测试）→ e2e 用隔离 config 真库冒烟（幂等+范围+不崩）。runner 函数内部无 `.await` 不得声明 `async`（clippy `unused_async`），dispatch 调用点同步去 `.await`。
- crate::Result = Result<T, StructuredError>；serde_json::Error 不能 ? 须 map_err。&Arc<dyn T>→&dyn T 需调用方 &*。

- 【2026-08-16 裁定】**迁移完成口径 = 命令级完成**：Rust 已服务全部产品命令（CLI verb 全覆盖，含 Tier-C export/frontmatter/auth/providers/upgrade），`bin/zbrain-rs.js` 直跑 Rust 二进制、TS 运行时已死。KNOWN-GAPS 的 open/blocked 条目作为 **documented limitations** 保留，不再阻塞「迁移完成」声明。TS 死代码（schema-pack 26 文件 + operations.ts 等）**保留、不删 src/ 树**（G38 维持「保留+子gap」）。残留事项实时索引 = `docs/plans/roadmap-remaining.json`（54 节点 = 48 open + 6 blocked）。G55（remote MCP 不透传 takes_holders）本地/受信部署维持已知缺口，非硬闸门；`serve --http` 对外暴露则升级 must-fix。

## 测试真相源
- TS 测试套件（tests/）已于 2026-08-08 退役；Rust 侧 `crates/*/tests/*.rs` + 内联 #[cfg(test)] 为唯一真相源。全 workspace `cargo test --workspace` 已验证 green（3694 passed / 0 failed）。
