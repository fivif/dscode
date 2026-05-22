"""Anthropic 兼容适配器。

为 Claude 模型族提供 LLMProviderProtocol 实现。

策略：
1. 优先使用官方 anthropic SDK（如安装）以获得最准确的字段映射（cache_control / thinking）。
2. 若 SDK 未安装则 fallback 到 LiteLLMProvider 走 `anthropic/<model>` 路由。

关键格式转换：
- **OpenAI 风格 tool_calls** → **Anthropic 风格 content 块（tool_use type）**
  OpenAI 把 tool_calls 放在 assistant message 的独立字段；Anthropic 把它们
  作为 content list 里的 `{"type": "tool_use", "id": ..., "name": ..., "input": {...}}` 块。
- **Anthropic 响应** → **OpenAI 风格**
  Anthropic 返回 `content: [{"type": "text", "text": "..."}, {"type": "tool_use", ...}]`，
  必须拍平为单一 `content: str` + 独立 `tool_calls: list[ToolCallSpec]`。
- **system 消息**：Anthropic 用顶层 `system` 字段，不在 messages 里。
- **tool result**：OpenAI 用 `{"role": "tool", "content": "...", "tool_call_id": "..."}`；
  Anthropic 用 `{"role": "user", "content": [{"type": "tool_result", "tool_use_id": ..., "content": ...}]}`。
"""
from __future__ import annotations

import json
from collections.abc import AsyncGenerator
from typing import Any, Literal

from dscode.core.types import (
    LLMResponse,
    Message,
    ToolCallSpec,
    ToolFunctionSpec,
    Usage,
)


def _import_anthropic() -> Any | None:
    """惰性导入 anthropic SDK。返回 None 表示未安装（触发 fallback）。"""
    try:
        import anthropic  # type: ignore[import-untyped]
    except ImportError:
        return None
    return anthropic


