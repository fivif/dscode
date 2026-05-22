"""PRDGenerator —— 根据 grill-me 访谈生成 PRD + 两份 ContextManifest。

输出物（落盘）：
    .dscode/tasks/<task_id>/
        prd.md            # 人类可读 PRD（含 frontmatter）
        prd.json          # 机器可读 PRDDocument 序列化（便于后续 MAGI 加载）
        implement.jsonl   # implement 阶段上下文清单（每行一个 JSON 对象）
        check.jsonl       # check 阶段上下文清单
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from dscode.core.types import ContextManifest, LLMProviderProtocol, Message, PRDDocument
from dscode.deepseek.client import DeepSeekClient
from dscode.deepseek.prefix_completion import force_json

_SYSTEM_PROMPT = """\
你是一名资深工程师，把用户的需求 + grill-me 访谈结果 + 项目规范，
合成一份**结构化 PRD** 和**两份上下文清单**（implement / check）。

你的输出必须是严格 JSON（不要 markdown / 不要解释），包含三个顶层字段：

{
  "prd": {
    "task_description": "一句话任务描述",
    "goals":              ["..."],   // 必填，至少 1 条
    "constraints":        ["..."],   // 必填，可以为空数组
    "acceptance_criteria":["..."],   // 必填，至少 1 条
    "related_files":      ["..."],   // 涉及的项目内文件路径，可为空数组
    "estimated_hours":    1.0,        // 数值，小 bug ~1, 模块重构 ~3, 新功能 ~5, 架构 ~8
    "risk_notes":         ["..."]
  },
  "implement_manifest": {
    "files":     ["..."],            // 实现阶段必读 / 必改文件
    "snippets":  [{"path": "...", "reason": "..."}],
    "related_facts": ["..."]
  },
  "check_manifest": {
    "files":     ["..."],            // 验收阶段必读测试/校验文件
    "snippets":  [{"path": "...", "reason": "..."}],
    "related_facts": ["..."]
  }
}

