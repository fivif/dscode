"""Promoter —— MAGI 第三脑（Melchior），提升阶段。

职责：
- 给本轮打质量分（0~100）。
- 决定是否停止整个 MAGI 循环：
    * 任务已完成 → should_stop=True (stop_reason="task_complete")
    * 连续 3 轮无进展（quality 不升） → should_stop=True ("stalled")
    * 异常率高（连续多轮 error_count>0） → 提示但不强停（由 LLM 决定）
- 给下一轮 focus。

时间预算停止由 MAGIScheduler 负责（外部 deadline），不在这里做。
"""
from __future__ import annotations

from typing import Any

from dscode.core.types import (
    ExecutionResult,
    LLMProviderProtocol,
    MAGIRoundLog,
    Message,
    PRDDocument,
    PromoteOutput,
)
from dscode.deepseek.auto_router import AutoRouter
from dscode.deepseek.client import DeepSeekClient
from dscode.deepseek.prefix_completion import force_json

# 连续 N 轮 quality 不升即视为停滞
_STALL_WINDOW: int = 3


_SYSTEM_PROMPT = """\
你是 MAGI-Melchior（提升脑）。MAGI 的每一轮在你这里收尾。

输入：
- 本轮执行结果（success/summary/steps/tokens/errors）
- 全部历史轮次（含 quality_score 序列）
- PRD（task_description / goals / acceptance_criteria）

任务：
1. 给本轮打 quality_score（0~100，整数即可）。评分标准：
   - 100 = 完全满足 acceptance_criteria
   - 70~90 = 主要目标达成，剩余细节
   - 30~60 = 部分推进，仍有较大缺口
   - 0~20 = 几乎没进展或出错
2. 判断是否停止：
   - 已达到 PRD 所有 acceptance_criteria → should_stop=true, stop_reason="task_complete"
   - 否则 should_stop=false
3. 给出下一轮 focus（具体到方向，不要客套话）。
4. 给下一轮 interval（秒），让 Anvil 有时间反思；快任务 0 即可，长任务 5~30。

严格输出 JSON：
{
  "quality_score":         85,
  "should_stop":           false,
  "stop_reason":           null,
  "next_round_focus":      "...",
  "next_round_interval_s": 0
}
"""


class Promoter:
    """MAGI Promote 阶段实现。"""

    def __init__(
        self,
        llm: LLMProviderProtocol,
        model: str = "deepseek-v4-pro",
        auto_router: AutoRouter | None = None,
    ) -> None:
        self.llm = llm
        self.model = model
        self.auto_router = auto_router

    async def promote(
        self,
        execution_result: ExecutionResult,
        history: list[MAGIRoundLog],
        prd: PRDDocument,
    ) -> PromoteOutput:
        """跑一次提升。

        除了 LLM 判断外，还叠加**确定性停止规则**：
        - 连续 3 轮 quality 不升 → 强制 should_stop=True ("stalled")
        - LLM 已判定 should_stop=True → 透传

        Returns:
            PromoteOutput
        """
        user_prompt = self._build_user_prompt(execution_result, history, prd)

        # —— Auto 路由：先决定本次调用用什么模型/thinking/effort ——
        model, thinking_kwargs = await self._resolve_route(
            task_text=prd.task_description,
            context_summary=execution_result.summary[:500] if execution_result.summary else "",
        )

        try:
            payload = await self._chat_json(
                user_prompt, model=model, thinking_kwargs=thinking_kwargs
            )
        except Exception:
            payload = {}

        output = _build_output(payload)
        return _apply_stall_rule(output, history)

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
                schema_hint=(
                    '{"quality_score": number, "should_stop": bool, '
                    '"stop_reason": str|null, "next_round_focus": str, '
                    '"next_round_interval_s": number}'
                ),
                model=eff_model,
                user_prompt=user_prompt,
                system_prompt=_SYSTEM_PROMPT,
                max_tokens=512,
            )
        messages = [
            Message(role="system", content=_SYSTEM_PROMPT),
            Message(role="user", content=user_prompt),
        ]
        resp = await self.llm.chat(messages=messages, model=eff_model, **extra)
        return _loose_parse_json(resp.content or "")

    def _build_user_prompt(
        self,
        execution_result: ExecutionResult,
        history: list[MAGIRoundLog],
        prd: PRDDocument,
    ) -> str:
        history_block = _format_history(history)
        return (
            f"# PRD\ntask: {prd.task_description}\n"
            f"goals: {prd.goals}\n"
            f"acceptance_criteria: {prd.acceptance_criteria}\n\n"
            f"# 本轮执行结果\n{_format_execute(execution_result)}\n\n"
            f"# 历史轮次（旧 → 新）\n{history_block}\n\n"
            "请基于以上信息输出 JSON。"
        )


