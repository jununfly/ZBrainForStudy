"""
zj-roadmap-driven — 路线图核心数据模型

确定性操作：所有方法都是纯函数，输入确定则输出确定。
JSON 是唯一真相源，Markdown 只是渲染视图。
"""

import json
import os
import shutil
import tempfile
import time
from datetime import datetime
from contextlib import contextmanager
from typing import Optional, Any

# ── 状态常量 ──────────────────────────────────────────────
STATUS_PENDING = "pending"
STATUS_IN_PROGRESS = "in_progress"
STATUS_COMPLETED = "completed"
STATUS_BLOCKED = "blocked"

STATUS_ICONS = {
    STATUS_PENDING: "[ ]",
    STATUS_IN_PROGRESS: "[~]",
    STATUS_COMPLETED: "[x]",
    STATUS_BLOCKED: "[!]",
}

MODE_EXPLORE = "explore"
MODE_EXPLOIT = "exploit"

MODE_TAG = {
    MODE_EXPLORE: "[X+]",
    MODE_EXPLOIT: "[Y+]",
}

DEFAULT_LOCK_TIMEOUT_SECONDS = 10.0
LOCK_RETRY_INTERVAL_SECONDS = 0.05

# ── 节点 ID 生成 ──────────────────────────────────────────

def gen_child_id(parent_id: str, index: int) -> str:
    """从父节点 id 生成子节点 id。
    "1" + 1 → "1-1", "1-1" + 2 → "1-1-2"
    """
    if parent_id == "":
        return str(index)
    return f"{parent_id}-{index}"

def next_child_index(roadmap: dict, parent_id: str) -> int:
    """计算父节点下下一个子节点的序号。"""
    parent = roadmap["nodes"].get(parent_id)
    if not parent or not parent["children"]:
        return 1
    # 从最后一个 child id 提取序号
    last = parent["children"][-1]
    parts = last.split("-")
    return int(parts[-1]) + 1

def node_depth(node_id: str) -> int:
    """节点深度。1 → 1, 1-1 → 2, 1-1-1 → 3"""
    return node_id.count("-") + 1

def parent_id_of(node_id: str) -> Optional[str]:
    """获取父节点 id。1-1 → 1, 1 → None"""
    parts = node_id.rsplit("-", 1)
    if len(parts) == 1:
        return None
    return parts[0]

def _fsync_dir_best_effort(path: str):
    """Best-effort directory fsync for atomic rename durability."""
    if not hasattr(os, "O_DIRECTORY"):
        return
    try:
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    except OSError:
        return
    try:
        os.fsync(fd)
    except OSError:
        pass
    finally:
        os.close(fd)

def atomic_write_text(path: str, content: str):
    """Atomically write text: temp file, fsync, replace, best-effort dir fsync."""
    abs_path = os.path.abspath(path)
    directory = os.path.dirname(abs_path)
    os.makedirs(directory, exist_ok=True)
    fd, tmp_path = tempfile.mkstemp(
        prefix=f".{os.path.basename(abs_path)}.",
        suffix=".tmp",
        dir=directory,
        text=True,
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(content)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp_path, abs_path)
        _fsync_dir_best_effort(directory)
    except Exception:
        try:
            os.unlink(tmp_path)
        except FileNotFoundError:
            pass
        raise

class RoadmapLockTimeout(TimeoutError):
    """Raised when a roadmap lock cannot be acquired before timeout."""

    def __init__(self, lock_dir: str, owner: dict, timeout_seconds: float):
        self.lock_dir = lock_dir
        self.owner = owner
        self.timeout_seconds = timeout_seconds
        owner_text = json.dumps(owner, ensure_ascii=False, indent=2) if owner else "(unavailable)"
        super().__init__(
            "Roadmap file is locked; retry later or avoid parallel roadmap mutations.\n"
            f"Lock path: {lock_dir}\n"
            f"Owner: {owner_text}\n"
            f"Timed out after {timeout_seconds:g}s. "
            f"If no roadmap_cli process is writing, run: python roadmap_cli.py unlock <json_path>"
        )

def roadmap_lock_dir(json_path: str) -> str:
    return os.path.abspath(json_path) + ".lock"

