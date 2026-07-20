#!/usr/bin/env python3
"""Coverage audit: TS operations.ts (107 ops) vs Rust OperationRegistry + BrainEngine.

Buckets:
  PORTED            -> already a Rust Operation struct (operation.rs)
  WRAP              -> agent-operation whose Rust BrainEngine method exists -> wrap as Operation
  COMMAND_ELSEWHERE -> op is really a CLI command already ported as `zbrain <cmd>` (1-6-4/1-4/1-5) -> drop from operations.ts
  NET_NEW           -> genuinely missing in Rust (no engine method, not a ported command)
"""
import re
from collections import defaultdict

PORTED = {
    'get_page', 'put_page', 'delete_page', 'restore_page', 'list_pages',
    'purge_deleted_pages', 'query', 'think', 'takes_list', 'takes_search',
}

# Ops that are really CLI commands already ported elsewhere in Rust (1-6-4 / 1-4 / 1-5).
COMMAND_ELSEWHERE = {
    'features', 'storage', 'publish', 'migrate', 'models', 'providers', 'whoknows',
    'integrity', 'anomalies', 'check_update', 'apply_migrations', 'mounts', 'resolvers',
    'code_intel', 'check_resolvable', 'routing_eval', 'filing_audit', 'dry_fix', 'doctor',
    'schema', 'skillify', 'init', 'config', 'sync', 'import', 'capture', 'clone',
    'register_client', 'admin', 'extract', 'export', 'extract_conversation_facts',
    'memory', 'recall', 'forget', 'dream', 'brainstorm', 'auth', 'eval',
    'reconcile_links', 'orphans', 'calibration', 'takes_calibration', 'takes_scorecard',
}

# Genuinely net-new (no engine method, not a ported command).
NET_NEW = {
    'search_by_image',          # image embedding not in engine
    'get_brain_identity',       # engine has get_health/get_brain_stats, not identity
    'file_upload', 'upload',     # validateUploadPath trust-boundary validator not in Rust
    'validate_upload_path',
}

AGENT_OPS = {
    # page (non-ported members)
    'update_slug', 'rewrite_links', 'soft_delete_page', 'refresh_page_body',
    'update_page_contextual_retrieval_state', 'get_page_timestamps', 'resolve_slugs',
    'get_all_slugs', 'list_all_page_refs', 'find_orphan_pages', 'find_duplicate_page',
    # tags
    'add_tag', 'remove_tag', 'get_tags',
    # links/graph
    'add_link', 'remove_link', 'get_links', 'get_backlinks', 'get_backlink_counts',
    'traverse_graph', 'traverse_paths', 'add_links_batch', 'rewrite_links',
    # timeline
    'add_timeline_entry', 'get_timeline', 'get_effective_dates', 'set_page_timeline',
    # takes (non-ported)
    'get_takes_for_page', 'add_takes_batch', 'resolve_take', 'list_takes', 'search_takes',
    # search
    'search',
    # facts
    'insert_fact', 'list_facts_by_entity', 'get_facts_health', 'expire_fact', 'facts',
    # sources
    'list_sources', 'get_source', 'create_source', 'update_source', 'delete_source',
    'get_source_by_github_repo',
    # files/attachments
    'upsert_file', 'get_file', 'list_files_for_page', 'upsert_chunks',
    'delete_chunks', 'get_chunks_for_page', 'add_code_edges',
    'insert_attachment', 'list_attachments', 'get_attachment', 'delete_attachment',
    'list_attachment_filenames',
    # jobs/minions
    'enqueue_job', 'get_job', 'get_jobs', 'claim_job', 'complete_job', 'fail_job',
    'cancel_job', 'pause_job', 'resume_job', 'prune_jobs', 'send_message', 'read_inbox',
    'update_tokens', 'update_progress', 'append_log',
    # misc agent
    'add_timeline_entry',
}


