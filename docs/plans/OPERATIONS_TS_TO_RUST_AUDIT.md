# operations.ts → Rust OperationRegistry 覆盖审计

> 生成于 1-6-5-9 收口后，作为「替换式增量迁移 operations.ts」(roadmap 1-6-7) 的输入地图。
> 原则（Q3 决策）：以 Rust `zbrain_core::operation::{OperationContext,OperationRegistry}` + `BrainEngine` 为继任者，
> 把 TS `operations.ts` 的 107 个唯一 operation 逐个对齐，随迁随从 `operations.ts` 删除。

## 总览

- TS `operations.ts` 唯一 operation 数：**107**（含别名去重后）
- Rust `operation.rs` 已定义 `Operation` 结构体：**10**（page CRUD 6 + query + think + takes_list + takes_search）
- `zbrain-mcp` 实际注册：**10** 个（同上）
- Rust `BrainEngine` trait 方法：**~120**，覆盖绝大多数 operation 领域

### Bucket 分布

| Bucket | 数量 | 含义 |
|--------|------|------|
| PORTED | 10 | 已是 Rust Operation 结构体 |
| WRAP | 87 | agent-operation，Rust engine 已有方法 → 包成 Operation |
| COMMAND_ELSEWHERE | 7 | 实为 CLI 命令，已在 1-6-4/1-4/1-5 迁为 `zbrain <cmd>` → 从 operations.ts 摘除 |
| NET_NEW | 3 | 真缺（无 engine 方法、非已迁命令） |

**关键结论**：`WRAP` 占大头——剩余工作主要是「把已有 engine 方法包成 Operation 并注册」，
不是重写逻辑。只有 `file_upload`/`search_by_image`/`get_brain_identity` 等少数为 net-new。

## 领域 rollup

| 领域 | 总数 | PORTED | WRAP | COMMAND_ELSEWHERE | NET_NEW |
|------|------|--------|------|------------------|---------|
| anomalies | 1 | 0 | 1 | 0 | 0 |
| code-intel | 7 | 0 | 7 | 0 | 0 |
| commands/misc | 13 | 1 | 6 | 5 | 1 |
| facts | 3 | 0 | 3 | 0 | 0 |
| files/attachments | 5 | 0 | 4 | 0 | 1 |
| health/stats | 6 | 0 | 6 | 0 | 0 |
| ingestion | 4 | 0 | 4 | 0 | 0 |
| jobs/minions | 12 | 0 | 12 | 0 | 0 |
| links/graph | 9 | 0 | 9 | 0 | 0 |
| page | 18 | 6 | 12 | 0 | 0 |
| schema-pack | 9 | 0 | 9 | 0 | 0 |
| search/query | 3 | 1 | 1 | 0 | 1 |
| sources | 4 | 0 | 4 | 0 | 0 |
| tags | 6 | 0 | 6 | 0 | 0 |
| takes | 4 | 2 | 0 | 2 | 0 |
| timeline | 3 | 0 | 3 | 0 | 0 |

## 逐 operation 对齐（按领域）

