"""MAGIScheduler —— 螺旋上升主循环调度器。

每轮的标准流程：
    raw_event(magi_round_start)
      → Scrutinize（审视）
      → Execute（执行，薄壳调 Forge）
      → side-git 快照（可选，若提供 handler）
      → Promote（提升，含停止判断）
    raw_event(magi_round_end)
    若 promote.should_stop or deadline 到 → break
    else 按 promote.next_round_interval_s 睡眠

estimate_hours：
- 用 LLM（推荐 v4-flash）评估任务复杂度，返回 float 小时数。
- 失败时落回启发式默认（1.0h）。
"""
from __future__ import annotations

import asyncio
import json
import time
from datetime import UTC, datetime
from typing import Any

from dscode.core.types import (
    LLMProviderProtocol,
    MAGIPhase,
    MAGIRoundLog,
    Message,
    PRDDocument,
    RawEvent,
    ScribeProtocol,
    ToolHandler,
)
from dscode.deepseek.auto_router import AutoRouter
from dscode.deepseek.client import DeepSeekClient
from dscode.deepseek.prefix_completion import force_json
from dscode.magi.execute import Executor
from dscode.magi.promote import Promoter
from dscode.magi.scrutinize import Scrutinizer

_ESTIMATE_SYSTEM_PROMPT = """\
你是一名资深工程师，需要估算编码任务耗时。

参考基准：
- 小 bug 修复：~1 小时
- 单模块重构：~3 小时
- 新功能开发：~5 小时
- 架构级改造：~8 小时

严格输出 JSON：{"estimated_hours": <数字>, "rationale": "<一句话>"}
"""