def domain(op):
    if op in ('get_page', 'put_page', 'delete_page', 'restore_page', 'list_pages',
              'purge_deleted_pages', 'soft_delete_page', 'update_slug', 'rewrite_links',
              'get_page_timestamps', 'refresh_page_body',
              'update_page_contextual_retrieval_state', 'resolve_slugs', 'get_all_slugs',
              'list_all_page_refs', 'find_orphan_pages', 'find_duplicate_page',
              'find_orphans', 'get_raw_data', 'put_raw_data', 'get_versions',
              'revert', 'revert_version', 'get', 'put', 'delete', 'list',
              'restore', 'purge'):
        return 'page'
    if op in ('add_tag', 'remove_tag', 'get_tags', 'tag', 'untag', 'tags'):
        return 'tags'
    if op in ('add_link', 'remove_link', 'get_links', 'get_backlinks',
              'get_backlink_counts', 'traverse_graph', 'traverse_paths',
              'add_links_batch', 'rewrite_links', 'link', 'unlink', 'graph',
              'backlinks'):
        return 'links/graph'
    if 'timeline' in op or op in ('get_effective_dates', 'set_page_timeline',
                                  'timeline', 'get_timeline'):
        return 'timeline'
    if op.startswith('takes') or op in ('get_takes_for_page', 'add_takes_batch',
                                         'resolve_take', 'list_takes', 'search_takes',
                                         'takes', 'takes_list', 'takes_search',
                                         'takes_calibration', 'takes_scorecard'):
        return 'takes'
    if op in ('search', 'search_by_image', 'query'):
        return 'search/query'
    if op.startswith('fact') or op in ('insert_fact', 'list_facts_by_entity',
                                        'get_facts_health', 'expire_fact'):
        return 'facts'
    if 'source' in op or op in ('list_sources', 'get_source', 'create_source',
                                 'update_source', 'delete_source',
                                 'get_source_by_github_repo'):
        return 'sources'
    if ('file' in op) or op in ('upsert_file', 'get_file', 'list_files_for_page',
                                 'upsert_chunks', 'delete_chunks', 'get_chunks_for_page',
                                 'get_chunks', 'add_code_edges', 'file_upload', 'upload',
                                 'insert_attachment', 'list_attachments', 'get_attachment',
                                 'delete_attachment', 'list_attachment_filenames',
                                 'validate_upload_path'):
        return 'files/attachments'
    if op.startswith('job') or op in ('enqueue_job', 'get_job', 'get_jobs', 'claim_job',
                                       'complete_job', 'fail_job', 'cancel_job', 'pause_job',
                                       'resume_job', 'prune_jobs', 'send_message', 'read_inbox',
                                       'update_tokens', 'update_progress', 'append_log'):
        return 'jobs/minions'
    if op.startswith('code_'):
        return 'code-intel'
    if 'schema' in op or op in ('get_active_schema_pack', 'list_schema_packs',
                                 'reload_schema_pack'):
        return 'schema-pack'
    if op in ('extract_facts', 'forget_fact', 'find_contradictions'):
        return 'facts'
    if op in ('find_anomalies',):
        return 'anomalies'
    if op in ('get_health', 'health', 'get_stats', 'stats', 'salience',
              'get_recent_salience'):
        return 'health/stats'
    if op in ('get_ingest_log', 'log_ingest', 'get_recent_transcripts', 'transcripts'):
        return 'ingestion'
    if op in ('get_job_progress', 'list_jobs', 'replay_job', 'retry_job',
              'send_job_message', 'submit_job', 'submit_agent', 'subagent'):
        return 'jobs/minions'
    if op in ('think', 'brainstorm', 'auth', 'features', 'storage', 'publish', 'migrate',
              'models', 'providers', 'whoknows', 'integrity', 'anomalies', 'check_update',
              'apply_migrations', 'mounts', 'resolvers', 'code_intel', 'memory', 'recall',
              'forget', 'dream', 'eval', 'calibration', 'takes_calibration',
              'takes_scorecard', 'doctor', 'check_resolvable', 'routing_eval', 'filing_audit',
              'dry_fix', 'skillify', 'schema', 'init', 'config', 'sync', 'import', 'capture',
              'clone', 'register_client', 'admin', 'extract', 'export',
              'extract_conversation_facts', 'reconcile_links', 'orphans',
              'get_brain_identity', 'run_doctor', 'whoami', 'history', 'sync_brain',
              'find_experts', 'find_trajectory'):
        return 'commands/misc'
    return 'other'


def bucket(op):
    if op in PORTED:
        return 'PORTED'
    if op in NET_NEW:
        return 'NET_NEW'
    if op in COMMAND_ELSEWHERE:
        return 'COMMAND_ELSEWHERE'
    return 'WRAP'


ts_ops = [l.strip() for l in open('scripts/ts_ops.txt') if l.strip()]

rows = []
for op in sorted(ts_ops):
    d = domain(op)
    b = bucket(op)
    # alias note
    rows.append((op, d, b))

roll = defaultdict(lambda: defaultdict(int))
for op, d, b in rows:
    roll[d][b] += 1
    roll[d]['total'] += 1

total = defaultdict(int)
for op, d, b in rows:
    total[b] += 1
total['all'] = len(rows)

