"""Strict Tool Calls (DeepSeek Beta)。

DeepSeek 在 beta 端点上支持 `?strict=true` URL 参数，开启后服务端会对工具
schema 做强约束校验、并保证返回的 `tool_calls.arguments` 严格符合 schema：

- `additionalProperties: false`（不允许额外字段）
- 所有 `properties` 必须出现在 `required` 中
- 不允许 schema 出现 oneOf / allOf / not 等非严格构造
- 顶层必须是 `type: "object"`

本模块提供两件事：
1. `validate_tools_schema(tools)` —— 客户端预检 schema，列出违规项。
2. `chat_with_strict_tools(client, ...)` —— 临时切到 `beta?strict=true`
   端点发起一次 chat，并保证返回的 tool_calls 参数已通过严格 schema 校验。

不修改原 `DeepSeekClient`——而是按需构造一个临时 client（拷贝凭证 + 改 base_url）。
"""
from __future__ import annotations

import json
from typing import Any

from dscode.core.types import LLMResponse, Message
from dscode.deepseek.client import BETA_BASE_URL, DeepSeekClient

# 严格 schema 模式下 base_url 上需要追加的 query string
_STRICT_QUERY = "?strict=true"


def _strict_base_url(base_url: str) -> str:
    """把任意 base_url 转换为 beta + ?strict=true 形态。

    规则：
    - 若已是 beta 端点（含 /beta），保留路径，追加/合并 strict=true。
    - 否则强制使用 BETA_BASE_URL 作为根。
    - 已带 strict=true 直接返回原值。
    """
    if "strict=true" in base_url:
        return base_url
    root = base_url if base_url.rstrip("/").endswith("/beta") else BETA_BASE_URL
    sep = "&" if "?" in root else "?"
    return f"{root.rstrip('/')}{sep}strict=true"


def validate_tools_schema(tools: list[dict[str, Any]]) -> list[str]:
    """检查 tools 是否满足 DeepSeek strict 模式要求。

    返回错误描述列表；空列表代表全部合规。
    """
    errors: list[str] = []
    if not isinstance(tools, list):
        return [f"tools 必须为 list，收到 {type(tools).__name__}"]

    forbidden_keys = ("oneOf", "anyOf", "allOf", "not")

    for idx, tool in enumerate(tools):
        prefix = f"tools[{idx}]"
        if not isinstance(tool, dict):
            errors.append(f"{prefix}: 每个工具必须是 dict")
            continue
        if tool.get("type") != "function":
            errors.append(f"{prefix}.type 必须为 'function'")
        fn = tool.get("function")
        if not isinstance(fn, dict):
            errors.append(f"{prefix}.function 必须为 dict")
            continue
        if not isinstance(fn.get("name"), str) or not fn["name"]:
            errors.append(f"{prefix}.function.name 必须为非空字符串")
        params = fn.get("parameters")
        if not isinstance(params, dict):
            errors.append(f"{prefix}.function.parameters 必须为 dict")
            continue

        if params.get("type") != "object":
            errors.append(f"{prefix}.parameters.type 必须为 'object'")
        if params.get("additionalProperties") is not False:
            errors.append(
                f"{prefix}.parameters.additionalProperties 必须显式设为 false"
            )

        props = params.get("properties") or {}
        if not isinstance(props, dict):
            errors.append(f"{prefix}.parameters.properties 必须为 dict")
            props = {}
        required = params.get("required") or []
        if not isinstance(required, list):
            errors.append(f"{prefix}.parameters.required 必须为 list")
            required = []

        # 严格模式：所有 property 必须在 required 中（DeepSeek 文档要求）
        missing_required = [k for k in props.keys() if k not in required]
        if missing_required:
            errors.append(
                f"{prefix}.parameters: 严格模式下所有 properties 都必须出现在 required 中，"
                f"缺失: {missing_required}"
            )
        # required 不能引用未定义的 property
        extra_required = [k for k in required if k not in props]
        if extra_required:
            errors.append(
                f"{prefix}.parameters.required 引用了未定义的 properties: {extra_required}"
            )

        # 禁止非严格构造
        for fk in forbidden_keys:
            if fk in params:
                errors.append(
                    f"{prefix}.parameters 禁止使用 '{fk}'（strict 模式仅支持纯 JSON Schema）"
                )
        for prop_name, prop_schema in props.items():
            if not isinstance(prop_schema, dict):
                errors.append(
                    f"{prefix}.parameters.properties.{prop_name} 必须为 dict"
                )
                continue
            for fk in forbidden_keys:
                if fk in prop_schema:
                    errors.append(
                        f"{prefix}.parameters.properties.{prop_name} "
                        f"禁止使用 '{fk}'"
                    )

    return errors