def read_lock_owner(json_path: str) -> dict:
    owner_path = os.path.join(roadmap_lock_dir(json_path), "owner.json")
    try:
        with open(owner_path, "r", encoding="utf-8") as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return {}

def unlock_roadmap(json_path: str) -> str:
    lock_dir = roadmap_lock_dir(json_path)
    if not os.path.isdir(lock_dir):
        return lock_dir
    shutil.rmtree(lock_dir)
    return lock_dir

@contextmanager
def roadmap_file_lock(json_path: str, timeout_seconds: float = DEFAULT_LOCK_TIMEOUT_SECONDS):
    """Cross-platform per-roadmap lock based on atomic directory creation."""
    lock_dir = roadmap_lock_dir(json_path)
    deadline = time.monotonic() + timeout_seconds
    acquired = False
    while not acquired:
        try:
            os.mkdir(lock_dir)
            acquired = True
        except FileExistsError:
            if time.monotonic() >= deadline:
                raise RoadmapLockTimeout(lock_dir, read_lock_owner(json_path), timeout_seconds)
            time.sleep(LOCK_RETRY_INTERVAL_SECONDS)

    owner = {
        "pid": os.getpid(),
        "created_at": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "roadmap_path": os.path.abspath(json_path),
    }
    try:
        atomic_write_text(
            os.path.join(lock_dir, "owner.json"),
            json.dumps(owner, ensure_ascii=False, indent=2),
        )
        yield
    finally:
        try:
            os.unlink(os.path.join(lock_dir, "owner.json"))
        except FileNotFoundError:
            pass
        try:
            os.rmdir(lock_dir)
        except FileNotFoundError:
            pass

# ── Roadmap 类 ────────────────────────────────────────────