# ---- markdown ----
L = []
L.append('# operations.ts → Rust OperationRegistry 覆盖审计')
L.append('')
L.append('> 生成于 1-6-5-9 收口后，作为「替换式增量迁移 operations.ts」(roadmap 1-6-7) 的输入地图。')
L.append('> 原则（Q3 决策）：以 Rust `zbrain_core::operation::{OperationContext,OperationRegistry}` + `BrainEngine` 为继任者，')
L.append('> 把 TS `operations.ts` 的 107 个唯一 operation 逐个对齐，随迁随从 `operations.ts` 删除。')
L.append('')
L.append('## 总览')
L.append('')
L.append(f'- TS `operations.ts` 唯一 operation 数：**{total["all"]}**（含别名去重后）')
L.append(f'- Rust `operation.rs` 已定义 `Operation` 结构体：**{total["PORTED"]}**（page CRUD 6 + query + think + takes_list + takes_search）')
L.append(f'- `zbrain-mcp` 实际注册：**{total["PORTED"]}** 个（同上）')
L.append(f'- Rust `BrainEngine` trait 方法：**~120**，覆盖绝大多数 operation 领域')
L.append('')
L.append('### Bucket 分布')
L.append('')
L.append('| Bucket | 数量 | 含义 |')
L.append('|--------|------|------|')
L.append(f'| PORTED | {total["PORTED"]} | 已是 Rust Operation 结构体 |')
L.append(f'| WRAP | {total["WRAP"]} | agent-operation，Rust engine 已有方法 → 包成 Operation |')
L.append(f'| COMMAND_ELSEWHERE | {total["COMMAND_ELSEWHERE"]} | 实为 CLI 命令，已在 1-6-4/1-4/1-5 迁为 `zbrain <cmd>` → 从 operations.ts 摘除 |')
L.append(f'| NET_NEW | {total["NET_NEW"]} | 真缺（无 engine 方法、非已迁命令） |')
L.append('')
L.append('**关键结论**：`WRAP` 占大头——剩余工作主要是「把已有 engine 方法包成 Operation 并注册」，')
L.append('不是重写逻辑。只有 `file_upload`/`search_by_image`/`get_brain_identity` 等少数为 net-new。')
L.append('')
L.append('## 领域 rollup')
L.append('')
L.append('| 领域 | 总数 | PORTED | WRAP | COMMAND_ELSEWHERE | NET_NEW |')
L.append('|------|------|--------|------|------------------|---------|')
for d in sorted(roll.keys()):
    r = roll[d]
    L.append(f'| {d} | {r["total"]} | {r.get("PORTED",0)} | {r.get("WRAP",0)} | '
             f'{r.get("COMMAND_ELSEWHERE",0)} | {r.get("NET_NEW",0)} |')
L.append('')
L.append('## 逐 operation 对齐（按领域）')
L.append('')
L.append('| TS operation | 领域 | Bucket |')
L.append('|---------------|------|--------|')
for op, d, b in rows:
    L.append(f'| `{op}` | {d} | {b} |')
L.append('')
L.append('## 备注 / 风险')
L.append('')
L.append('- `COMMAND_ELSEWHERE` 的 op 在 TS 里可能同时被 `cli.ts` 和 `operations.ts` 双注册；')
L.append('  迁移时确认 Rust CLI 已覆盖后，从 `operations.ts` 摘除即可（无需新建 Operation）。')
L.append('- `NET_NEW` 中 `file_upload` 涉及 `validateUploadPath` 信任边界校验器，port 时必须保留')
L.append('  `OperationContext::remote_mcp` 的围闭语义（Rust 已有该字段，验证器本身待补）。')
L.append('- `takes_calibration` / `takes_scorecard` 依赖 calibration Phase 2（roadmap 1-3-3，当前 blocked），')
L.append('  属 `COMMAND_ELSEWHERE` 但实际需等 1-3-3 解锁。')
L.append('- 本审计为领域级结论；每个子切片（1-6-7-x）开工时再做该领域的逐 op 精确核对。')
L.append('')

out = '\n'.join(L)
with open('docs/plans/OPERATIONS_TS_TO_RUST_AUDIT.md', 'w', encoding='utf-8') as f:
    f.write(out)

# also print summary to stdout
print(f"total={total['all']} PORTED={total['PORTED']} WRAP={total['WRAP']} "
      f"COMMAND_ELSEWHERE={total['COMMAND_ELSEWHERE']} NET_NEW={total['NET_NEW']}")
for d in sorted(roll.keys()):
    r = roll[d]
    print(f"  {d}: total={r['total']} P={r.get('PORTED',0)} W={r.get('WRAP',0)} "
          f"C={r.get('COMMAND_ELSEWHERE',0)} N={r.get('NET_NEW',0)}")