class AnthropicCompatProvider:
    """Anthropic SDK 适配器。

    若官方 SDK 未安装，自动 fallback 到 LiteLLMProvider。两条路径都实现
    LLMProviderProtocol 的相同行为契约。
    """

    def __init__(
        self,
        api_key: str | None = None,
        base_url: str | None = None,
        timeout: float = 120.0,
        max_retries: int = 2,
        prefer_sdk: bool = True,
    ) -> None:
        """构造适配器。

        Args:
            api_key: Anthropic API key（None 则用 ANTHROPIC_API_KEY env）。
            base_url: 自定义 endpoint（如 DeepSeek 的 Anthropic 兼容端点）。
            timeout: 单次请求超时秒。
            max_retries: 重试次数。
            prefer_sdk: True 时优先用 anthropic SDK；False 时强制走 litellm。
        """
        self.api_key = api_key
        self.base_url = base_url
        self.timeout = timeout
        self.max_retries = max_retries

        self._anthropic = _import_anthropic() if prefer_sdk else None
        self._client: Any = None
        self._fallback: Any = None  # LiteLLMProvider 实例

        if self._anthropic is not None:
            kwargs: dict[str, Any] = {
                "timeout": timeout,
                "max_retries": max_retries,
            }
            if api_key is not None:
                kwargs["api_key"] = api_key
            if base_url is not None:
                kwargs["base_url"] = base_url
            self._client = self._anthropic.AsyncAnthropic(**kwargs)
        else:
            # 回落 litellm。延迟到首次使用，避免 ImportError 阻断构造
            from dscode.providers.litellm_adapter import LiteLLMProvider

            self._fallback = LiteLLMProvider(
                api_key=api_key,
                api_base=base_url,
                timeout=timeout,
                max_retries=max_retries,
            )

    # ------------------------------------------------------------
    # 公共工具：OpenAI ↔ Anthropic 双向格式转换
    # ------------------------------------------------------------

    @staticmethod
    def _split_system_and_messages(
        messages: list[Message],
    ) -> tuple[str | None, list[Message]]:
        """提取首部连续的 system 消息作为顶层 system，剩余作为对话消息。"""
        system_parts: list[str] = []
        rest: list[Message] = []
        sys_consumed = False
        for m in messages:
            if m.role == "system" and not sys_consumed:
                if m.content:
                    system_parts.append(m.content)
                continue
            sys_consumed = True
            # 中段 system 消息：转 user-style note（Anthropic 不支持中段 system）
            if m.role == "system":
                rest.append(
                    Message(role="user", content=f"[system]\n{m.content or ''}")
                )
            else:
                rest.append(m)
        system = "\n\n".join(s for s in system_parts if s) if system_parts else None
        return system, rest

    @classmethod
    def messages_to_anthropic(
        cls, messages: list[Message]
    ) -> tuple[str | None, list[dict[str, Any]]]:
        """把 Message 列表转为 Anthropic 顶层 (system, messages)。

        - assistant tool_calls → content 内 tool_use 块
        - tool 角色 → user 角色 + tool_result 块
        - 同 role 连续合并由调用方负责（Anthropic 实际允许同 role 连续，故不强制）
        """
        system, rest = cls._split_system_and_messages(messages)
        out: list[dict[str, Any]] = []
        for m in rest:
            if m.role == "tool":
                # tool result → user / tool_result 块
                out.append(
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": m.tool_call_id or "",
                                "content": m.content or "",
                            }
                        ],
                    }
                )
                continue

            role = "assistant" if m.role == "assistant" else "user"

            blocks: list[dict[str, Any]] = []
            if m.content:
                blocks.append({"type": "text", "text": m.content})

            if m.tool_calls:
                for tc in m.tool_calls:
                    try:
                        args_obj: Any = (
                            json.loads(tc.function.arguments)
                            if tc.function.arguments
                            else {}
                        )
                    except (json.JSONDecodeError, TypeError):
                        args_obj = {"_raw": tc.function.arguments}
                    blocks.append(
                        {
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function.name,
                            "input": args_obj,
                        }
                    )

            # 若没有任何块，给个空 text 兜底（Anthropic 拒绝空 content）
            if not blocks:
                blocks.append({"type": "text", "text": ""})

            out.append({"role": role, "content": blocks})
        return system, out

    @classmethod
    def tools_to_anthropic(
        cls, tools: list[dict[str, Any]] | None
    ) -> list[dict[str, Any]] | None:
        """OpenAI function-tools → Anthropic tools。"""
        if not tools:
            return None
        converted: list[dict[str, Any]] = []
        for t in tools:
            # OpenAI: {"type": "function", "function": {"name", "description", "parameters"}}
            fn = t.get("function") if isinstance(t, dict) else None
            if isinstance(fn, dict):
                converted.append(
                    {
                        "name": fn.get("name", ""),
                        "description": fn.get("description", ""),
                        "input_schema": fn.get("parameters", {"type": "object"}),
                    }
                )
            elif isinstance(t, dict) and "name" in t and (
                "input_schema" in t or "parameters" in t
            ):
                # 已经是 Anthropic 风格
                converted.append(
                    {
                        "name": t["name"],
                        "description": t.get("description", ""),
                        "input_schema": t.get("input_schema")
                        or t.get("parameters", {"type": "object"}),
                    }
                )
        return converted or None

    @classmethod
    def anthropic_response_to_openai(
        cls, raw: Any, model: str
    ) -> LLMResponse:
        """把 Anthropic Message 对象转回 LLMResponse（OpenAI 风格）。"""
        data = raw if isinstance(raw, dict) else _to_dict(raw)

        content_blocks = data.get("content") or []
        text_parts: list[str] = []
        thinking_parts: list[str] = []
        tool_calls: list[ToolCallSpec] = []
        for blk in content_blocks:
            b = blk if isinstance(blk, dict) else _to_dict(blk)
            btype = b.get("type")
            if btype == "text":
                text_parts.append(b.get("text", "") or "")
            elif btype == "thinking":
                thinking_parts.append(b.get("thinking", "") or b.get("text", "") or "")
            elif btype == "tool_use":
                args_obj = b.get("input") or {}
                args_str = (
                    json.dumps(args_obj) if not isinstance(args_obj, str) else args_obj
                )
                tool_calls.append(
                    ToolCallSpec(
                        id=b.get("id") or "",
                        type="function",
                        function=ToolFunctionSpec(
                            name=b.get("name", "") or "",
                            arguments=args_str,
                        ),
                    )
                )

        usage_raw = data.get("usage") or {}
        u = usage_raw if isinstance(usage_raw, dict) else _to_dict(usage_raw)
        prompt = u.get("input_tokens", 0) or 0
        completion = u.get("output_tokens", 0) or 0
        cache_read = (
            u.get("cache_read_input_tokens", 0)
            or u.get("prompt_cache_hit_tokens", 0)
            or 0
        )
        cache_creation = (
            u.get("cache_creation_input_tokens", 0)
            or u.get("prompt_cache_miss_tokens", 0)
            or 0
        )
        usage = Usage(
            prompt_tokens=prompt,
            completion_tokens=completion,
            total_tokens=prompt + completion,
            prompt_cache_hit_tokens=cache_read,
            prompt_cache_miss_tokens=cache_creation,
        )

        stop_reason = data.get("stop_reason")
        finish_reason_map: dict[str, Literal["stop", "length", "tool_calls", "content_filter"]] = {
            "end_turn": "stop",
            "stop_sequence": "stop",
            "max_tokens": "length",
            "tool_use": "tool_calls",
        }
        finish_reason = finish_reason_map.get(str(stop_reason)) if stop_reason else None

        total_prompt = usage.prompt_cache_hit_tokens + usage.prompt_cache_miss_tokens
        cache_rate = (
            usage.prompt_cache_hit_tokens / total_prompt if total_prompt > 0 else 0.0
        )

        return LLMResponse(
            content="".join(text_parts),
            reasoning_content=("".join(thinking_parts) or None),
            tool_calls=tool_calls or None,
            finish_reason=finish_reason,
            usage=usage,
            model=data.get("model") or model,
            cache_hit_rate=cache_rate,
        )

    # ------------------------------------------------------------
    # 公共 API
    # ------------------------------------------------------------

    async def chat(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        stream: bool = False,
        thinking: bool = False,
        reasoning_effort: Literal["low", "medium", "high", "max"] | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """非流式 chat。"""
        # fallback 路径：直接交给 litellm（litellm 内部处理格式转换）
        if self._client is None:
            assert self._fallback is not None
            return await self._fallback.chat(
                messages=messages,
                model=_litellm_model_name(model),
                tools=tools,
                stream=False,
                thinking=thinking,
                reasoning_effort=reasoning_effort,
                **kwargs,
            )

        system, anth_messages = self.messages_to_anthropic(messages)
        req: dict[str, Any] = {
            "model": _strip_anthropic_prefix(model),
            "messages": anth_messages,
            "max_tokens": kwargs.pop("max_tokens", 4096),
        }
        if system is not None:
            req["system"] = system
        anth_tools = self.tools_to_anthropic(tools)
        if anth_tools:
            req["tools"] = anth_tools
        if thinking:
            # Anthropic extended thinking：要求 budget_tokens
            budget = kwargs.pop("thinking_budget_tokens", 4096)
            req["thinking"] = {"type": "enabled", "budget_tokens": budget}

        # 透传剩余 kwargs（temperature / top_p / metadata 等）
        for k, v in kwargs.items():
            req[k] = v

        try:
            raw = await self._client.messages.create(**req)
        except Exception as e:
            raise RuntimeError(f"anthropic.messages.create failed: {e!s}") from e
        return self.anthropic_response_to_openai(raw, model)

    async def chat_stream(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        thinking: bool = False,
        reasoning_effort: Literal["low", "medium", "high", "max"] | None = None,
        **kwargs: Any,
    ) -> AsyncGenerator[LLMResponse, None]:
        """流式 chat。yield 增量 LLMResponse。"""
        if self._client is None:
            assert self._fallback is not None
            async for chunk in self._fallback.chat_stream(
                messages=messages,
                model=_litellm_model_name(model),
                tools=tools,
                thinking=thinking,
                reasoning_effort=reasoning_effort,
                **kwargs,
            ):
                yield chunk
            return

        system, anth_messages = self.messages_to_anthropic(messages)
        req: dict[str, Any] = {
            "model": _strip_anthropic_prefix(model),
            "messages": anth_messages,
            "max_tokens": kwargs.pop("max_tokens", 4096),
            "stream": True,
        }
        if system is not None:
            req["system"] = system
        anth_tools = self.tools_to_anthropic(tools)
        if anth_tools:
            req["tools"] = anth_tools
        if thinking:
            budget = kwargs.pop("thinking_budget_tokens", 4096)
            req["thinking"] = {"type": "enabled", "budget_tokens": budget}
        for k, v in kwargs.items():
            req[k] = v

        try:
            stream = await self._client.messages.create(**req)
        except Exception as e:
            raise RuntimeError(
                f"anthropic.messages.create (stream) failed: {e!s}"
            ) from e

        # Anthropic 流是 event 流（content_block_delta / message_delta 等）
        # 简化策略：累积 text / tool_use input_json，每个 delta 就 yield 一段
        async for ev in stream:
            ev_dict = ev if isinstance(ev, dict) else _to_dict(ev)
            etype = ev_dict.get("type")

            if etype == "content_block_delta":
                delta = ev_dict.get("delta") or {}
                d = delta if isinstance(delta, dict) else _to_dict(delta)
                if d.get("type") == "text_delta":
                    yield LLMResponse(
                        content=d.get("text", "") or "",
                        model=model,
                    )
                elif d.get("type") == "thinking_delta":
                    yield LLMResponse(
                        content="",
                        reasoning_content=d.get("thinking", "") or d.get("text", "") or "",
                        model=model,
                    )
            elif etype == "message_delta":
                # 包含 usage 终态
                usage_raw = ev_dict.get("usage") or {}
                u = usage_raw if isinstance(usage_raw, dict) else _to_dict(usage_raw)
                if u:
                    cache_read = u.get("cache_read_input_tokens", 0) or 0
                    cache_creation = u.get("cache_creation_input_tokens", 0) or 0
                    usage = Usage(
                        prompt_tokens=u.get("input_tokens", 0) or 0,
                        completion_tokens=u.get("output_tokens", 0) or 0,
                        total_tokens=(u.get("input_tokens", 0) or 0)
                        + (u.get("output_tokens", 0) or 0),
                        prompt_cache_hit_tokens=cache_read,
                        prompt_cache_miss_tokens=cache_creation,
                    )
                    total_p = (
                        usage.prompt_cache_hit_tokens
                        + usage.prompt_cache_miss_tokens
                    )
                    rate = (
                        usage.prompt_cache_hit_tokens / total_p if total_p > 0 else 0.0
                    )
                    yield LLMResponse(
                        content="",
                        usage=usage,
                        model=model,
                        cache_hit_rate=rate,
                    )
            elif etype == "message_stop":
                # 终止信号
                return


# ------------------------------------------------------------
# 模块私有 helpers
# ------------------------------------------------------------


def _to_dict(obj: Any) -> dict[str, Any]:
    if obj is None:
        return {}
    if isinstance(obj, dict):
        return obj
    if hasattr(obj, "model_dump"):
        try:
            return obj.model_dump()
        except Exception:  # pragma: no cover - 防御
            pass
    if hasattr(obj, "dict"):
        try:
            return obj.dict()  # type: ignore[no-any-return]
        except Exception:  # pragma: no cover
            pass
    if hasattr(obj, "__dict__"):
        return {k: v for k, v in obj.__dict__.items() if not k.startswith("_")}
    return {}


def _strip_anthropic_prefix(model: str) -> str:
    """把 `anthropic/claude-sonnet-4-5` → `claude-sonnet-4-5`（Anthropic SDK 直连）。"""
    if model.startswith("anthropic/"):
        return model[len("anthropic/") :]
    return model


def _litellm_model_name(model: str) -> str:
    """fallback 走 litellm 时需要 provider 前缀。如果用户传了 claude-xxx，补 anthropic/。"""
    if "/" in model:
        return model
    if model.startswith("claude"):
        return f"anthropic/{model}"
    return model
