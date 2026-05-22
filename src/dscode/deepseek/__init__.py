"""DeepSeek 优化层。

暴露公共 API：
- DeepSeekClient: OpenAI 兼容客户端，支持 thinking / reasoning_effort / cache 字段
- CacheStableAssembler: 缓存稳定性消息装配器
- CacheTelemetry: 缓存命中率监控
- AutoRouter: Flash/Pro 自动路由
- force_json: prefix completion 强制 JSON 输出
- chat_with_strict_tools / validate_tools_schema: Beta strict tool calls
- chat_with_thinking / append_with_reasoning: thinking + tool loop（reasoning_content 完整回传）
- fim_complete: Fill-In-Middle 补全（光标处场景）
"""
from __future__ import annotations

from dscode.deepseek.auto_router import AutoRouter, RouteDecision
from dscode.deepseek.cache_stable import CacheStableAssembler
from dscode.deepseek.client import BETA_BASE_URL, DEFAULT_BASE_URL, DeepSeekClient
from dscode.deepseek.fim import fim_complete
from dscode.deepseek.prefix_completion import force_json
from dscode.deepseek.strict_tools import chat_with_strict_tools, validate_tools_schema
from dscode.deepseek.telemetry import CacheTelemetry
from dscode.deepseek.thinking import append_with_reasoning, chat_with_thinking

__all__ = [
    "BETA_BASE_URL",
    "DEFAULT_BASE_URL",
    "AutoRouter",
    "CacheStableAssembler",
    "CacheTelemetry",
    "DeepSeekClient",
    "RouteDecision",
    "append_with_reasoning",
    "chat_with_strict_tools",
    "chat_with_thinking",
    "fim_complete",
    "force_json",
    "validate_tools_schema",
]
