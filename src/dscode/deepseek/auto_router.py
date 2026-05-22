"""Flash/Pro 自动路由。

用最便宜的模型（v4-flash + thinking=off）先跑一次任务分类，
再根据复杂度决定主回路用什么模型。

路由策略：
- simple   → deepseek-v4-flash, thinking=off
- medium   → deepseek-v4-flash, reasoning_effort=medium
- complex  → deepseek-v4-pro, reasoning_effort=high
- deep     → deepseek-v4-pro, reasoning_effort=max
"""
from __future__ import annotations

import json
import re
from typing import Literal

from pydantic import BaseModel, Field

from dscode.core.types import Message
from dscode.deepseek.client import DeepSeekClient

Complexity = Literal["simple", "medium", "complex", "deep"]


ROUTER_SYSTEM_PROMPT = """你是任务复杂度分类器。你的唯一输出是一个 JSON 对象。

分类规则：
- simple: 单文件简单改动、问答、查询、格式化、列出文件等无需推理的任务
- medium: 跨 2-3 个文件的改动、需要少量推理、典型 bug 修复
- complex: 多文件重构、需要规划、需要权衡多个方案、跨模块设计
- deep: 算法设计、架构决策、需要长链推理或证明、根因分析

只输出形如 {"complexity": "simple|medium|complex|deep", "rationale": "<= 30 字理由"} 的 JSON。
不要任何额外文本、markdown、代码块。
"""


class RouteDecision(BaseModel):
    """路由决策结果。"""

    recommended_model: str = Field(description="推荐使用的模型 ID")
    thinking: bool = Field(default=False, description="是否开启 thinking 模式")
    reasoning_effort: str | None = Field(
        default=None, description="reasoning_effort: low/medium/high/max"
    )
    rationale: str = Field(default="", description="决策理由")


# 复杂度 -> 模型档位映射
_DECISION_TABLE: dict[Complexity, RouteDecision] = {
    "simple": RouteDecision(
        recommended_model="deepseek-v4-flash",
        thinking=False,
        reasoning_effort=None,
        rationale="simple",
    ),
    "medium": RouteDecision(
        recommended_model="deepseek-v4-flash",
        thinking=True,
        reasoning_effort="medium",
        rationale="medium",
    ),
    "complex": RouteDecision(
        recommended_model="deepseek-v4-pro",
        thinking=True,
        reasoning_effort="high",
        rationale="complex",
    ),
    "deep": RouteDecision(
        recommended_model="deepseek-v4-pro",
        thinking=True,
        reasoning_effort="max",
        rationale="deep",
    ),
}


class AutoRouter:
    """Flash/Pro 自动路由器。"""

    def __init__(
        self,
        client: DeepSeekClient,
        router_model: str = "deepseek-v4-flash",
    ) -> None:
        """构造路由器。

        Args:
            client: DeepSeek 客户端实例。
            router_model: 用于分类的便宜模型。
        """
        self.client = client
        self.router_model = router_model

    async def route(
        self,
        task: str,
        context_summary: str = "",
    ) -> RouteDecision:
        """对任务进行路由。

        内部调用一次 v4-flash（thinking=off）进行复杂度分类。

        Args:
            task: 任务描述。
            context_summary: 可选的上下文摘要（仓库情况、当前阶段等）。

        Returns:
            RouteDecision：推荐的模型、thinking、reasoning_effort、rationale。
        """
        user_payload = task if not context_summary else (
            f"任务: {task}\n\n上下文: {context_summary}"
        )
        messages = [
            Message(role="system", content=ROUTER_SYSTEM_PROMPT),
            Message(role="user", content=user_payload),
        ]

        try:
            resp = await self.client.chat(
                messages=messages,
                model=self.router_model,
                thinking=False,
                reasoning_effort=None,
                temperature=0.0,
                max_tokens=200,
            )
            complexity, rationale = self._parse_response(resp.content)
        except Exception as exc:  # 路由失败兜底
            complexity = self._heuristic_fallback(task)
            rationale = f"router_error: {exc!s}; fallback heuristic"

        decision = _DECISION_TABLE[complexity].model_copy()
        decision.rationale = rationale or complexity
        return decision

    # ------------------------------------------------------------
    # 解析与兜底
    # ------------------------------------------------------------

    @staticmethod
    def _parse_response(text: str) -> tuple[Complexity, str]:
        """解析 router 模型输出。"""
        text = (text or "").strip()
        if not text:
            return "medium", "empty router response, default medium"
        # 抽取最外层 JSON
        match = re.search(r"\{.*\}", text, re.DOTALL)
        if not match:
            return "medium", "unparseable router response"
        try:
            data = json.loads(match.group(0))
        except json.JSONDecodeError:
            return "medium", "json decode failed"
        comp = (data.get("complexity") or "").strip().lower()
        if comp not in _DECISION_TABLE:
            return "medium", f"unknown complexity '{comp}'"
        rationale = str(data.get("rationale", "")).strip() or comp
        return comp, rationale  # type: ignore[return-value]

    @staticmethod
    def _heuristic_fallback(task: str) -> Complexity:
        """路由调用失败时用关键词启发式分类。"""
        t = task.lower()
        if any(w in t for w in ("架构", "重构", "设计", "architect", "refactor", "design")):
            return "complex"
        if any(w in t for w in ("证明", "推理", "算法", "proof", "algorithm")):
            return "deep"
        if any(w in t for w in ("修", "fix", "bug", "调试")):
            return "medium"
        if len(task) < 80:
            return "simple"
        return "medium"