约束：
1. 严格遵守已有 spec（命名 / 安全 / 测试规范）。
2. acceptance_criteria 必须可被自动化验证（例如"pytest 通过"）。
3. implement_manifest.files 与 check_manifest.files 应当尽量不重复。
4. 字段不可缺；列表至少为 [] 不可为 null。
"""


class PRDGenerator:
    """PRD + 上下文清单生成器。"""

    def __init__(
        self,
        llm: LLMProviderProtocol,
        model: str = "deepseek-v4-flash",
    ) -> None:
        self.llm = llm
        self.model = model

    # ------------------------------------------------------------
    # 公共 API
    # ------------------------------------------------------------

    async def generate(
        self,
        task_description: str,
        interview_qa: list[tuple[str, str]],
        spec_text: str,
        related_files: list[str] | tuple[str, ...] = (),
    ) -> tuple[PRDDocument, ContextManifest, ContextManifest]:
        """让 LLM 合成 PRDDocument + 两份 ContextManifest。

        Args:
            task_description: 原始任务描述。
            interview_qa: GrillMe 输出的 [(question, answer), ...]
            spec_text: SpecLoader.format_for_prompt() 的注入文本。
            related_files: 调用方已知的候选相关文件（hint，可空）。

        Returns:
            (PRDDocument, implement_manifest, check_manifest)
        """
        qa_block = _format_qa(interview_qa)
        hint_block = "\n".join(f"- {p}" for p in related_files) if related_files else "(无)"

        user_prompt = (
            f"# 任务\n{task_description}\n\n"
            f"# 项目规范\n{(spec_text or '(暂无)').strip()}\n\n"
            f"# grill-me 访谈结果\n{qa_block}\n\n"
            f"# 调用方提供的候选相关文件\n{hint_block}\n\n"
            "请基于以上信息输出 JSON（结构见 system prompt）。"
        )

        payload = await self._chat_json(user_prompt)
        prd = _build_prd(task_description, payload.get("prd") or {})
        impl = _build_manifest("implement", payload.get("implement_manifest") or {})
        chk = _build_manifest("check", payload.get("check_manifest") or {})
        return prd, impl, chk

    def write_to_disk(
        self,
        prd: PRDDocument,
        implement: ContextManifest,
        check: ContextManifest,
        project_root: Path | str,
    ) -> Path:
        """落盘到 .dscode/tasks/<task_id>/。

        Returns:
            task 目录绝对路径。
        """
        project_root = Path(project_root)
        task_dir = project_root / ".dscode" / "tasks" / prd.task_id
        task_dir.mkdir(parents=True, exist_ok=True)

        # 1) prd.md —— 人类可读
        (task_dir / "prd.md").write_text(_render_prd_markdown(prd), encoding="utf-8")
        # 2) prd.json —— 机器可读（MAGI 主循环加载用）
        (task_dir / "prd.json").write_text(
            prd.model_dump_json(indent=2),
            encoding="utf-8",
        )
        # 3) implement.jsonl
        (task_dir / "implement.jsonl").write_text(
            _render_manifest_jsonl(implement),
            encoding="utf-8",
        )
        # 4) check.jsonl
        (task_dir / "check.jsonl").write_text(
            _render_manifest_jsonl(check),
            encoding="utf-8",
        )
        return task_dir

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    async def _chat_json(self, user_prompt: str) -> dict[str, Any]:
        """调 LLM 拿严格 JSON。优先 force_json，失败回退普通 chat。"""
        if isinstance(self.llm, DeepSeekClient):
            return await force_json(
                client=self.llm,
                schema_hint='{"prd": {...}, "implement_manifest": {...}, "check_manifest": {...}}',
                model=self.model,
                user_prompt=user_prompt,
                system_prompt=_SYSTEM_PROMPT,
                max_tokens=2048,
            )
        # 非 DeepSeek provider 路径
        messages = [
            Message(role="system", content=_SYSTEM_PROMPT),
            Message(role="user", content=user_prompt),
        ]
        resp = await self.llm.chat(messages=messages, model=self.model)
        return _parse_json_or_raise(resp.content or "")


# ============================================================
# 渲染 / 解析辅助
# ============================================================

def _format_qa(qa: list[tuple[str, str]]) -> str:
    if not qa:
        return "(无访谈记录)"
    lines: list[str] = []
    for i, (q, a) in enumerate(qa, 1):
        lines.append(f"Q{i}: {q}\nA{i}: {a}")
    return "\n\n".join(lines)


def _build_prd(default_task: str, raw: dict[str, Any]) -> PRDDocument:
    """从 LLM 返回的 dict 构造 PRDDocument，做防御性默认值处理。"""
    task_description = str(raw.get("task_description") or default_task).strip() or default_task
    goals = _as_str_list(raw.get("goals")) or [f"完成: {task_description}"]
    constraints = _as_str_list(raw.get("constraints"))
    acceptance = _as_str_list(raw.get("acceptance_criteria")) or ["任务可被人工或自动验收"]
    related = _as_str_list(raw.get("related_files"))
    risk = _as_str_list(raw.get("risk_notes"))
    try:
        hours = float(raw.get("estimated_hours") or 1.0)
    except (TypeError, ValueError):
        hours = 1.0
    return PRDDocument(
        task_description=task_description,
        goals=goals,
        constraints=constraints,
        acceptance_criteria=acceptance,
        related_files=related,
        estimated_hours=max(0.1, hours),
        risk_notes=risk,
    )


def _build_manifest(phase: str, raw: dict[str, Any]) -> ContextManifest:
    files = _as_str_list(raw.get("files"))
    snippets_raw = raw.get("snippets") or []
    snippets: list[dict[str, Any]] = []
    if isinstance(snippets_raw, list):
        for item in snippets_raw:
            if isinstance(item, dict):
                snippets.append(item)
    facts = _as_str_list(raw.get("related_facts"))
    return ContextManifest(
        phase=phase,  # type: ignore[arg-type]
        files=files,
        snippets=snippets,
        related_facts=facts,
    )


def _as_str_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value]
    if isinstance(value, (list, tuple)):
        return [str(x) for x in value if x is not None]
    return [str(value)]


def _render_prd_markdown(prd: PRDDocument) -> str:
    """把 PRDDocument 渲染为带 frontmatter 的 markdown。"""
    fm = (
        "---\n"
        f"task_id: {prd.task_id}\n"
        f"created_at: {prd.created_at:.0f}\n"
        f"estimated_hours: {prd.estimated_hours}\n"
        "---\n"
    )
    body_lines: list[str] = [
        f"# PRD: {prd.task_description}",
        "",
        "## 目标",
    ]
    body_lines += [f"- {g}" for g in prd.goals] or ["- (无)"]
    body_lines += ["", "## 约束"]
    body_lines += [f"- {c}" for c in prd.constraints] or ["- (无)"]
    body_lines += ["", "## 验收标准"]
    body_lines += [f"- {a}" for a in prd.acceptance_criteria] or ["- (无)"]
    body_lines += ["", "## 相关文件"]
    body_lines += [f"- `{p}`" for p in prd.related_files] or ["- (无)"]
    body_lines += ["", "## 风险说明"]
    body_lines += [f"- {r}" for r in prd.risk_notes] or ["- (无)"]
    return fm + "\n".join(body_lines).rstrip() + "\n"


def _render_manifest_jsonl(manifest: ContextManifest) -> str:
    """把 ContextManifest 写为 JSONL 风格：
    第 1 行 metadata，之后每个 file / snippet / fact 各一行。
    """
    lines: list[str] = [
        json.dumps(
            {
                "type": "metadata",
                "phase": manifest.phase,
                "file_count": len(manifest.files),
                "snippet_count": len(manifest.snippets),
                "fact_count": len(manifest.related_facts),
            },
            ensure_ascii=False,
        )
    ]
    for f in manifest.files:
        lines.append(json.dumps({"type": "file", "path": f}, ensure_ascii=False))
    for snip in manifest.snippets:
        lines.append(json.dumps({"type": "snippet", **snip}, ensure_ascii=False))
    for fact in manifest.related_facts:
        lines.append(json.dumps({"type": "fact", "ref": fact}, ensure_ascii=False))
    return "\n".join(lines) + "\n"


def _parse_json_or_raise(text: str) -> dict[str, Any]:
    text = (text or "").strip()
    if not text:
        raise ValueError("empty model output")
    try:
        loaded = json.loads(text)
    except json.JSONDecodeError as e:
        # 救援：抽最外层 {...}
        import re
        match = re.search(r"\{.*\}", text, re.DOTALL)
        if not match:
            raise ValueError(f"no JSON in PRD output: {text!r}") from e
        loaded = json.loads(match.group(0))
    if not isinstance(loaded, dict):
        raise ValueError("PRD output is not a JSON object")
    return loaded


__all__ = ["PRDGenerator"]