class MAGIScheduler:
    """三脑螺旋主循环。"""

    def __init__(
        self,
        scrutinizer: Scrutinizer,
        executor: Executor,
        promoter: Promoter,
        scribe: ScribeProtocol,
        side_git_handler: ToolHandler | None = None,
        auto_router: AutoRouter | None = None,
    ) -> None:
        self.scrutinizer = scrutinizer
        self.executor = executor
        self.promoter = promoter
        self.scribe = scribe
        self.side_git_handler = side_git_handler
        self.auto_router = auto_router

        # 若 scheduler 传入了 auto_router 而三脑还没绑定，自动透传一次
        if auto_router is not None:
            if getattr(self.scrutinizer, "auto_router", None) is None:
                try:
                    self.scrutinizer.auto_router = auto_router  # type: ignore[attr-defined]
                except Exception:
                    pass
            if getattr(self.promoter, "auto_router", None) is None:
                try:
                    self.promoter.auto_router = auto_router  # type: ignore[attr-defined]
                except Exception:
                    pass

    # ------------------------------------------------------------
    # 主循环
    # ------------------------------------------------------------

    async def run(
        self,
        prd: PRDDocument,
        session_id: str,
        deadline: datetime | None = None,
        max_rounds: int = 20,
        spec_text: str = "",
        codebase_summary: str = "",
    ) -> list[MAGIRoundLog]:
        """螺旋上升主循环。

        Args:
            prd: 由 Plan 阶段产出的 PRDDocument。
            session_id: 当前会话 ID（用于 RawEvent / Forge 路由）。
            deadline: 绝对截止时间。None 表示不限时（仅看 max_rounds / should_stop）。
            max_rounds: 最大轮数硬上限（防止无限循环）。
            spec_text: 注入 Scrutinize 的项目规范。
            codebase_summary: 注入 Scrutinize 的代码摘要（可空）。

        Returns:
            每轮的 MAGIRoundLog 列表（按时间顺序）。
        """
        history: list[MAGIRoundLog] = []
        round_num = 0
        step_seed = 0  # MAGI 边界 RawEvent 的 step_number；与 Forge 内部 step 不冲突

        while True:
            # —— 全局停止：deadline / max_rounds ——
            if deadline is not None and datetime.now(tz=deadline.tzinfo or UTC) >= deadline:
                break
            if round_num >= max_rounds:
                break

            round_num += 1
            round_log = MAGIRoundLog(round_number=round_num)

            # —— magi_round_start ——
            step_seed += 1
            await self._safe_write_raw(
                RawEvent(
                    session_id=session_id,
                    task_id=prd.task_id,
                    step_number=step_seed,
                    event_type="magi_round_start",
                    data={
                        "round_number": round_num,
                        "phase": MAGIPhase.SCRUTINIZE.value,
                        "task_description": prd.task_description,
                    },
                )
            )

            # ===== ① Scrutinize =====
            previous_round = history[-1] if history else None
            scrutinize_out = await self.scrutinizer.scrutinize(
                prd=prd,
                previous_round=previous_round,
                spec_text=spec_text,
                codebase_summary=codebase_summary,
            )
            round_log.scrutinize = scrutinize_out

            # ===== ② Execute =====
            exec_result = await self.executor.execute(
                next_action=scrutinize_out.next_action,
                session_id=session_id,
                task_id=prd.task_id,
                max_steps=20,
            )
            round_log.execute_result = exec_result

            # ===== Side-git 快照（可选） =====
            if self.side_git_handler is not None:
                round_log.snapshot_id = await self._try_snapshot(
                    round_num=round_num,
                    task_id=prd.task_id,
                )

            # ===== ③ Promote =====
            promote_out = await self.promoter.promote(
                execution_result=exec_result,
                history=history,
                prd=prd,
            )
            round_log.promote = promote_out
            round_log.ended_at = time.time()

            history.append(round_log)

            # —— magi_round_end ——
            step_seed += 1
            await self._safe_write_raw(
                RawEvent(
                    session_id=session_id,
                    task_id=prd.task_id,
                    step_number=step_seed,
                    event_type="magi_round_end",
                    data={
                        "round_number": round_num,
                        "quality_score": promote_out.quality_score,
                        "should_stop": promote_out.should_stop,
                        "stop_reason": promote_out.stop_reason,
                        "snapshot_id": round_log.snapshot_id,
                    },
                )
            )

            # —— 停止判定 ——
            if promote_out.should_stop:
                break

            # —— 自适应间隔 ——
            if promote_out.next_round_interval_s > 0:
                # deadline-aware：不要睡过头
                interval = promote_out.next_round_interval_s
                if deadline is not None:
                    remaining = (deadline - datetime.now(tz=deadline.tzinfo or UTC)).total_seconds()
                    interval = max(0.0, min(interval, remaining))
                if interval > 0:
                    await asyncio.sleep(interval)

        return history

    # ------------------------------------------------------------
    # 估算
    # ------------------------------------------------------------

    async def estimate_hours(
        self,
        task_description: str,
        llm: LLMProviderProtocol,
        model: str = "deepseek-v4-flash",
    ) -> float:
        """让 LLM 估算任务复杂度（小时）。失败兜底 1.0。"""
        user_prompt = (
            f"Task: {task_description}\n\n"
            "请基于复杂度估算 estimated_hours（数字）。"
        )
        try:
            if isinstance(llm, DeepSeekClient):
                payload = await force_json(
                    client=llm,
                    schema_hint='{"estimated_hours": number, "rationale": str}',
                    model=model,
                    user_prompt=user_prompt,
                    system_prompt=_ESTIMATE_SYSTEM_PROMPT,
                    max_tokens=256,
                )
            else:
                resp = await llm.chat(
                    messages=[
                        Message(role="system", content=_ESTIMATE_SYSTEM_PROMPT),
                        Message(role="user", content=user_prompt),
                    ],
                    model=model,
                )
                payload = _loose_parse_json(resp.content or "")

            hours = float(payload.get("estimated_hours") or 1.0)
        except Exception:
            hours = 1.0
        return max(0.1, hours)

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    async def _safe_write_raw(self, event: RawEvent) -> None:
        try:
            await self.scribe.write_raw(event)
        except Exception:
            # Scribe 写入失败不应阻塞 MAGI 主循环
            pass

    async def _try_snapshot(self, round_num: int, task_id: str | None) -> str | None:
        """调 side-git 快照 handler；任何异常都返回 None。"""
        snapshot_id = f"magi-{task_id or 'none'}-r{round_num}-{int(time.time())}"
        try:
            result = await self.side_git_handler(  # type: ignore[misc]
                {
                    "round_id": snapshot_id,
                    "message": f"MAGI R{round_num} after execute",
                }
            )
            if getattr(result, "success", False):
                return snapshot_id
        except Exception:
            pass
        return None


# ============================================================
# 辅助
# ============================================================

def _loose_parse_json(text: str) -> dict[str, Any]:
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


__all__ = ["MAGIScheduler"]