class Roadmap:
    """路线图核心类。"""

    def __init__(self, json_path: str):
        self.json_path = os.path.abspath(json_path)
        self.data: dict = {}

    # ── 文件 I/O ───────────────────────────────────────

    def load(self) -> dict:
        """从 JSON 文件加载路线图数据。"""
        if not os.path.exists(self.json_path):
            raise FileNotFoundError(f"路线图文件不存在: {self.json_path}")
        with open(self.json_path, "r", encoding="utf-8") as f:
            self.data = json.load(f)
        return self.data

    def save(self) -> str:
        """保存路线图数据到 JSON 文件，自动更新 metadata.updated。"""
        self.data.setdefault("metadata", {})
        self.data["metadata"]["updated"] = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        content = json.dumps(self.data, ensure_ascii=False, indent=2)
        atomic_write_text(self.json_path, content)
        return self.json_path

    # ── 初始化 ─────────────────────────────────────────

    def init(self, title: str, description: str = "", md_file: str = "") -> dict:
        """创建空路线图，带一个 root 节点。"""
        now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        self.data = {
            "title": title,
            "description": description,
            "version": 1,
            "nodes": {
                "1": {
                    "id": "1",
                    "label": title,
                    "status": STATUS_IN_PROGRESS,
                    "mode": MODE_EXPLORE,
                    "parent": None,
                    "children": [],
                    "decisions": [],
                    "notes": "",
                }
            },
            "metadata": {
                "created": now,
                "updated": now,
                "md_file": md_file,
            },
        }
        return self.data

    # ── 节点 CRUD ──────────────────────────────────────

    def add_node(
        self, parent_id: str, label: str, status: str = STATUS_PENDING, mode: str = MODE_EXPLORE
    ) -> dict:
        """在父节点下添加子节点。返回新节点。"""
        if parent_id not in self.data["nodes"]:
            raise KeyError(f"父节点不存在: {parent_id}")

        index = next_child_index(self.data, parent_id)
        node_id = gen_child_id(parent_id, index)

        node = {
            "id": node_id,
            "label": label,
            "status": status,
            "mode": mode,
            "parent": parent_id,
            "children": [],
            "decisions": [],
            "notes": "",
        }

        self.data["nodes"][node_id] = node
        self.data["nodes"][parent_id]["children"].append(node_id)

        self._sync_parent_status(node_id)

        return node

    def update_node(
        self,
        node_id: str,
        label: Optional[str] = None,
        status: Optional[str] = None,
        mode: Optional[str] = None,
        notes: Optional[str] = None,
    ) -> dict:
        """更新节点的属性。只更新传入的非 None 字段。"""
        if node_id not in self.data["nodes"]:
            raise KeyError(f"节点不存在: {node_id}")

        node = self.data["nodes"][node_id]
        if label is not None:
            node["label"] = label
        if status is not None:
            if status not in (STATUS_PENDING, STATUS_IN_PROGRESS, STATUS_COMPLETED, STATUS_BLOCKED):
                raise ValueError(f"无效状态: {status}")
            node["status"] = status
        if mode is not None:
            if mode not in (MODE_EXPLORE, MODE_EXPLOIT):
                raise ValueError(f"无效模式: {mode}")
            node["mode"] = mode
        if notes is not None:
            node["notes"] = notes

        self._sync_parent_status(node_id)

        return node

    def delete_node(self, node_id: str) -> list[str]:
        """删除节点及其所有子节点。返回被删除的 id 列表。"""
        if node_id not in self.data["nodes"]:
            raise KeyError(f"节点不存在: {node_id}")
        if node_id == "1":
            raise ValueError("不能删除根节点")

        # 递归收集所有子孙节点
        deleted = []

        def _collect(nid):
            node = self.data["nodes"].get(nid)
            if not node:
                return
            for cid in list(node["children"]):
                _collect(cid)
            deleted.append(nid)

        _collect(node_id)

        # 从父节点的 children 中移除
        parent_id = self.data["nodes"][node_id]["parent"]
        if parent_id and parent_id in self.data["nodes"]:
            self.data["nodes"][parent_id]["children"].remove(node_id)

        # 删除节点
        for nid in deleted:
            del self.data["nodes"][nid]

        self._sync_parent_status(node_id)

        return deleted

    def get_node(self, node_id: str) -> dict:
        """获取节点。"""
        if node_id not in self.data["nodes"]:
            raise KeyError(f"节点不存在: {node_id}")
        return self.data["nodes"][node_id]

    # ── 决策 ───────────────────────────────────────────

    def add_decision(self, node_id: str, question: str, answer: str, note: str = "") -> dict:
        """为节点添加决策记录。"""
        node = self.get_node(node_id)
        decision = {"q": question, "answer": answer, "note": note}
        node["decisions"].append(decision)
        return decision

    def get_decisions(self, node_id: Optional[str] = None) -> list:
        """获取决策记录。无 node_id 则返回全部。"""
        if node_id:
            return self.get_node(node_id)["decisions"]
        result = []
        for nid, node in self.data["nodes"].items():
            for d in node["decisions"]:
                result.append({"node_id": nid, "node_label": node["label"], **d})
        return result

    # ── 树遍历 ─────────────────────────────────────────

    def get_tree(self, root_id: str = "1", max_depth: int = 10) -> str:
        """生成 Unicode 盒状树形文本视图。"""
        if root_id not in self.data["nodes"]:
            return f"(节点 {root_id} 不存在)"

        lines = []

        def _render(nid: str, prefix: str, is_last: bool, depth: int):
            if depth > max_depth:
                return
            node = self.data["nodes"].get(nid)
            if not node:
                return

            icon = STATUS_ICONS.get(node["status"], "[?]")
            mode_tag = MODE_TAG.get(node.get("mode"), "")
            connector = "└── " if is_last else "├── "
            line = f"{prefix}{connector}{icon}{mode_tag} {nid}. {node['label']}"
            lines.append(line)

            children = node.get("children", [])
            for i, cid in enumerate(children):
                child_is_last = (i == len(children) - 1)
                child_prefix = prefix + ("    " if is_last else "│   ")
                _render(cid, child_prefix, child_is_last, depth + 1)

        if root_id in self.data["nodes"]:
            root = self.data["nodes"][root_id]
            icon = STATUS_ICONS.get(root["status"], "[?]")
            mode_tag = MODE_TAG.get(root.get("mode"), "")
            lines.append(f"{icon}{mode_tag} {root_id}. {root['label']}")
            children = root.get("children", [])
            for i, cid in enumerate(children):
                child_is_last = (i == len(children) - 1)
                _render(cid, "", child_is_last, 1)

        return "\n".join(lines)

    def get_path(self, node_id: str) -> list[str]:
        """获取从根到目标节点的路径（id 列表）。"""
        path = []
        current = node_id
        while current:
            path.insert(0, current)
            current = self.data["nodes"][current]["parent"]
        return path

    def get_siblings(self, node_id: str) -> list[str]:
        """获取兄弟节点 id 列表（不含自身）。"""
        node = self.get_node(node_id)
        parent_id = node["parent"]
        if not parent_id:
            return []
        parent = self.data["nodes"][parent_id]
        return [cid for cid in parent["children"] if cid != node_id]

    def get_current_focus(self) -> Optional[str]:
        """找到最深的 in_progress 节点作为当前施工点。"""
        candidates = [
            nid for nid, node in self.data["nodes"].items()
            if node["status"] == STATUS_IN_PROGRESS
        ]
        if not candidates:
            return None
        return max(candidates, key=node_depth)

    def _sync_parent_status(self, node_id: str):
        """自底向上级联同步父节点状态。

        规则：
        - 全部子节点 completed → 父节点 = completed
        - 任一子节点非 completed → 父节点 ≠ completed（降为 in_progress）
        """
        current = self.data["nodes"].get(node_id)
        if not current:
            return
        parent_id = current.get("parent")
        while parent_id and parent_id in self.data["nodes"]:
            parent = self.data["nodes"][parent_id]
            children = parent.get("children", [])
            if not children:
                break
            all_done = all(
                self.data["nodes"][cid]["status"] == STATUS_COMPLETED
                for cid in children if cid in self.data["nodes"]
            )
            if all_done:
                if parent["status"] != STATUS_COMPLETED:
                    parent["status"] = STATUS_COMPLETED
            else:
                if parent["status"] == STATUS_COMPLETED:
                    parent["status"] = STATUS_IN_PROGRESS
            parent_id = parent.get("parent")

    # ── Markdown rendering ──────────────────────────────────

    def link_md_file(self, md_path: str):
        """关联 Markdown 文件路径。"""
        self.data["md_file"] = md_path

    def _build_status_summary(self) -> dict:
        """统计各状态节点数量。"""
        counts = {
            STATUS_PENDING: 0,
            STATUS_IN_PROGRESS: 0,
            STATUS_COMPLETED: 0,
            STATUS_BLOCKED: 0,
        }
        for node in self.data["nodes"].values():
            s = node.get("status", STATUS_PENDING)
            if s in counts:
                counts[s] += 1
        return counts

    def _build_rendered_tree(self, root_id: str = "1", max_depth: int = 2) -> str:
        """渲染树形文本视图到字符串。"""
        return self.get_tree(root_id, max_depth)

    def _build_focus_section(self) -> str:
        """构建当前焦点节点详情的 Markdown 文本。"""
        focus_id = self.get_current_focus()
        if not focus_id:
            return "_No in-progress node._"

        node = self.get_node(focus_id)
        lines = [f"### 🔨 当前施工: {focus_id}. {node['label']}"]
        lines.append(f"**Status:** `{node['status']}` | **Mode:** `{node.get('mode', 'explore')}`")
        if node.get("notes"):
            lines.append(f"\n{node['notes']}")

        decisions = node.get("decisions", [])
        if decisions:
            lines.append("\n**决策记录:**")
            for d in decisions:
                lines.append(f"- Q: {d['q']}")
                lines.append(f"  A: {d['answer']}")
                if d.get("note"):
                    lines.append(f"  > {d['note']}")

        # 焦点子节点浅展开一层
        children = node.get("children", [])
        if children:
            lines.append("\n**子节点:**")
            for cid in children[:10]:
                cnode = self.get_node(cid)
                icon = STATUS_ICONS.get(cnode.get("status", STATUS_PENDING), "[?]")
                lines.append(f"- {icon} {cid}. {cnode['label']}")

        return "\n".join(lines)

    def render_full_section(self) -> str:
        """生成完整 Markdown section（stdout 调试用）。"""
        title = self.data.get("title", "Roadmap")
        desc = self.data.get("description", "")

        parts = [f"## {title}"]
        if desc:
            parts.append(f"\n{desc}\n")

        # 全量树形
        parts.append("### 完整树形\n")
        parts.append("```")
        parts.append(self._build_rendered_tree(max_depth=10))
        parts.append("```")

        # 统计
        counts = self._build_status_summary()
        parts.append(f"\n### 统计: pending={counts[STATUS_PENDING]} in_progress={counts[STATUS_IN_PROGRESS]} completed={counts[STATUS_COMPLETED]} blocked={counts[STATUS_BLOCKED]}")

        # 焦点详情
        parts.append(f"\n{self._build_focus_section()}")

        # 全局决策
        decisions = self.get_decisions()
        if decisions:
            parts.append("\n### 全局决策\n")
            for d in decisions:
                parts.append(f"- **{d['node_id']}**: Q: {d['q']} → A: {d['answer']}")

        return "\n\n".join(parts)

    def write_markdown_section(self) -> Optional[str]:
        """渲染轻量 Markdown section 并写入关联 md 文件。

        Returns 写入的文件路径，或 None（若未 link）。
        """
        md_file = self.data.get("md_file")
        if not md_file:
            return None

        # 确保路径解析正确
        if not os.path.isabs(md_file):
            md_file = os.path.join(os.path.dirname(self.json_path), md_file)

        title = self.data.get("title", "Roadmap")

        parts = [
            "<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->",
            f"## {title}",
            "",
            "### 树形视图 (depth=2)",
            "",
            "```",
            self._build_rendered_tree(max_depth=2),
            "```",
            "",
            self._build_focus_section(),
        ]

        section_text = "\n".join(parts)

        # 读取现有文件，替换或追加
        marker = "<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成"
        file_start = "<!-- ⚠️ ROADMAP_SECTION_START -->"
        file_end = "<!-- ⚠️ ROADMAP_SECTION_END -->"

        try:
            with open(md_file, "r", encoding="utf-8") as f:
                existing = f.read()
        except FileNotFoundError:
            existing = ""

        wrapped = f"{file_start}\n{section_text}\n{file_end}"

        if file_start in existing and file_end in existing:
            # 替换现有 section
            start_idx = existing.index(file_start)
            end_idx = existing.index(file_end) + len(file_end)
            new_content = existing[:start_idx] + wrapped + existing[end_idx:]
        else:
            # 追加到末尾
            if existing and not existing.endswith("\n"):
                existing += "\n"
            new_content = existing + "\n" + wrapped + "\n"

        with open(md_file, "w", encoding="utf-8") as f:
            f.write(new_content)

        return os.path.abspath(md_file)

    # ── Validation & stats ──────────────────────────────────

    def stats(self) -> dict:
        """返回路线图统计信息。"""
        counts = self._build_status_summary()
        total = sum(counts.values())
        return {
            "total_nodes": total,
            "pending": counts[STATUS_PENDING],
            "in_progress": counts[STATUS_IN_PROGRESS],
            "completed": counts[STATUS_COMPLETED],
            "blocked": counts[STATUS_BLOCKED],
            "title": self.data.get("title", ""),
        }

    def validate(self) -> list[str]:
        """验证数据完整性，返回错误列表。"""
        errors = []
        nodes = self.data.get("nodes", {})

        for nid, node in nodes.items():
            # 检查必填字段
            for field in ("id", "label", "status", "mode"):
                if field not in node:
                    errors.append(f"Node '{nid}' missing field '{field}'")

            # 检查状态合法性
            status = node.get("status", "")
            if status not in (STATUS_PENDING, STATUS_IN_PROGRESS, STATUS_COMPLETED, STATUS_BLOCKED):
                errors.append(f"Node '{nid}' invalid status: '{status}'")

            # 检查父节点存在性
            parent = node.get("parent")
            if parent and parent not in nodes:
                errors.append(f"Node '{nid}' parent '{parent}' not found")

            # 检查子节点存在性
            for cid in node.get("children", []):
                if cid not in nodes:
                    errors.append(f"Node '{nid}' child '{cid}' not found")

            # 检查 id 一致性
            if node.get("id") != nid:
                errors.append(f"Node key '{nid}' != id field '{node.get('id')}'")

        return errors