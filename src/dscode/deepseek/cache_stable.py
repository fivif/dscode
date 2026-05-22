"""缓存稳定性消息装配器。

DeepSeek 的 prompt caching 按"前缀完全相同"匹配。本模块严格规范了
消息装配顺序，把所有"任务内不变"的内容钉死在前缀，把"会变的"放尾部，
从而最大化缓存命中率。

装配顺序（前 7 块是稳定前缀，最后 2 块是滚动尾部）：
    1. system_prompt
    2. spec_block       —— .dscode/spec/ 注入的项目规范
    3. tools_block      —— 工具定义文本化
    4. repo_summary     —— 仓库摘要（session 内不变）
    5. warm_memory      —— L2 facts（任务内不变）
    6. cold_memory      —— L3 patterns（任务内不变）
    7. task_prd         —— 当前 PRD（任务内不变）
    --- 以上属于稳定前缀，fingerprint 据此计算 ---
    8. round_history    —— MAGI 轮次历史（每轮追加）
    9. current_turn     —— 当前轮内的 thought / tool_call / tool_result
"""
from __future__ import annotations

import hashlib
from typing import Any

from dscode.core.types import Message

# 稳定前缀块的固定分隔标记。注入到内容中，使 fingerprint 抗碰撞且便于调试。
_BLOCK_MARKERS = (
    "<|system_prompt|>",
    "<|spec_block|>",
    "<|tools_block|>",
    "<|repo_summary|>",
    "<|warm_memory|>",
    "<|cold_memory|>",
    "<|task_prd|>",
)


class CacheStableAssembler:
    """按固定顺序装配消息列表，确保前缀稳定以最大化 DeepSeek prompt caching 命中率。"""

    def __init__(
        self,
        system_prompt: str = "",
        spec_block: str = "",
        tools_block: str = "",
        repo_summary: str = "",
        warm_memory: str = "",
        cold_memory: str = "",
        task_prd: str = "",
        round_history: list[Message] | None = None,
        current_turn: list[Message] | None = None,
    ) -> None:
        """初始化装配器。

        所有 *_block 都是文本块（在 system 角色或 user 角色中拼接），
        round_history / current_turn 是已经构造好的 Message 列表（含 user/assistant/tool）。

        Args:
            system_prompt: 主系统提示（钉死，session 内不变）。
            spec_block: 项目规范文本（来自 .dscode/spec/）。
            tools_block: 工具定义文本化（虽然 OpenAI tools 通过 tools= 参数传，
                         这里以文本形式入 prompt 是为了 cache key 稳定）。
            repo_summary: 仓库摘要（session 内不变）。
            warm_memory: L2 已验证事实（任务内不变）。
            cold_memory: L3 已学习模式（任务内不变）。
            task_prd: 当前 PRD（任务内不变）。
            round_history: 之前 MAGI 轮次的 Message。
            current_turn: 当前轮内累积的 Message。
        """
        self.system_prompt = system_prompt
        self.spec_block = spec_block
        self.tools_block = tools_block
        self.repo_summary = repo_summary
        self.warm_memory = warm_memory
        self.cold_memory = cold_memory
        self.task_prd = task_prd
        self.round_history: list[Message] = list(round_history or [])
        self.current_turn: list[Message] = list(current_turn or [])

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    def _prefix_blocks(self) -> tuple[str, ...]:
        """前 7 个稳定块的有序文本。"""
        return (
            self.system_prompt,
            self.spec_block,
            self.tools_block,
            self.repo_summary,
            self.warm_memory,
            self.cold_memory,
            self.task_prd,
        )

    def _build_system_text(self) -> str:
        """把前 7 个块拼成单段 system 文本。"""
        parts: list[str] = []
        for marker, body in zip(_BLOCK_MARKERS, self._prefix_blocks(), strict=True):
            parts.append(marker)
            parts.append(body or "")
        return "\n".join(parts)

    # ------------------------------------------------------------
    # 公共 API
    # ------------------------------------------------------------

    def assemble(self, **_: Any) -> list[Message]:
        """组装最终的 Message 列表。

        前 7 块合并为一个 system message（保证 cache key 完全一致），
        之后追加 round_history，再追加 current_turn。

        Returns:
            可直接喂给 LLMProviderProtocol.chat 的 Message 列表。
        """
        messages: list[Message] = [
            Message(role="system", content=self._build_system_text()),
        ]
        messages.extend(self.round_history)
        messages.extend(self.current_turn)
        return messages

    def compute_prefix_fingerprint(self) -> str:
        """对前 7 个稳定块计算 SHA256 指纹。

        用途：在 telemetry / 断言中验证"前缀稳定"，
        若 fingerprint 在轮间变化，说明缓存会失效，应该报警。

        Returns:
            64 字符的十六进制 SHA256 摘要。
        """
        h = hashlib.sha256()
        for marker, body in zip(_BLOCK_MARKERS, self._prefix_blocks(), strict=True):
            h.update(marker.encode("utf-8"))
            h.update(b"\x1f")  # ASCII unit separator
            h.update((body or "").encode("utf-8"))
            h.update(b"\x1e")  # ASCII record separator
        return h.hexdigest()

    def total_token_estimate(self) -> int:
        """粗略估算总 token 数（字符数 / 3）。

        DeepSeek 中文 ~1.5 char/token，英文 ~4 char/token，
        混合内容取 3 为经验值。

        Returns:
            估算的 token 数。
        """
        total_chars = 0
        for body in self._prefix_blocks():
            total_chars += len(body or "")
        for msg in self.round_history:
            total_chars += len(msg.content or "")
            if msg.reasoning_content:
                total_chars += len(msg.reasoning_content)
        for msg in self.current_turn:
            total_chars += len(msg.content or "")
            if msg.reasoning_content:
                total_chars += len(msg.reasoning_content)
        return total_chars // 3