def _validate_tool_call_args(
    tool_calls: list[Any] | None,
    tools: list[dict[str, Any]],
) -> list[str]:
    """对 LLM 返回的 tool_calls 做最小化字段校验。

    严格模式下 DeepSeek 服务端已保证 schema 合规，这里只做防御性检查：
    - arguments 必须是合法 JSON
    - 顶层必须是 object
    - 所有 required 字段都出现
    """
    if not tool_calls:
        return []
    spec_by_name: dict[str, dict[str, Any]] = {}
    for t in tools:
        fn = t.get("function") or {}
        name = fn.get("name")
        if isinstance(name, str):
            spec_by_name[name] = fn.get("parameters") or {}
    errs: list[str] = []
    for tc in tool_calls:
        name = tc.function.name
        raw = tc.function.arguments or "{}"
        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError as exc:
            errs.append(f"tool_call '{name}' arguments 非合法 JSON: {exc}")
            continue
        if not isinstance(parsed, dict):
            errs.append(f"tool_call '{name}' arguments 顶层必须为 object")
            continue
        params = spec_by_name.get(name) or {}
        required = params.get("required") or []
        for key in required:
            if key not in parsed:
                errs.append(
                    f"tool_call '{name}' 缺少 required 字段 '{key}'"
                )
    return errs


async def chat_with_strict_tools(
    client: DeepSeekClient,
    messages: list[Message],
    tools: list[dict[str, Any]],
    model: str = "deepseek-v4-flash",
    **kwargs: Any,
) -> LLMResponse:
    """启用 DeepSeek Beta 的 strict tool calls 模式发起一次 chat。

    1. 客户端预检 schema：不合规直接 ValueError，不浪费 API 调用。
    2. 临时构造 base_url=`<beta>?strict=true` 的客户端发请求。
    3. 返回 LLMResponse；如服务端兜底失败，做一次本地最小校验。

    Args:
        client: 已有 DeepSeekClient（用其 api_key、timeout 等）。
        messages: 对话历史。
        tools: OpenAI 风格工具定义；必须先通过 `validate_tools_schema`。
        model: 模型名。
        **kwargs: 透传 chat 的其他参数（temperature / max_tokens 等）。

    Returns:
        LLMResponse，其中 tool_calls.arguments 已通过严格 schema 校验。

    Raises:
        ValueError: schema 预检失败或返回结果不符合 schema。
    """
    schema_errors = validate_tools_schema(tools)
    if schema_errors:
        raise ValueError(
            "strict tools schema 预检失败:\n- " + "\n- ".join(schema_errors)
        )

    strict_client = DeepSeekClient(
        api_key=client.api_key,
        base_url=_strict_base_url(client.base_url),
    )

    try:
        resp: LLMResponse = await strict_client.chat(  # type: ignore[assignment]
            messages=messages,
            model=model,
            tools=tools,
            **kwargs,
        )
    finally:
        # 主动关 HTTP client，避免句柄泄漏
        try:
            await strict_client.close()
        except Exception:
            # close 失败不该影响主流程
            pass

    runtime_errors = _validate_tool_call_args(resp.tool_calls, tools)
    if runtime_errors:
        raise ValueError(
            "strict tools 返回不符合 schema:\n- " + "\n- ".join(runtime_errors)
        )
    return resp


__all__ = [
    "chat_with_strict_tools",
    "validate_tools_schema",
]
