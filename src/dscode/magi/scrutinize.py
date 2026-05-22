"""Scrutinizer —— MAGI 第一脑（Casper），审视阶段。

职责：在新一轮开始时，根据当前 PRD + 上轮执行结果 + 项目规范 + 代码摘要，
吐出本轮要解决的关键问题清单 + 下一步建议动作 + 风险标签。

设计要点：
- 用 deepseek-v4-flash（评估类任务、便宜）。
- 用 `force_json` 强制结构化输出，对齐 `ScrutinizeOutput`。
- LLM 异常时返回保底输出（next_action = "继续推进 PRD 目标"），让循环不中断。
"""
from __future__ import annotations

from typing import Any

from dscode.core.types import (
    LLMProviderProtocol,
    MAGIRoundLog,
    Message,
    PRDDocument,
    ScrutinizeOutput,
)
from dscode.deepseek.auto_router import AutoRouter
from dscode.deepseek.client import DeepSeekClient
from dscode.deepseek.prefix_completion import force_json

_SYSTEM_PROMPT = """\
你是 MAGI-Casper（审视脑）。你的任务是在新一轮 MAGI 循环开始时审视当前状态。

输入：
- 当前 PRD（task_description / goals / acceptance_criteria / constraints）
- 上一轮的执行结果与提升评分（可能为空，表示这是第一轮）
- 项目规范（spec）
- 代码摘要（codebase_summary）

工作方式：
1. 找出"未验证假设"、"缺陷"、"矛盾"、"未覆盖的验收点"。
2. 提出 3~7 个最关键的问题（grill-me 风，问题要具体、可验证）。
3. 推荐**唯一的下一步动作**（next_action），描述要具体到"用哪个工具改哪个文件做什么"。
4. 列出明确的风险标签（safety / regression / performance / unknown）。

严格输出 JSON：
{
  "questions": ["..."],          // 3~7 条
  "next_action": "一句话动作",
  "risk_flags": ["..."]
}
"""


