"""TUI        

SessionBridge(     TUI widgets(             
   agent           
"""
from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Literal


class SessionEventType(str, Enum):
    """       """
    CHAT_STREAM = "chat_stream"        #     (LLM     
    CHAT_CHUNK = "chat_chunk"          #      (    turn   
    TOOL_START = "tool_start"          #       
    TOOL_END = "tool_end"              #     
    PHASE_CHANGE = "phase_change"      # MAGI     
    STATUS_UPDATE = "status_update"    #      
    SYSTEM = "system"                  #     (       
    INTERRUPTED = "interrupted"        #     


@dataclass
class SessionEvent:
    """       """
    type: SessionEventType
    data: dict = field(default_factory=dict)


# ============================================================
#      
# ============================================================

def chat_stream(chunk: str) -> SessionEvent:
    """       """
    return SessionEvent(SessionEventType.CHAT_STREAM, {"content": chunk})


def chat_chunk(content: str, role: str = "assistant") -> SessionEvent:
    """      """
    return SessionEvent(SessionEventType.CHAT_CHUNK, {"content": content, "role": role})


def tool_start(tool_name: str, args: str) -> SessionEvent:
    """     """
    return SessionEvent(SessionEventType.TOOL_START, {"tool_name": tool_name, "args": args})


def tool_end(tool_name: str, status: str, result: str = "", elapsed_ms: int = 0) -> SessionEvent:
    """     """
    return SessionEvent(SessionEventType.TOOL_END, {
        "tool_name": tool_name, "status": status,
        "result": result, "elapsed_ms": elapsed_ms,
    })


def phase_change(phase: str, round_num: int = 0, summary: str = "") -> SessionEvent:
    """MAGI      """
    return SessionEvent(
        SessionEventType.PHASE_CHANGE,
        {"phase": phase, "round_num": round_num, "summary": summary},
    )


def status_update(
    model: str = "",
    cache_hit_rate: float = 0.0,
    cost_saved: float = 0.0,
    round_num: int = 0,
    phase: str = "idle",
    call_count: int = 0,
) -> SessionEvent:
    """      """
    return SessionEvent(SessionEventType.STATUS_UPDATE, {
        "model": model, "cache_hit_rate": cache_hit_rate,
        "cost_saved": cost_saved, "round_num": round_num,
        "phase": phase, "call_count": call_count,
    })


def system_msg(content: str) -> SessionEvent:
    """     """
    return SessionEvent(SessionEventType.SYSTEM, {"content": content})


def interrupted() -> SessionEvent:
    """     """
    return SessionEvent(SessionEventType.INTERRUPTED, {})
