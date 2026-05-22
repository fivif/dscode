"""Textual TUI    --          

  :
-    Header
-    :  70% ChatPanel +   30% ActivityPanel
-    Input     + StatusBar    

   :Input   ChatSession.send()   AsyncGenerator[SessionEvent]   widgets
"""
from __future__ import annotations

import asyncio
from pathlib import Path
from typing import ClassVar

from textual.app import App, ComposeResult
from textual.containers import Container
from textual.reactive import reactive
from textual.widgets import Header, Input

from dscode.tui.chat import ChatPanel
from dscode.tui.activity import ActivityPanel
from dscode.tui.status import StatusBar
from dscode.tui.events import SessionEventType
try:
    from dscode.tui.session import ChatSession  # noqa: F811
except ImportError:
    ChatSession = None  # type: ignore[assignment]


class DSCodeApp(App[None]):
    """DS Code     TUI 

         +     +     
       ChatSession     SessionEvent   
    """

    CSS_PATH: ClassVar[str | Path] = "dscode.tcss"

    BINDINGS: ClassVar[list] = [
        ("ctrl+c", "interrupt", "Stop"),
        ("ctrl+l", "clear", "Clear"),
        ("ctrl+d", "quit", "Quit"),
    ]

    #     (reactive     watcher   
    cache_hit_rate: reactive[float] = reactive(0.0)
    saved_cny: reactive[float] = reactive(0.0)
    call_count: reactive[int] = reactive(0)
    current_round: reactive[int] = reactive(0)
    current_phase: reactive[str] = reactive("idle")

    def __init__(
        self,
        project_root: Path,
        model: str | None = None,
        telemetry_path: Path | None = None,
    ) -> None:
        super().__init__()
        self.project_root = Path(project_root)
        self.model = model
        self.telemetry_path = telemetry_path or (
            project_root / ".dscode" / "telemetry.json"
        )
        self._chat: ChatPanel | None = None
        self._activity: ActivityPanel | None = None
        self._status: StatusBar | None = None
        self._input: Input | None = None
        self._session: ChatSession | None = None
        self._process_task: asyncio.Task | None = None

    # ------------------------------------------------------------------
    #   
    # ------------------------------------------------------------------

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        with Container(id="main"):
            yield ChatPanel(id="chat")
            yield ActivityPanel(id="activity")
        yield Input(placeholder="Type a message... (Ctrl+D quit, Ctrl+L clear)", id="input")
        yield StatusBar(id="status")

    def on_mount(self) -> None:
        self._chat = self.query_one("#chat", ChatPanel)
        self._activity = self.query_one("#activity", ActivityPanel)
        self._status = self.query_one("#status", StatusBar)
        self._input = self.query_one("#input", Input)

        #      
        try:
            self._session = ChatSession(self.project_root, self.model)
        except Exception:
            self._session = None

        #        
        self._status.render_status()

        #      +     
        self._refresh_telemetry()
        self.set_interval(2.0, self._refresh_telemetry)

        #     
        self._chat.add_system("DS Code interactive mode started")
        self._chat.add_system("Type /help for command list")

        #      
        if self._input is not None:
            self._input.focus()

    # ------------------------------------------------------------------
    #     
    # ------------------------------------------------------------------

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """       """
        if self._input is None or self._chat is None:
            return
        text = event.value.strip()
        if not text:
            return

        self._input.clear()

        #     
        if text.startswith("/"):
            self._handle_command(text)
            return

        #     
        self._chat.add_user_message(text)

        if self._session is not None:
            self._process_task = asyncio.create_task(self._process(text))

    def _handle_command(self, text: str) -> None:
        """   /         """
        if self._chat is None:
            return
        cmd = text.lower()
        if cmd in ("/help", "/?"):
            self._chat.add_system("Commands:")
            self._chat.add_system("  /help   - Show help")
            self._chat.add_system("  /clear  - Clear chat")
            self._chat.add_system("  /quit   - Quit TUI")
            self._chat.add_system("  Ctrl+C  - Interrupt")
            self._chat.add_system("  Ctrl+L  - Clear screen")
            self._chat.add_system("  Ctrl+D  - Quit")
        elif cmd == "/clear":
            self.action_clear()
        elif cmd == "/quit":
            self.action_quit()
        else:
            self._chat.add_system(f"Unknown command: {text}, type /help for help")

    async def _process(self, text: str) -> None:
        """            ChatSession     """
        if self._session is None:
            self._chat.add_system("[red]Session not initialized[/]")
            return
        try:
            async for evt in self._session.send(text):
                self._dispatch_event(evt)
        except asyncio.CancelledError:
            self._chat.add_system("Interrupted")
        except Exception as e:
            if self._chat is not None:
                self._chat.add_system(f"[red]Error: {e}[/]")
        finally:
            if self._chat is not None:
                self._chat.add_assistant_end()

    # ------------------------------------------------------------------
    #     
    # ------------------------------------------------------------------

    def _dispatch_event(self, evt) -> None:
        """            widget """
        etype = evt.type
        data = evt.data

        if etype == SessionEventType.CHAT_STREAM:
            if self._chat is not None:
                self._chat.add_assistant_stream(data.get("content", ""))
        elif etype == SessionEventType.CHAT_CHUNK:
            if self._chat is not None:
                self._chat.add_assistant_end()
        elif etype == SessionEventType.TOOL_START:
            if self._activity is not None:
                self._activity.add_tool_start(
                    data.get("tool_name", ""),
                    data.get("args", ""),
                )
        elif etype == SessionEventType.TOOL_END:
            if self._activity is not None:
                self._activity.add_tool_end(
                    data.get("tool_name", ""),
                    status=data.get("status", "success"),
                    result=data.get("result", ""),
                    elapsed_ms=data.get("elapsed_ms", 0),
                )
        elif etype == SessionEventType.PHASE_CHANGE:
            if self._activity is not None:
                self._activity.add_phase_change(
                    phase=data.get("phase", ""),
                    round_num=data.get("round_num", 0),
                    summary=data.get("summary", ""),
                )
        elif etype == SessionEventType.STATUS_UPDATE:
            if self._status is not None:
                self._status.render_status(
                    model=data.get("model", ""),
                    cache_hit_rate=data.get("cache_hit_rate", 0.0),
                    cost_saved=data.get("cost_saved", 0.0),
                    round_num=data.get("round_num", 0),
                    phase=data.get("phase", "idle"),
                    call_count=data.get("call_count", 0),
                )
        elif etype == SessionEventType.SYSTEM:
            if self._chat is not None:
                self._chat.add_system(data.get("content", ""))
        elif etype == SessionEventType.INTERRUPTED:
            if self._chat is not None:
                self._chat.add_system("Task interrupted")

        #      UI
        self.refresh()

    # ------------------------------------------------------------------
    # Key Bindings
    # ------------------------------------------------------------------

    def action_interrupt(self) -> None:
        """Ctrl+C:         """
        if self._process_task is not None and not self._process_task.done():
            self._process_task.cancel()
            if self._chat is not None:
                self._chat.add_system("User interrupted")

    def action_clear(self) -> None:
        """Ctrl+L:       """
        if self._chat is not None:
            self._chat.clear()

    def action_quit(self) -> None:
        """Ctrl+D:   TUI """
        self.exit()

    # ------------------------------------------------------------------
    #   
    # ------------------------------------------------------------------

    def _refresh_telemetry(self) -> None:
        """  telemetry.json                """
        if self._status is not None and self.telemetry_path is not None:
            self._status.refresh_from_file(self.telemetry_path)


__all__ = ["DSCodeApp"]