| TS operation | 领域 | Bucket |
|---------------|------|--------|
| `add_link` | links/graph | WRAP |
| `add_tag` | tags | WRAP |
| `add_timeline_entry` | timeline | WRAP |
| `anomalies` | commands/misc | COMMAND_ELSEWHERE |
| `backlinks` | links/graph | WRAP |
| `cancel_job` | jobs/minions | WRAP |
| `code_blast` | code-intel | WRAP |
| `code_callees` | code-intel | WRAP |
| `code_callers` | code-intel | WRAP |
| `code_def` | code-intel | WRAP |
| `code_flow` | code-intel | WRAP |
| `code_refs` | code-intel | WRAP |
| `code_traversal_cache_clear` | code-intel | WRAP |
| `delete` | page | WRAP |
| `delete_page` | page | PORTED |
| `extract_facts` | facts | WRAP |
| `file_list` | files/attachments | WRAP |
| `file_upload` | files/attachments | NET_NEW |
| `file_url` | files/attachments | WRAP |
| `find_anomalies` | anomalies | WRAP |
| `find_contradictions` | facts | WRAP |
| `find_experts` | commands/misc | WRAP |
| `find_orphans` | page | WRAP |
| `find_trajectory` | commands/misc | WRAP |
| `forget_fact` | facts | WRAP |
| `get` | page | WRAP |
| `get_active_schema_pack` | schema-pack | WRAP |
| `get_backlinks` | links/graph | WRAP |
| `get_brain_identity` | commands/misc | NET_NEW |
| `get_calibration_profile` | files/attachments | WRAP |
| `get_chunks` | files/attachments | WRAP |
| `get_health` | health/stats | WRAP |
| `get_ingest_log` | ingestion | WRAP |
| `get_job` | jobs/minions | WRAP |
| `get_job_progress` | jobs/minions | WRAP |
| `get_links` | links/graph | WRAP |
| `get_page` | page | PORTED |
| `get_raw_data` | page | WRAP |
| `get_recent_salience` | health/stats | WRAP |
| `get_recent_transcripts` | ingestion | WRAP |
| `get_stats` | health/stats | WRAP |
| `get_tags` | tags | WRAP |
| `get_timeline` | timeline | WRAP |
| `get_versions` | page | WRAP |
| `graph` | links/graph | WRAP |
| `health` | health/stats | WRAP |
| `history` | commands/misc | WRAP |
| `link` | links/graph | WRAP |
| `list` | page | WRAP |
| `list_jobs` | jobs/minions | WRAP |
| `list_pages` | page | PORTED |
| `list_schema_packs` | schema-pack | WRAP |
| `log_ingest` | ingestion | WRAP |
| `orphans` | commands/misc | COMMAND_ELSEWHERE |
| `pause_job` | jobs/minions | WRAP |
| `purge_deleted_pages` | page | PORTED |
| `put` | page | WRAP |
| `put_page` | page | PORTED |
| `put_raw_data` | page | WRAP |
| `query` | search/query | PORTED |
| `recall` | commands/misc | COMMAND_ELSEWHERE |
| `reload_schema_pack` | schema-pack | WRAP |
| `remove_link` | links/graph | WRAP |
| `remove_tag` | tags | WRAP |
| `replay_job` | jobs/minions | WRAP |
| `resolve_slugs` | page | WRAP |
| `restore` | page | WRAP |
| `restore_page` | page | PORTED |
| `resume_job` | jobs/minions | WRAP |
| `retry_job` | jobs/minions | WRAP |
| `revert` | page | WRAP |
| `revert_version` | page | WRAP |
| `run_doctor` | commands/misc | WRAP |
| `salience` | health/stats | WRAP |
| `schema_apply_mutations` | schema-pack | WRAP |
| `schema_explain_type` | schema-pack | WRAP |
| `schema_graph` | schema-pack | WRAP |
| `schema_lint` | schema-pack | WRAP |
| `schema_review_orphans` | schema-pack | WRAP |
| `schema_stats` | schema-pack | WRAP |
| `search` | search/query | WRAP |
| `search_by_image` | search/query | NET_NEW |
| `send_job_message` | jobs/minions | WRAP |
| `sources_add` | sources | WRAP |
| `sources_list` | sources | WRAP |
| `sources_remove` | sources | WRAP |
| `sources_status` | sources | WRAP |
| `stats` | health/stats | WRAP |
| `subagent` | jobs/minions | WRAP |
| `submit_agent` | jobs/minions | WRAP |
| `submit_job` | jobs/minions | WRAP |
| `sync` | commands/misc | COMMAND_ELSEWHERE |
| `sync_brain` | commands/misc | WRAP |
| `tag` | tags | WRAP |
| `tags` | tags | WRAP |
| `takes_calibration` | takes | COMMAND_ELSEWHERE |
| `takes_list` | takes | PORTED |
| `takes_scorecard` | takes | COMMAND_ELSEWHERE |
| `takes_search` | takes | PORTED |
| `think` | commands/misc | PORTED |
| `timeline` | timeline | WRAP |
| `transcripts` | ingestion | WRAP |
| `traverse_graph` | links/graph | WRAP |
| `unlink` | links/graph | WRAP |
| `untag` | tags | WRAP |
| `whoami` | commands/misc | WRAP |
| `whoknows` | commands/misc | COMMAND_ELSEWHERE |

## 备注 / 风险

- `COMMAND_ELSEWHERE` 的 op 在 TS 里可能同时被 `cli.ts` 和 `operations.ts` 双注册；
  迁移时确认 Rust CLI 已覆盖后，从 `operations.ts` 摘除即可（无需新建 Operation）。
- `NET_NEW` 中 `file_upload` 涉及 `validateUploadPath` 信任边界校验器，port 时必须保留
  `OperationContext::remote_mcp` 的围闭语义（Rust 已有该字段，验证器本身待补）。
- `takes_calibration` / `takes_scorecard` 依赖 calibration Phase 2（roadmap 1-3-3，当前 blocked），
  属 `COMMAND_ELSEWHERE` 但实际需等 1-3-3 解锁。
- 本审计为领域级结论；每个子切片（1-6-7-x）开工时再做该领域的逐 op 精确核对。
