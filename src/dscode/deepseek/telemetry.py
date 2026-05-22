"""缓存命中率监控。

按 DeepSeek v4-flash 定价估算节省的成本：
- 缓存命中：¥0.02 / M tokens
- 缓存未命中：¥1 / M tokens
- 节省 = (¥1 - ¥0.02) * hit_tokens / 1_000_000

支持持久化到 .dscode/telemetry.json。
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from dscode.core.types import Usage

# v4-flash 定价（人民币 / 百万 tokens）
PRICE_HIT_CNY_PER_MTOK = 0.02
PRICE_MISS_CNY_PER_MTOK = 1.0


class CacheTelemetry:
    """累计缓存命中数据，并格式化为状态栏可显示文本。"""

    def __init__(
        self,
        persist_path: str | Path | None = None,
        *,
        load_existing: bool = True,
    ) -> None:
        """初始化 telemetry。

        Args:
            persist_path: 持久化路径（默认 None 不持久化）。
                          典型值 `.dscode/telemetry.json`。
            load_existing: 若 persist_path 存在，是否加载。
        """
        self.persist_path: Path | None = Path(persist_path) if persist_path else None
        self.total_hit_tokens: int = 0
        self.total_miss_tokens: int = 0
        self.total_prompt_tokens: int = 0
        self.total_completion_tokens: int = 0
        self.call_count: int = 0

        if load_existing and self.persist_path and self.persist_path.exists():
            try:
                self._load()
            except (OSError, json.JSONDecodeError):
                # 持久化文件损坏：忽略，从零开始
                pass

    # ------------------------------------------------------------
    # 累计
    # ------------------------------------------------------------

    def record(self, usage: Usage) -> None:
        """记录一次 LLM 调用的 token 消耗。"""
        self.total_hit_tokens += usage.prompt_cache_hit_tokens
        self.total_miss_tokens += usage.prompt_cache_miss_tokens
        self.total_prompt_tokens += usage.prompt_tokens
        self.total_completion_tokens += usage.completion_tokens
        self.call_count += 1
        if self.persist_path is not None:
            self._save()

    def reset(self) -> None:
        """清零所有计数。"""
        self.total_hit_tokens = 0
        self.total_miss_tokens = 0
        self.total_prompt_tokens = 0
        self.total_completion_tokens = 0
        self.call_count = 0
        if self.persist_path is not None:
            self._save()

    # ------------------------------------------------------------
    # 派生指标
    # ------------------------------------------------------------

    @property
    def total_prompt_cache_tokens(self) -> int:
        """命中 + 未命中的总和（即所有 prompt tokens）。"""
        return self.total_hit_tokens + self.total_miss_tokens

    @property
    def hit_rate(self) -> float:
        """缓存命中率（0.0 - 1.0）。"""
        total = self.total_prompt_cache_tokens
        return self.total_hit_tokens / total if total > 0 else 0.0

    @property
    def total_saved_cny(self) -> float:
        """节省的金额（人民币）。

        基于 v4-flash 价格：
            saved = (miss_price - hit_price) * hit_tokens / 1M
        """
        delta = PRICE_MISS_CNY_PER_MTOK - PRICE_HIT_CNY_PER_MTOK
        return delta * self.total_hit_tokens / 1_000_000

    @property
    def total_cost_cny(self) -> float:
        """实际消耗金额（人民币）。仅 prompt 端粗算，未含 completion。"""
        return (
            PRICE_HIT_CNY_PER_MTOK * self.total_hit_tokens / 1_000_000
            + PRICE_MISS_CNY_PER_MTOK * self.total_miss_tokens / 1_000_000
        )

    # ------------------------------------------------------------
    # 格式化
    # ------------------------------------------------------------

    @staticmethod
    def _format_tokens(n: int) -> str:
        """1234567 -> '1.2M', 12345 -> '12.3K'."""
        if n >= 1_000_000:
            return f"{n / 1_000_000:.1f}M"
        if n >= 1_000:
            return f"{n / 1_000:.1f}K"
        return str(n)

    def format_statusline(self) -> str:
        """渲染状态栏字符串。

        Returns:
            形如 `[Cache: 87.3% | Saved: ¥12.45 | Tokens: 1.2M]`。
        """
        rate_pct = self.hit_rate * 100
        saved = self.total_saved_cny
        tok = self._format_tokens(self.total_prompt_cache_tokens)
        return f"[Cache: {rate_pct:.1f}% | Saved: ¥{saved:.2f} | Tokens: {tok}]"

    # ------------------------------------------------------------
    # 持久化
    # ------------------------------------------------------------

    def to_dict(self) -> dict[str, Any]:
        """导出为 dict（便于序列化）。"""
        return {
            "total_hit_tokens": self.total_hit_tokens,
            "total_miss_tokens": self.total_miss_tokens,
            "total_prompt_tokens": self.total_prompt_tokens,
            "total_completion_tokens": self.total_completion_tokens,
            "call_count": self.call_count,
            "hit_rate": self.hit_rate,
            "total_saved_cny": self.total_saved_cny,
        }

    def _save(self) -> None:
        """写入持久化文件（原子写）。"""
        assert self.persist_path is not None
        self.persist_path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.persist_path.with_suffix(self.persist_path.suffix + ".tmp")
        tmp.write_text(json.dumps(self.to_dict(), ensure_ascii=False, indent=2))
        tmp.replace(self.persist_path)

    def _load(self) -> None:
        """从持久化文件读取。"""
        assert self.persist_path is not None
        data = json.loads(self.persist_path.read_text())
        self.total_hit_tokens = int(data.get("total_hit_tokens", 0))
        self.total_miss_tokens = int(data.get("total_miss_tokens", 0))
        self.total_prompt_tokens = int(data.get("total_prompt_tokens", 0))
        self.total_completion_tokens = int(data.get("total_completion_tokens", 0))
        self.call_count = int(data.get("call_count", 0))
