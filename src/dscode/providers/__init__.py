"""跨模型 Provider 适配层。

公共导出：
- `LiteLLMProvider`：用 litellm 后端，支持 OpenAI / Gemini / Ollama / Groq 等。
- `AnthropicCompatProvider`：原生 anthropic SDK（或 fallback litellm）。
- `make_provider`：模型字符串 → 合适 Provider 的工厂。

路由规则（`make_provider`）：
- `deepseek-*` 或 `deepseek/*` → `DeepSeekClient`（原生缓存优化路径）
- `anthropic/*` 或 `claude-*` → `AnthropicCompatProvider`
- 其他（`openai/*`, `gemini/*`, `ollama/*`, ...) → `LiteLLMProvider`
"""
from __future__ import annotations

from typing import Any

from dscode.core.types import LLMProviderProtocol
from dscode.providers.anthropic_compat import AnthropicCompatProvider
from dscode.providers.litellm_adapter import LiteLLMProvider

__all__ = [
    "AnthropicCompatProvider",
    "LiteLLMProvider",
    "make_provider",
]


def make_provider(model: str, **kwargs: Any) -> LLMProviderProtocol:
    """根据模型字符串路由到合适的 Provider 实现。

    Args:
        model: 模型字符串。例如 `deepseek-v4-pro`、`anthropic/claude-sonnet-4-5`、
               `openai/gpt-4o`、`gemini/gemini-2.5-pro`、`ollama/qwen3-32b`。
        **kwargs: 透传给 Provider 构造函数（api_key / base_url / timeout 等）。

    Returns:
        实现 LLMProviderProtocol 的 Provider 实例。

    Raises:
        ImportError: 路由到的 Provider 所需的可选依赖未安装。
    """
    name = model.lower()

    # DeepSeek 原生（cache stability + thinking 优化）
    if name.startswith("deepseek-") or name.startswith("deepseek/"):
        # 延迟导入，避免循环以及给 LiteLLM-only 用户保留省启动时间
        from dscode.deepseek.client import DeepSeekClient

        return DeepSeekClient(**kwargs)

    # Anthropic Claude 系列
    if name.startswith("anthropic/") or name.startswith("claude"):
        return AnthropicCompatProvider(**kwargs)

    # 其余：openai / gemini / ollama / groq / mistral / ...
    return LiteLLMProvider(**kwargs)
