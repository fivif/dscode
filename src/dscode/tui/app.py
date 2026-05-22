"""Textual TUI 入口（基础占位版）。

v1 设计：
- 三区布局：左侧 magi 轮次树（占位 Static）+ 中央 RichLog（事件流）+ 底部状态栏。
- statusline 每 2 秒从 `.dscode/telemetry.json` 读一次，实时显示缓存命中率/已节省。
- 通过 `stream_events(...)` 订阅 Forge yield 的 StreamEvent，渲染到中央面板。
- v2 计划：把左侧换成 textual.widgets.Tree，加进度条与轮次切换。
"""
from __future__ import annotations

import json
from collections.abc import AsyncIterable
from pathlib import Path
from typing import ClassVar

from textual.app import App, ComposeResult
from textual.containers import Container, Horizontal
from textual.reactive import reactive
from textual.widgets import Footer, Header, RichLog, Static

from dscode.core.types import StreamEvent, StreamEventType


class DSCodeApp(App[None]):
    """DS Code 基础 TUI。

    3 区布局：左侧 magi 轮次树 + 中央流式事件 + 底部状态栏。
    v1 简化：只做事件流 + 状态栏，左侧树留占位。
    """

    CSS = """
    Screen {
        layout: vertical;
    }
    #main {
        height: 1fr;
    }
    #side-tree {
        width: 28;
        border: solid grey;
        padding: 1 1;
    }
    #event-log {
        border: solid grey;
        padding: 0 1;
    }
    #statusbar {
        height: 1;
        background: $boost;
        color: $text;
    }
    """

    BINDINGS: ClassVar[list] = []

    # 实时统计（reactive，绑定到状态栏）
    cache_hit_rate: reactive[float] = reactive(0.0)
    saved_cny: reactive[float] = reactive(0.0)
    call_count: reactive[int] = reactive(0)
    current_round: reactive[int] = reactive(0)
    current_phase: reactive[str] = reactive("idle")

    def __init__(
        self,
        project_root: Path,
        telemetry_path: Path | None = None,
        refresh_interval: float = 2.0,
    ) -> None:
        super().__init__()
        self.project_root = project_root
        self.telemetry_path = telemetry_path or (
            project_root / ".dscode" / "telemetry.json"
        )
        self.refresh_interval = refresh_interval
        self._log: RichLog | None = None
        self._status: Static | None = None

    # ------------------------------------------------------------
    # 布局
    # ------------------------------------------------------------

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        with Container(id="main"):
            with Horizontal():
                yield Static("MAGI 轮次\n(待执行)", id="side-tree")
                yield RichLog(highlight=True, markup=True, id="event-log")
        yield Static("[ready]", id="statusbar")
        yield Footer()

    def on_mount(self) -> None:
        self._log = self.query_one("#event-log", RichLog)
        self._status = self.query_one("#statusbar", Static)
        self._log.write(f"[dim]project_root={self.project_root}[/dim]")
        self._log.write("[bold cyan]DS Code TUI ready.[/bold cyan]")
        # 立即刷新一次 + 周期刷新
        self.refresh_telemetry()
        self.set_interval(self.refresh_interval, self.refresh_telemetry)
        self._render_status()

    # ------------------------------------------------------------
    # Telemetry 刷新
    # ------------------------------------------------------------

    def refresh_telemetry(self) -> None:
        """从 telemetry.json 读取最新缓存命中数据。

        文件不存在或读取失败时静默忽略（保持上一次的值）。
        """
        try:
            data = json.loads(self.telemetry_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError, ValueError):
            return
        try:
            self.cache_hit_rate = float(data.get("hit_rate") or 0.0)
            self.saved_cny = float(data.get("total_saved_cny") or 0.0)
            self.call_count = int(data.get("call_count") or 0)
        except (TypeError, ValueError):
            return
        self._render_status()

    def update_phase(self, round_number: int, phase: str) -> None:
        """外部把当前 MAGI 轮次/阶段推到状态栏。"""
        self.current_round = round_number
        self.current_phase = phase
        self._render_status()

    def _render_status(self) -> None:
        if self._status is None:
            return
        hit_pct = self.cache_hit_rate * 100
        round_label = (
            f"R{self.current_round}" if self.current_round > 0 else "—"
        )
        line = (
            f"\\[ {round_label} | phase={self.current_phase} | "
            f"cache={hit_pct:.1f}% | saved=¥{self.saved_cny:.2f} | "
            f"calls={self.call_count} \\]"
        )
        self._status.update(line)

    # ------------------------------------------------------------
    # 事件流
    # ------------------------------------------------------------

    async def stream_events(self, events: AsyncIterable[StreamEvent]) -> None:
        """订阅 Forge stream events，渲染到中央面板。"""
        async for event in events:
            self.render_event(event)

    def render_event(self, event: StreamEvent) -> None:
        """单条 StreamEvent → 富文本行。"""
        if self._log is None:
            return
        color = _COLOR_BY_TYPE.get(event.type, "white")
        line = f"[{color}][{event.type.value}][/{color}] {_short_payload(event)}"
        self._log.write(line)
        # 同时刷新一次状态栏（事件触发的轻量刷新）
        self.refresh_telemetry()

    # ------------------------------------------------------------
    # 工具方法
    # ------------------------------------------------------------

    def log_line(self, text: str) -> None:
        """外部直接打印一行（测试 / 调试用）。"""
        if self._log is not None:
            self._log.write(text)


_COLOR_BY_TYPE: dict[StreamEventType, str] = {
    StreamEventType.THOUGHT: "white",
    StreamEventType.TOOL_START: "cyan",
    StreamEventType.TOOL_RESULT: "green",
    StreamEventType.SAFETY_BLOCK: "yellow",
    StreamEventType.ERROR: "red",
    StreamEventType.COMPLETE: "magenta",
    StreamEventType.USAGE: "blue",
}


def _short_payload(event: StreamEvent) -> str:
    data = event.data
    if not data:
        return ""
    # 选若干常用键展示；其余截断
    parts: list[str] = []
    for key in ("name", "content", "status", "error", "summary"):
        val_raw = data.get(key)
        if val_raw:
            val = str(val_raw).replace("\n", " ")
            if len(val) > 120:
                val = val[:117] + "..."
            parts.append(f"{key}={val}")
    return " ".join(parts) if parts else str(data)[:200]


__all__ = ["DSCodeApp"]
