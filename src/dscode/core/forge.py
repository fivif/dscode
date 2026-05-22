"""Forge —— ReAct 流式执行引擎。

主循环：
1. Scribe.context_packet(task) → 拿到 patterns/facts/recent
2. 装配 messages（system + 上下文注入 + user task）
3. for step in range(max_steps):
   - llm.chat(messages, tools)
   - 解析 tool_calls：依次调用 → 写 RawEvent → yield StreamEvent → 提取 Fact
   - 无 tool_calls：yield THOUGHT + COMPLETE，break
4. 所有异常 → StreamEvent(ERROR)
"""
from __future__ import annotations

import json
import time
from collections.abc import AsyncGenerator
from typing import Any

from dscode.core.types import (
    ContextPacket,
    Fact,
    LLMProviderProtocol,
    Message,
    RawEvent,
    ScribeProtocol,
    StreamEvent,
    StreamEventType,
    ToolCallSpec,
    ToolRegistryProtocol,
    ToolResult,
    ToolStatus,
)

# ============================================================
# System prompt 模板
# ============================================================

_SYSTEM_PROMPT_BASE = """\
你是 DS Code，一个严谨、流式、工具优先的代码 Agent。

行为原则：
1. 优先使用工具读取真实状态，不要凭记忆猜测。
2. 每一步都先简述意图，再发起工具调用。
3. 出错时分析错误，调整后继续；不要无声跳过。
4. 完成任务后，简明汇报你做了什么以及验证依据。
"""


def _format_context_packet(packet: ContextPacket) -> str:
    """把 ContextPacket 序列化为可读 system 提示片段。"""
    blocks: list[str] = []

    if packet.patterns:
        lines = ["## 学习到的模式（高优先采用）："]
        for p in packet.patterns:
            lines.append(
                f"- [{p.pattern_type}] 触发：{p.trigger_condition} "
                f"(confidence={p.confidence:.2f}, samples={p.sample_count})"
            )
        blocks.append("\n".join(lines))

    if packet.facts:
        lines = ["## 已验证事实："]
        for f in packet.facts:
            lines.append(f"- {f.subject} {f.predicate} {f.object} (conf={f.confidence:.2f})")
        blocks.append("\n".join(lines))

    if packet.recent_events:
        lines = ["## 最近事件（旧 → 新）："]
        for ev in packet.recent_events[-10:]:  # 防止过长
            summary = json.dumps(ev.data, ensure_ascii=False, default=str)[:200]
            lines.append(f"- step {ev.step_number} [{ev.event_type}] {summary}")
        blocks.append("\n".join(lines))

    return "\n\n".join(blocks) if blocks else ""


# ============================================================
# Forge
# ============================================================

