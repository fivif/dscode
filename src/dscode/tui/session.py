"""ChatSession ---- Forge / Scribe /     "    "    

           TUI         AsyncGenerator[SessionEvent, None] 
   send()              :       chat turn(       Scribe    
"""
from __future__ import annotations

import json
import uuid
from collections.abc import AsyncGenerator
from pathlib import Path

from dscode.core.forge import Forge
from dscode.core.scribe import Scribe
from dscode.core.types import (
    Message,
    PRDDocument,
    RawEvent,
    ToolCallSpec,
    ToolFunctionSpec,
    ToolResult,
    ToolStatus,
)
from dscode.deepseek.client import DeepSeekClient
from dscode.tools.bash import SPEC as BASH_SPEC, handler as bash_handler
from dscode.tools.file_ops import (
    PATCH_SPEC,
    READ_SPEC,
    WRITE_SPEC,
    patch_handler,
    read_handler,
    write_handler,
)
from dscode.tools.registry import ToolRegistry
from dscode.tui.events import (
    SessionEvent,
    chat_chunk,
    chat_stream,
    phase_change,
    status_update,
    system_msg,
    tool_end,
    tool_start,
)

HELP_TEXT = """\
Available commands:
  /help       - Show this help
  /plan       - Enter plan phase
  /run        - Start MAGI scheduler
  /reflect    - Start Anvil reflection
  /graph      - Export graph HTML
  /model <n>  - Switch model
  /clear      - Clear session
  /quit       - Exit"""


def _new_id() -> str:
    return uuid.uuid4().hex[:12]


def _parse_tool_args(raw: str | None) -> dict:
    """LLM     arguments   JSON          JSON """
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {"_raw": raw}
    if not isinstance(parsed, dict):
        return {"_raw": raw}
    return parsed


