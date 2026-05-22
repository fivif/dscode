"""GrillMe —— mattpocock SKILL 风格深度访谈引擎。

行为约定：
- 单线程提问，**每问一题先给推荐答案**，让用户做"确认 / 修改"而非"白答"。
- 5~10 轮收敛；模型自评 `is_final=True` 时提前结束。
- 任何一轮模型若返回不合法 JSON，自动跳出（避免死循环）。

实现技术细节：
- 使用 `force_json` 强制结构化输出（DeepSeek beta + prefix completion）。
  GrillMe 自身只依赖 `LLMProviderProtocol`；若拿到的不是 `DeepSeekClient`，
  退回到普通 chat + 启发式 JSON 抽取。
"""
from __future__ import annotations

import json
import re
from collections.abc import Awaitable, Callable
from typing import Any

from dscode.core.types import LLMProviderProtocol, Message
from dscode.deepseek.client import DeepSeekClient
from dscode.deepseek.prefix_completion import force_json

AskUserFn = Callable[[str, str], Awaitable[str]]
"""(question, recommended_answer) -> user_answer 的异步回调。"""


_SYSTEM_PROMPT = """\
你是一名资深工程师面试官，正在做 grill-me 风格的需求深度访谈。

目标：把模糊任务在 5~10 轮内收敛成清晰、可执行的需求。

规则：
1. **单线程提问**：一次只问一个最关键的、未明确的点。
2. **每问一题先给推荐答案**：用户最常见的合理选择是什么？给出你的判断。
3. **依据先于结论**：用 1~2 句话解释为什么推荐这个答案（参考 spec、行业惯例、安全考量）。
4. **判断是否结束**：若关键点都已澄清，置 `is_final: true`。
5. 已澄清的问题不要重复；推荐答案要具体、可落地，不要"看情况"。

每一轮严格输出如下 JSON（不要任何额外文本）：
{
  "question": "下一个最关键的问题，问题要具体",
  "recommended_answer": "你推荐的答案，具体到字段/路径/数值/取舍",
  "rationale": "1-2 句简要理由",
  "is_final": false
}
"""


class GrillMe:
    """grill-me 风格访谈引擎。

    Args:
        llm: 任意实现 `LLMProviderProtocol` 的 provider。
        model: 默认 deepseek-v4-flash（访谈用 flash 即可省钱）。
    """

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

    async def interview(
        self,
        task_description: str,
        spec_text: str,
        max_rounds: int = 10,
        ask_user: AskUserFn | None = None,
    ) -> list[tuple[str, str]]:
        """跑一轮 grill-me 访谈，返回 (问题, 回答) 列表。

        Args:
            task_description: 用户最初的模糊任务描述。
            spec_text: 来自 SpecLoader.format_for_prompt() 的注入文本。
            max_rounds: 最大轮数（默认 10，PRD 要求 5~10）。
            ask_user: 与用户交互的回调；为 None 时进入"非交互模式"，
                      自动采用每轮的 `recommended_answer`。

        Returns:
            访谈 QA 列表，长度 1..max_rounds。
        """
        qa_log: list[tuple[str, str]] = []

        for round_idx in range(1, max_rounds + 1):
            try:
                payload = await self._ask_one_round(
                    task_description=task_description,
                    spec_text=spec_text,
                    qa_log=qa_log,
                    round_idx=round_idx,
                    max_rounds=max_rounds,
                )
            except Exception:
                # 模型异常 / JSON 解析失败 -> 收敛退出
                break

            question = str(payload.get("question", "")).strip()
            recommended = str(payload.get("recommended_answer", "")).strip()
            is_final = bool(payload.get("is_final", False))

            if not question:
                # 模型没问出有意义的问题，结束
                break

            # 用户交互（或非交互模式）
            if ask_user is None:
                user_answer = recommended
            else:
                user_answer = await ask_user(question, recommended)
                if user_answer is None or not user_answer.strip():
                    user_answer = recommended

            qa_log.append((question, user_answer.strip()))

            if is_final:
                break

        return qa_log

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    async def _ask_one_round(
        self,
        task_description: str,
        spec_text: str,
        qa_log: list[tuple[str, str]],
        round_idx: int,
        max_rounds: int,
    ) -> dict[str, Any]:
        """触发 LLM 输出当前轮的 JSON 决策。"""
        context_block = _format_qa_log(qa_log)
        spec_block = spec_text.strip() or "(暂无项目规范)"

        user_prompt = (
            f"# 任务\n{task_description}\n\n"
            f"# 项目规范\n{spec_block}\n\n"
            f"# 已澄清问答（旧 → 新）\n{context_block}\n\n"
            f"# 现在是第 {round_idx} / {max_rounds} 轮。\n"
            "请基于以上信息，输出下一轮访谈 JSON。"
            "如果关键点都已澄清，把 is_final 设为 true。"
        )

        # 优先走 force_json（DeepSeek beta + prefix completion）
        if isinstance(self.llm, DeepSeekClient):
            return await force_json(
                client=self.llm,
                schema_hint=(
                    '{"question": str, "recommended_answer": str, '
                    '"rationale": str, "is_final": bool}'
                ),
                model=self.model,
                user_prompt=user_prompt,
                system_prompt=_SYSTEM_PROMPT,
            )

        # 非 DeepSeek provider：普通 chat + 启发式 JSON 抽取
        messages = [
            Message(role="system", content=_SYSTEM_PROMPT),
            Message(role="user", content=user_prompt),
        ]
        resp = await self.llm.chat(messages=messages, model=self.model)
        return _extract_first_json_object(resp.content or "")


# ============================================================
# 辅助
# ============================================================

def _format_qa_log(qa_log: list[tuple[str, str]]) -> str:
    if not qa_log:
        return "(尚无)"
    lines: list[str] = []
    for i, (q, a) in enumerate(qa_log, 1):
        lines.append(f"Q{i}: {q}\nA{i}: {a}")
    return "\n\n".join(lines)


def _extract_first_json_object(text: str) -> dict[str, Any]:
    """从模型自由文本中抽出第一个完整 JSON 对象。"""
    text = (text or "").strip()
    if not text:
        raise ValueError("empty model output")
    try:
        loaded = json.loads(text)
        if isinstance(loaded, dict):
            return loaded
    except json.JSONDecodeError:
        pass
    match = re.search(r"\{.*\}", text, re.DOTALL)
    if not match:
        raise ValueError(f"no JSON object in model output: {text!r}")
    loaded = json.loads(match.group(0))
    if not isinstance(loaded, dict):
        raise ValueError("model output is not a JSON object")
    return loaded


__all__ = ["AskUserFn", "GrillMe"]