# ============================================================
# 辅助 / 规则
# ============================================================

def _format_execute(r: ExecutionResult) -> str:
    return (
        f"success: {r.success}\n"
        f"summary: {r.summary}\n"
        f"steps_taken: {r.steps_taken}\n"
        f"tool_call_count: {r.tool_call_count}\n"
        f"error_count: {r.error_count}\n"
        f"tokens_used: {r.tokens_used}\n"
        f"cache_hit_rate: {r.cache_hit_rate:.2f}\n"
        f"wall_time_ms: {r.wall_time_ms}"
    )


def _format_history(history: list[MAGIRoundLog]) -> str:
    if not history:
        return "(无历史轮次)"
    lines: list[str] = []
    for h in history:
        q = h.promote.quality_score if h.promote else None
        focus = h.promote.next_round_focus if h.promote else None
        lines.append(f"r{h.round_number}: quality={q} focus={focus!r}")
    return "\n".join(lines)


def _build_output(payload: dict[str, Any]) -> PromoteOutput:
    try:
        quality = float(payload.get("quality_score") or 0.0)
    except (TypeError, ValueError):
        quality = 0.0
    quality = max(0.0, min(100.0, quality))

    should_stop = bool(payload.get("should_stop") or False)
    stop_reason = payload.get("stop_reason")
    stop_reason_s = str(stop_reason).strip() if stop_reason else None

    focus = payload.get("next_round_focus")
    focus_s = str(focus).strip() if focus else None

    try:
        interval = float(payload.get("next_round_interval_s") or 0.0)
    except (TypeError, ValueError):
        interval = 0.0
    interval = max(0.0, interval)

    return PromoteOutput(
        quality_score=quality,
        should_stop=should_stop,
        stop_reason=stop_reason_s,
        next_round_focus=focus_s,
        next_round_interval_s=interval,
    )


def _apply_stall_rule(out: PromoteOutput, history: list[MAGIRoundLog]) -> PromoteOutput:
    """连续 _STALL_WINDOW 轮 quality 未升 → 强制 should_stop=True。

    取最近 _STALL_WINDOW 轮历史 + 本轮（即将写入）。本轮的 quality_score 在 out 中。
    """
    if out.should_stop:
        return out

    # 收集本轮前面 (_STALL_WINDOW - 1) 轮的 quality
    prev_qualities: list[float] = []
    for h in history[-(_STALL_WINDOW - 1):]:
        if h.promote is not None:
            prev_qualities.append(h.promote.quality_score)

    # 至少需要满 _STALL_WINDOW 个数据点才能触发
    if len(prev_qualities) < _STALL_WINDOW - 1:
        return out

    sequence = [*prev_qualities, out.quality_score]
    # 非严格递增：任何相邻 a >= b 都打破"递增"前提；
    # 这里采用"连续 N 轮 quality 都没超过历史最高"的判定。
    historic_best = max(prev_qualities)
    if all(q <= historic_best for q in sequence[len(prev_qualities):]):
        # 本轮也没超过历史最高 → 停滞
        return out.model_copy(update={
            "should_stop": True,
            "stop_reason": out.stop_reason or "stalled: quality did not improve over last rounds",
        })
    return out


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


__all__ = ["Promoter"]