class Scrutinizer:
    """MAGI Scrutinize 阶段实现。"""

    def __init__(
        self,
        llm: LLMProviderProtocol,
        model: str = "deepseek-v4-flash",
        auto_router: AutoRouter | None = None,
    ) -> None:
        self.llm = llm
        self.model = model
        self.auto_router = auto_router

    async def scrutinize(
        self,
        prd: PRDDocument,
        previous_round: MAGIRoundLog | None,
        spec_text: str,
        codebase_summary: str = "",
    ) -> ScrutinizeOutput:
        """跑一次审视。任何异常都返回安全默认值，绝不让主循环崩溃。"""
        user_prompt = self._build_user_prompt(
            prd=prd,
            previous_round=previous_round,
            spec_text=spec_text,
            codebase_summary=codebase_summary,
        )

        # —— Auto 路由：先决定本次调用用什么模型/thinking/effort ——
        model, thinking_kwargs = await self._resolve_route(
            task_text=prd.task_description,
            context_summary=spec_text[:500] if spec_text else "",
        )

        try:
            payload = await self._chat_json(user_prompt, model=model, thinking_kwargs=thinking_kwargs)
        except Exception as exc:
            return ScrutinizeOutput(
                questions=[
                    "当前未能完成审视调用——上轮成果是否已经达成 PRD？",
                    "是否需要回退到上一个 snapshot 再继续？",
                ],
                next_action="继续推进 PRD 目标；先读最近事件再决定具体操作。",
                risk_flags=["scrutinizer_error", f"reason:{type(exc).__name__}"],
            )

        return _build_output(payload)

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    async def _resolve_route(
        self,
        task_text: str,
        context_summary: str,
    ) -> tuple[str, dict[str, Any]]:
        """如有 auto_router，调路由决定模型/thinking/effort；失败回退默认。"""
        if self.auto_router is None:
            return self.model, {}
        try:
            decision = await self.auto_router.route(
                task=task_text,
                context_summary=context_summary,
            )
        except Exception:
            return self.model, {}
        return decision.recommended_model, {
            "thinking": decision.thinking,
            "reasoning_effort": decision.reasoning_effort,
        }

    async def _chat_json(
        self,
        user_prompt: str,
        model: str | None = None,
        thinking_kwargs: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        eff_model = model or self.model
        extra = thinking_kwargs or {}
        if isinstance(self.llm, DeepSeekClient):
            return await force_json(
                client=self.llm,
                schema_hint='{"questions": [str], "next_action": str, "risk_flags": [str]}',
                model=eff_model,
                user_prompt=user_prompt,
                system_prompt=_SYSTEM_PROMPT,
                max_tokens=1024,
            )
        messages = [
            Message(role="system", content=_SYSTEM_PROMPT),
            Message(role="user", content=user_prompt),
        ]
        resp = await self.llm.chat(messages=messages, model=eff_model, **extra)
        return _loose_parse_json(resp.content or "")

    def _build_user_prompt(
        self,
        prd: PRDDocument,
        previous_round: MAGIRoundLog | None,
        spec_text: str,
        codebase_summary: str,
    ) -> str:
        prev_block = _format_previous_round(previous_round)
        return (
            f"# 当前 PRD\n{_format_prd(prd)}\n\n"
            f"# 项目规范\n{(spec_text or '(无)').strip()}\n\n"
            f"# 代码摘要\n{(codebase_summary or '(无)').strip()}\n\n"
            f"# 上一轮结果\n{prev_block}\n\n"
            "请基于以上信息输出审视 JSON。"
        )


# ============================================================
# 辅助
# ============================================================

def _format_prd(prd: PRDDocument) -> str:
    lines = [
        f"task: {prd.task_description}",
        f"goals: {prd.goals}",
        f"constraints: {prd.constraints}",
        f"acceptance_criteria: {prd.acceptance_criteria}",
        f"related_files: {prd.related_files}",
    ]
    return "\n".join(lines)


def _format_previous_round(prev: MAGIRoundLog | None) -> str:
    if prev is None:
        return "(无 —— 这是第一轮)"
    parts = [f"round_number: {prev.round_number}"]
    if prev.scrutinize:
        parts.append(f"prev_questions: {prev.scrutinize.questions}")
        parts.append(f"prev_next_action: {prev.scrutinize.next_action}")
    if prev.execute_result:
        parts.append(f"execute_success: {prev.execute_result.success}")
        parts.append(f"execute_summary: {prev.execute_result.summary}")
        parts.append(f"steps_taken: {prev.execute_result.steps_taken}")
        parts.append(f"error_count: {prev.execute_result.error_count}")
    if prev.promote:
        parts.append(f"prev_quality_score: {prev.promote.quality_score}")
        parts.append(f"prev_next_round_focus: {prev.promote.next_round_focus}")
    return "\n".join(parts)


def _build_output(payload: dict[str, Any]) -> ScrutinizeOutput:
    questions_raw = payload.get("questions")
    questions: list[str] = []
    if isinstance(questions_raw, list):
        questions = [str(q).strip() for q in questions_raw if str(q).strip()]
    if not questions:
        questions = ["模型未返回有效问题；先验证上轮假设并复核 PRD。"]

    next_action = str(payload.get("next_action") or "").strip()
    if not next_action:
        next_action = "继续推进 PRD：先读取最相关文件再选择动作。"

    risk_raw = payload.get("risk_flags")
    risk_flags: list[str] = []
    if isinstance(risk_raw, list):
        risk_flags = [str(r).strip() for r in risk_raw if str(r).strip()]

    return ScrutinizeOutput(
        questions=questions,
        next_action=next_action,
        risk_flags=risk_flags,
    )


def _loose_parse_json(text: str) -> dict[str, Any]:
    import json
    import re

    text = (text or "").strip()
    if not text:
        return {}
    try:
        loaded = json.loads(text)
    except json.JSONDecodeError:
        match = re.search(r"\{.*\}", text, re.DOTALL)
        if not match:
            return {}
        loaded = json.loads(match.group(0))
    return loaded if isinstance(loaded, dict) else {}


__all__ = ["Scrutinizer"]