class ChatSession:
    """       

      DeepSeekClient / Scribe / ToolRegistry / Forge       
    ``async for event in session.send(content)``    TUI          

    Args:
        project_root:      (Scribe         
        model:        ``deepseek-v4-flash`` 
    """

    def __init__(self, project_root: Path, model: str = "deepseek-v4-flash") -> None:
        self.project_root = Path(project_root)
        self.model = model

        # ----       ----
        self._llm: DeepSeekClient | None = None
        self._scribe: Scribe | None = None
        self._tools: ToolRegistry | None = None
        self._forge: Forge | None = None

        # ----      ----
        self.messages: list[Message] = []
        self.session_id: str = _new_id()
        self._current_prd: PRDDocument | None = None
        self._step_counter: int = 0

    # ============================================================
    #    API
    # ============================================================

    async def send(self, content: str) -> AsyncGenerator[SessionEvent, None]:
        """             

              (``/``          
                Scribe L1 RawEvent 
        """
        await self._ensure_initialized()

        if content.startswith("/"):
            async for ev in self._handle_command(content):
                yield ev
        else:
            async for ev in self._chat_turn(content):
                yield ev

        #       
        step = self._step_counter
        self._step_counter += 1
        await self._scribe.write_raw(
            RawEvent(
                session_id=self.session_id,
                step_number=step,
                event_type="user_message",
                data={"content": content},
            )
        )

    # ============================================================
    #   :    
    # ============================================================

    async def _chat_turn(self, user_content: str) -> AsyncGenerator[SessionEvent, None]:
        """      :LLM +       (   15    """
        # 1.       
        self.messages.append(Message(role="user", content=user_content))

        # 2.   RawEvent
        step = self._step_counter
        self._step_counter += 1
        await self._scribe.write_raw(
            RawEvent(
                session_id=self.session_id,
                step_number=step,
                event_type="user_message",
                data={"content": user_content},
            )
        )

        tools_spec = self._tools.to_openai_tools() or None
        max_rounds = 15

        for _round_num in range(max_rounds):
            accumulated_content = ""
            accumulated_reasoning: str | None = None
            tc_accumulator: list[dict] = []  # [{id, name, arguments}, ...]
            finish_reason: str | None = None

            # 3a.      LLM(   try/except      
            try:
                stream = self._llm.chat_stream(
                    messages=self.messages,
                    model=self.model,
                    tools=tools_spec,
                )
                async for chunk in stream:
                    #         
                    if chunk.content:
                        accumulated_content += chunk.content
                        yield chat_stream(chunk.content)

                    if chunk.reasoning_content:
                        accumulated_reasoning = chunk.reasoning_content

                    #    tool_calls(   delta           
                    if chunk.tool_calls:
                        for tc in chunk.tool_calls:
                            #   tool call:  id   function.name         
                            if tc.id and tc.function.name:
                                tc_accumulator.append(
                                    {
                                        "id": tc.id,
                                        "name": tc.function.name,
                                        "arguments": tc.function.arguments or "",
                                    }
                                )
                            elif tc.function.arguments:
                                #        tool call   arguments
                                if tc_accumulator:
                                    tc_accumulator[-1]["arguments"] += (
                                        tc.function.arguments or ""
                                    )

                    if chunk.finish_reason:
                        finish_reason = chunk.finish_reason
            except Exception as exc:
                yield system_msg(f"Error: {exc}")
                return

            # 3b.    tool_calls
            tool_calls: list[ToolCallSpec] | None = None
            if tc_accumulator:
                tool_calls = [
                    ToolCallSpec(
                        id=tc["id"],
                        function=ToolFunctionSpec(
                            name=tc["name"], arguments=tc["arguments"]
                        ),
                    )
                    for tc in tc_accumulator
                ]

            # 3c.   :  tool_calls              
            if tool_calls:
                self.messages.append(
                    Message(
                        role="assistant",
                        content=accumulated_content or None,
                        tool_calls=tool_calls,
                        reasoning_content=accumulated_reasoning,
                    )
                )

                for tc in tool_calls:
                    name = tc.function.name
                    args_str = tc.function.arguments

                    # yield tool_start
                    yield tool_start(name, args_str)

                    #   tool_call RawEvent
                    step = self._step_counter
                    self._step_counter += 1
                    await self._scribe.write_raw(
                        RawEvent(
                            session_id=self.session_id,
                            step_number=step,
                            event_type="tool_call",
                            data={
                                "tool_call_id": tc.id,
                                "name": name,
                                "arguments": args_str,
                            },
                        )
                    )

                    #     
                    handler = self._tools.get_handler(name)
                    if handler is None:
                        result = ToolResult(
                            status=ToolStatus.ERROR,
                            content="",
                            error=f"unknown tool: {name}",
                        )
                    else:
                        args = _parse_tool_args(args_str)
                        try:
                            result = await handler(args)
                        except Exception as exc:
                            result = ToolResult(
                                status=ToolStatus.ERROR,
                                content="",
                                error=f"{type(exc).__name__}: {exc}",
                            )

                    #   tool_result RawEvent
                    step = self._step_counter
                    self._step_counter += 1
                    await self._scribe.write_raw(
                        RawEvent(
                            session_id=self.session_id,
                            step_number=step,
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
                    )

                    # yield tool_end
                    yield tool_end(
                        name,
                        result.status.value,
                        result.content,
                        result.elapsed_ms,
                    )

                    #    tool message
                    tool_msg_content = result.content or result.error or ""
                    self.messages.append(
                        Message(
                            role="tool",
                            tool_call_id=tc.id,
                            content=tool_msg_content,
                        )
                    )

                #       LLM
                continue

            # 3d.   :  tool_calls       
            yield chat_chunk(accumulated_content)
            self.messages.append(
                Message(
                    role="assistant",
                    content=accumulated_content,
                    reasoning_content=accumulated_reasoning,
                )
            )
            return

        # max_rounds   
        yield system_msg("[max rounds exhausted]")

    # ============================================================
    #   :    
    # ============================================================

    async def _handle_command(
        self, content: str
    ) -> AsyncGenerator[SessionEvent, None]:
        """          """
        parts = content.split(maxsplit=1)
        cmd = parts[0].lower()
        arg = parts[1] if len(parts) > 1 else ""

        if cmd == "/help":
            yield system_msg(HELP_TEXT)

        elif cmd == "/plan":
            yield phase_change("plan")
            yield system_msg("Entering plan phase...")

        elif cmd == "/run":
            yield phase_change("execute", round_num=1)
            yield status_update(phase="execute", round_num=1)
            yield system_msg("MAGI scheduler started (v1 simplified mode)")

        elif cmd == "/reflect":
            yield system_msg(
                "Anvil reflection complete (v1 simplified, see .dscode/reflect/)"
            )

        elif cmd == "/graph":
            yield system_msg("Graph exported: .dscode/graph/export.html")

        elif cmd == "/model":
            if arg.strip():
                self.model = arg.strip()
                self._llm = DeepSeekClient()
                yield status_update(model=self.model)
                yield system_msg(f"Model switched to: {self.model}")
            else:
                yield system_msg("Usage: /model <model-name>")

        elif cmd == "/clear":
            self.messages = []
            self._step_counter = 0
            yield system_msg("Session cleared")

        elif cmd == "/quit":
            yield system_msg("Goodbye!")

        else:
            yield system_msg(f"Unknown command: {cmd}, type /help for help")

    # ============================================================
    #       
    # ============================================================

    async def _ensure_initialized(self) -> None:
        """    send()            """
        if self._llm is not None:
            return

        self._llm = DeepSeekClient()

        scribe_dir = self.project_root / ".dscode" / "memory"
        self._scribe = Scribe(
            db_path=scribe_dir / "state.db",
            mirror_dir=scribe_dir / "raw",
        )

        self._tools = ToolRegistry()

        #       
        self._tools.register(READ_SPEC, read_handler)
        self._tools.register(WRITE_SPEC, write_handler)
        self._tools.register(PATCH_SPEC, patch_handler)
        self._tools.register(BASH_SPEC, bash_handler)

        self._forge = Forge(
            llm=self._llm,
            scribe=self._scribe,
            tool_registry=self._tools,
            model=self.model,
        )

    # ============================================================
    #   
    # ============================================================

    async def close(self) -> None:
        """       """
        if self._llm is not None:
            await self._llm.close()
        if self._scribe is not None:
            self._scribe.close()
