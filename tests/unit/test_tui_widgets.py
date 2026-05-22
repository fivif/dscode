"""TUI widget 单元测试。

测试 ChatPanel / ActivityPanel / StatusBar 的公开 API。
使用 Textual TestApp 上下文挂载 widgets 以支持 RichLog 渲染。
"""
from __future__ import annotations

import pytest
from textual.app import App, ComposeResult

from dscode.tui.chat import ChatPanel
from dscode.tui.activity import ActivityPanel
from dscode.tui.status import StatusBar


# ------------------------------------------------------------------
# Helpers
# ------------------------------------------------------------------

def _last_text(widget) -> str:
    """从 RichLog widget 提取最后写入行的纯文本。"""
    lines = widget.lines
    if not lines:
        return ""
    return lines[-1].text


def _all_text(widget) -> str:
    """从 RichLog widget 提取所有写入行的纯文本（空格连接）。"""
    return " ".join(line.text for line in widget.lines)


# ------------------------------------------------------------------
# ChatPanel
# ------------------------------------------------------------------

@pytest.mark.asyncio
async def test_chat_panel_add_user():
    """用户消息正确显示：包含消息内容。"""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield ChatPanel()

    app = TestApp()
    async with app.run_test() as pilot:
        chat = pilot.app.query_one(ChatPanel)
        chat.add_user_message("Hello World")
        await pilot.pause()
        text = _last_text(chat)
        assert "Hello World" in text


@pytest.mark.asyncio
async def test_chat_panel_stream():
    """流式追加：首 chunk 后 line 数正确增长，add_assistant_end 重置状态。"""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield ChatPanel()

    app = TestApp()
    async with app.run_test() as pilot:
        chat = pilot.app.query_one(ChatPanel)

        # 首个 chunk
        chat.add_assistant_stream("Hello")
        await pilot.pause()
        assert len(chat.lines) == 1

        # 第二个 chunk（同一回复的延续）
        chat.add_assistant_stream(" World")
        await pilot.pause()
        assert len(chat.lines) == 2

        # 结束当前回复
        chat.add_assistant_end()
        assert not chat._streaming

        # 新回复：行数继续增长（流式标志已重置）
        chat.add_assistant_stream("New reply")
        await pilot.pause()
        assert len(chat.lines) == 3


@pytest.mark.asyncio
async def test_chat_panel_system():
    """系统消息正确显示：包含内容。"""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield ChatPanel()

    app = TestApp()
    async with app.run_test() as pilot:
        chat = pilot.app.query_one(ChatPanel)
        chat.add_system("初始化完成")
        await pilot.pause()
        text = _last_text(chat)
        assert "初始化完成" in text


# ------------------------------------------------------------------
# ActivityPanel
# ------------------------------------------------------------------

@pytest.mark.asyncio
async def test_activity_panel_tool_start():
    """工具调用正确显示：包含工具名和参数。"""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield ActivityPanel()

    app = TestApp()
    async with app.run_test() as pilot:
        activity = pilot.app.query_one(ActivityPanel)
        activity.add_tool_start("read_file", "path=/tmp/foo.py")
        await pilot.pause()
        text = _last_text(activity)
        assert "read_file" in text
        assert "path=/tmp/foo.py" in text


@pytest.mark.asyncio
async def test_activity_panel_tool_end():
    """工具结束正确显示：success 和 error 状态均有输出。"""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield ActivityPanel()

    app = TestApp()
    async with app.run_test() as pilot:
        activity = pilot.app.query_one(ActivityPanel)

        # 成功
        activity.add_tool_end("read_file", status="success", elapsed_ms=150)
        await pilot.pause()
        assert len(activity.lines) == 1

        # 失败
        activity.add_tool_end("write_file", status="error")
        await pilot.pause()
        assert len(activity.lines) == 2


@pytest.mark.asyncio
async def test_activity_panel_phase_change():
    """MAGI 阶段切换正确显示：包含 R 编号和阶段名。"""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield ActivityPanel()

    app = TestApp()
    async with app.run_test() as pilot:
        activity = pilot.app.query_one(ActivityPanel)
        activity.add_phase_change("scrutinize", round_num=3, summary="检查代码质量")
        await pilot.pause()
        text = _last_text(activity)
        assert "R3" in text
        assert "scrutinize" in text
        assert "检查代码质量" in text


# ------------------------------------------------------------------
# StatusBar
# ------------------------------------------------------------------

@pytest.mark.asyncio
async def test_status_bar_ready():
    """Default values show 'ready'."""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield StatusBar()

    app = TestApp()
    async with app.run_test() as pilot:
        bar = pilot.app.query_one(StatusBar)
        bar.render_status()
        await pilot.pause()
        rendered = str(bar.render())
        assert "ready" in rendered


@pytest.mark.asyncio
async def test_status_bar_with_data():
    """有数据时显示模型、缓存、轮次等信息。"""

    class TestApp(App[None]):
        def compose(self) -> ComposeResult:
            yield StatusBar()

    app = TestApp()
    async with app.run_test() as pilot:
        bar = pilot.app.query_one(StatusBar)
        bar.render_status(
            model="deepseek-v4",
            cache_hit_rate=0.75,
            cost_saved=12.50,
            round_num=5,
            phase="execute",
            call_count=10,
        )
        await pilot.pause()
        rendered = str(bar.render())
        assert "deepseek-v4" in rendered
        assert "75" in rendered  # 0.75 * 100 = 75%
        assert "12.50" in rendered
        assert "R5" in rendered
        assert "execute" in rendered