class Forge:
    """ReAct 流式执行引擎。

    Args:
        llm: LLM Provider（DeepSeek / Anthropic / litellm 任一实现 LLMProviderProtocol）。
        scribe: 记忆引擎。
        tool_registry: 工具注册表。
        model: 模型名（默认 deepseek-v4-flash）。
        max_steps: 默认最大步数。可被 execute(max_steps=...) 覆盖。
    """

    def __init__(
        self,
        llm: LLMProviderProtocol,
        scribe: ScribeProtocol,
        tool_registry: ToolRegistryProtocol,
        model: str = "deepseek-v4-flash",
        max_steps: int = 40,
    ) -> None:
        self.llm = llm
        self.scribe = scribe
        self.tools = tool_registry
        self.model = model
        self.max_steps = max_steps

    async def execute(
        self,
        task: str,
        session_id: str,
        task_id: str | None = None,
        max_steps: int | None = None,
    ) -> AsyncGenerator[StreamEvent, None]:
        """跑一轮 ReAct 流式循环。

        Yields:
            StreamEvent（THOUGHT / TOOL_START / TOOL_RESULT / ERROR / USAGE / COMPLETE）。
        """
        if max_steps is None:
            max_steps = self.max_steps

        started_at = time.time()
        step_counter = 0
        tool_call_count = 0
        error_count = 0
        total_prompt_tokens = 0
        total_completion_tokens = 0
        cache_hit_tokens = 0
        cache_miss_tokens = 0

        # 1) 取上下文
        try:
            packet = await self.scribe.context_packet(task)
        except Exception as e:
            error_count += 1
            yield StreamEvent(
                type=StreamEventType.ERROR,
                data={"phase": "context_packet", "error": str(e)},
            )
            packet = ContextPacket()

        context_block = _format_context_packet(packet)
        system_text = _SYSTEM_PROMPT_BASE
        if context_block:
            system_text = system_text + "\n\n" + context_block

        # 2) 装配 messages
        messages: list[Message] = [
            Message(role="system", content=system_text),
            Message(role="user", content=task),
        ]

        # 写入 user_message RawEvent
        step_counter += 1
        await self._safe_write_raw(
            RawEvent(
                session_id=session_id,
                task_id=task_id,
                step_number=step_counter,
                event_type="user_message",
                data={"task": task},
            )
        )

        # 3) 主循环
        tools_spec = self.tools.to_openai_tools()
        finish_summary = ""

        for _ in range(max_steps):
            try:
                response = await self.llm.chat(
                    messages=messages,
                    model=self.model,
                    tools=tools_spec or None,
                )
            except Exception as e:
                error_count += 1
                yield StreamEvent(
                    type=StreamEventType.ERROR,
                    data={"phase": "llm.chat", "error": str(e)},
                )
                break

            # 累计 usage
            total_prompt_tokens += response.usage.prompt_tokens
            total_completion_tokens += response.usage.completion_tokens
            cache_hit_tokens += response.usage.prompt_cache_hit_tokens
            cache_miss_tokens += response.usage.prompt_cache_miss_tokens

            tool_calls = response.tool_calls or []

            # —— 分支 A：有 tool_calls，进入执行 → 回喂
            if tool_calls:
                # 先记录 assistant 思考片段（如果有）
                if response.content:
                    yield StreamEvent(
                        type=StreamEventType.THOUGHT,
                        data={"content": response.content},
                    )

                # 将 assistant 消息（含 tool_calls）加入 messages
                messages.append(
                    Message(
                        role="assistant",
                        content=response.content or "",
                        tool_calls=tool_calls,
                        reasoning_content=response.reasoning_content,
                    )
                )

                for tc in tool_calls:
                    tool_call_count += 1
                    step_counter += 1
                    name = tc.function.name
                    args = _parse_tool_args(tc.function.arguments)

                    # —— TOOL_START
                    yield StreamEvent(
                        type=StreamEventType.TOOL_START,
                        data={
                            "tool_call_id": tc.id,
                            "name": name,
                            "arguments": args,
                        },
                    )
                    call_event = RawEvent(
                        session_id=session_id,
                        task_id=task_id,
                        step_number=step_counter,
                        event_type="tool_call",
                        data={
                            "tool_call_id": tc.id,
                            "name": name,
                            "arguments": args,
                        },
                    )
                    await self._safe_write_raw(call_event)

                    # —— 执行工具
                    handler = self.tools.get_handler(name)
                    if handler is None:
                        result = ToolResult(
                            status=ToolStatus.ERROR,
                            content="",
                            error=f"unknown tool: {name}",
                        )
                    else:
                        try:
                            result = await handler(args)
                        except Exception as e:
                            error_count += 1
                            result = ToolResult(
                                status=ToolStatus.ERROR,
                                content="",
                                error=f"{type(e).__name__}: {e}",
                            )

                    if not result.success:
                        error_count += 1

                    # —— TOOL_RESULT
                    step_counter += 1
                    yield StreamEvent(
                        type=StreamEventType.TOOL_RESULT,
                        data={
                            "tool_call_id": tc.id,
                            "name": name,
                            "status": result.status.value,
                            "content": result.content,
                            "error": result.error,
                            "elapsed_ms": result.elapsed_ms,
                        },
                    )
                    result_event = RawEvent(
                        session_id=session_id,
                        task_id=task_id,
                        step_number=step_counter,
                        event_type="tool_result",
                        data={
                            "tool_call_id": tc.id,
                            "name": name,
                            "status": result.status.value,
                            "content": result.content,
                            "error": result.error,
                            "elapsed_ms": result.elapsed_ms,
                        },
                    )
                    await self._safe_write_raw(result_event)

                    # —— 事实提取
                    if result.success:
                        for fact in self._extract_facts(name, args, result):
                            # 注入 provenance：本次工具调用 + 结果两个事件
                            if not fact.provenance_chain:
                                fact = fact.model_copy(
                                    update={
                                        "provenance_chain": [call_event.id, result_event.id],
                                        "source_raw_event_id": result_event.id,
                                    }
                                )
                            try:
                                await self.scribe.write_fact(fact)
                            except Exception:
                                # fact 写入失败不应阻塞主循环
                                pass

                    # —— 回喂 tool message
                    tool_msg_content = result.content if result.content else (
                        result.error or ""
                    )
                    messages.append(
                        Message(
                            role="tool",
                            tool_call_id=tc.id,
                            name=name,
                            content=tool_msg_content,
                        )
                    )

                # 进入下一轮 LLM
                continue

            # —— 分支 B：纯文本 → 任务完成
            final_text = response.content or ""
            if final_text:
                yield StreamEvent(
                    type=StreamEventType.THOUGHT,
                    data={"content": final_text},
                )
                step_counter += 1
                await self._safe_write_raw(
                    RawEvent(
                        session_id=session_id,
                        task_id=task_id,
                        step_number=step_counter,
                        event_type="llm_thought",
                        data={"content": final_text},
                    )
                )
            messages.append(Message(role="assistant", content=final_text))
            finish_summary = final_text
            break
        else:
            # max_steps 用尽
            finish_summary = "[max_steps exhausted]"
            yield StreamEvent(
                type=StreamEventType.ERROR,
                data={"phase": "loop", "error": "max_steps exhausted"},
            )

        # 4) USAGE + COMPLETE
        yield StreamEvent(
            type=StreamEventType.USAGE,
            data={
                "prompt_tokens": total_prompt_tokens,
                "completion_tokens": total_completion_tokens,
                "cache_hit_tokens": cache_hit_tokens,
                "cache_miss_tokens": cache_miss_tokens,
            },
        )

        wall_ms = int((time.time() - started_at) * 1000)
        yield StreamEvent(
            type=StreamEventType.COMPLETE,
            data={
                "summary": finish_summary,
                "steps_taken": step_counter,
                "tool_call_count": tool_call_count,
                "error_count": error_count,
                "wall_time_ms": wall_ms,
                "usage": {
                    "prompt_tokens": total_prompt_tokens,
                    "completion_tokens": total_completion_tokens,
                    "cache_hit_tokens": cache_hit_tokens,
                    "cache_miss_tokens": cache_miss_tokens,
                },
            },
        )

    # -------- 辅助 --------

    async def _safe_write_raw(self, event: RawEvent) -> None:
        """写 RawEvent 失败不应中断主循环。"""
        try:
            await self.scribe.write_raw(event)
        except Exception:
            # TODO(forge): 失败时降级落到本地 fallback 文件
            pass

    def _extract_facts(
        self,
        tool_name: str,
        args: dict[str, Any],
        result: ToolResult,
    ) -> list[Fact]:
        """从工具结果中抽取 Fact。

        v1 极简策略：只为 file_read / grep / bash / shell 等常见读类工具
        构造一条 placeholder fact 作为示例骨架。

        TODO(forge): 引入更结构化的抽取（grep 的命中位置、bash 的退出码、
        file_read 的文件大小/行数等），按工具类型注册抽取器。
        """
        if not result.success:
            return []

        # 简单的工具白名单 → 抽取最少必要信息
        name = tool_name.lower()
        facts: list[Fact] = []

        if name in {"file_read", "do_file_read", "read"}:
            path = str(args.get("path") or args.get("file_path") or "")
            if path:
                facts.append(
                    Fact(
                        subject=path,
                        predicate="was_read_at",
                        object=str(int(time.time())),
                        confidence=1.0,
                    )
                )
        elif name in {"grep", "do_grep", "search"}:
            pattern = str(args.get("pattern") or args.get("query") or "")
            if pattern:
                facts.append(
                    Fact(
                        subject=pattern,
                        predicate="searched_at",
                        object=str(int(time.time())),
                        confidence=0.8,
                    )
                )
        # 其它工具：v1 不抽取
        return facts


# ============================================================
# 内部工具
# ============================================================

def _parse_tool_args(raw: str | None) -> dict[str, Any]:
    """LLM 返回的 arguments 是 JSON 字符串；容忍空 / 非法 JSON。"""
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {"_raw": raw}
    if not isinstance(parsed, dict):
        return {"_raw": raw}
    return parsed


# 重新导出 ToolCallSpec 以便测试 import 简化
__all__ = ["Forge", "ToolCallSpec"]
