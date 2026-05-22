"""do_bash —— 受限 shell 执行。

- 异步子进程，wait_for 超时
- 拦截危险模式
- 输出截断 5000 字符
"""
from __future__ import annotations

import asyncio
import re
import time
from typing import Any

from dscode.core.types import ToolResult, ToolSpec, ToolStatus

SPEC = ToolSpec(
    name="do_bash",
    description=(
        "Execute a shell command and capture stdout/stderr. Dangerous patterns "
        "(rm -rf /, sudo, dd if=*) are blocked. Output truncated to 5000 chars."
    ),
    parameters={
        "type": "object",
        "properties": {
            "command": {"type": "string", "description": "Shell command line."},
            "cwd": {
                "type": ["string", "null"],
                "description": "Working directory; defaults to current.",
                "default": None,
            },
            "timeout": {
                "type": "integer",
                "description": "Timeout in seconds.",
                "default": 60,
                "minimum": 1,
            },
        },
        "required": ["command"],
    },
    capability="code_execute",
    timeout_s=120,
)

_DANGEROUS_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"\brm\s+(-[rRfFvV]*\s+)*(--no-preserve-root\s+)?/(\s|$)"),
    re.compile(r"\brm\s+(-[rRfFvV]*\s+)*(--no-preserve-root\s+)?/\*"),
    re.compile(r"\brm\s+-[rRfFvV]*\s+(--no-preserve-root\s+)?/(\s|$)"),
    re.compile(r"(^|\s|;|&&|\|\|)\s*sudo(\s|$)"),
    re.compile(r"\bdd\s+if="),
    re.compile(r":\(\)\s*\{.*:\|:&\s*\}"),  # fork bomb
    re.compile(r">\s*/dev/sd[a-z]"),
    re.compile(r"\bmkfs\."),
    re.compile(r"\bshutdown\s"),
    re.compile(r"\breboot\b"),
)

_OUTPUT_LIMIT = 5000


def _check_dangerous(command: str) -> str | None:
    """返回触发的危险模式描述，否则 None。"""
    for pat in _DANGEROUS_PATTERNS:
        if pat.search(command):
            return pat.pattern
    return None


def _truncate(text: str, limit: int = _OUTPUT_LIMIT) -> tuple[str, bool]:
    if len(text) <= limit:
        return text, False
    head_len = limit - 200
    return (
        text[:head_len] + f"\n... [truncated {len(text) - limit + 200} chars] ...\n" + text[-200:],
        True,
    )


async def handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    command = args.get("command")
    if not isinstance(command, str) or not command.strip():
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="command is required",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    cwd = args.get("cwd")
    if cwd is not None and not isinstance(cwd, str):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="cwd must be a string or null",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    timeout = int(args.get("timeout", 60) or 60)
    if timeout <= 0:
        timeout = 60

    triggered = _check_dangerous(command)
    if triggered:
        return ToolResult(
            status=ToolStatus.BLOCKED,
            content="",
            error=f"command blocked: matches dangerous pattern `{triggered}`",
            elapsed_ms=int((time.time() - started) * 1000),
            metadata={"blocked_pattern": triggered},
        )

    try:
        proc = await asyncio.create_subprocess_shell(
            command,
            cwd=cwd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
    except OSError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"failed to spawn shell: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    try:
        stdout_b, stderr_b = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except TimeoutError:
        try:
            proc.kill()
        except ProcessLookupError:
            pass
        await proc.wait()
        return ToolResult(
            status=ToolStatus.TIMEOUT,
            content="",
            error=f"command timed out after {timeout}s",
            elapsed_ms=int((time.time() - started) * 1000),
            metadata={"command": command, "timeout": timeout},
        )

    stdout = stdout_b.decode("utf-8", errors="replace")
    stderr = stderr_b.decode("utf-8", errors="replace")
    combined = ""
    if stdout:
        combined += "[stdout]\n" + stdout
    if stderr:
        if combined:
            combined += "\n"
        combined += "[stderr]\n" + stderr
    if not combined:
        combined = "(no output)"

    truncated_text, was_truncated = _truncate(combined)
    rc = proc.returncode if proc.returncode is not None else -1

    status = ToolStatus.SUCCESS if rc == 0 else ToolStatus.ERROR
    error = None if rc == 0 else f"exit code {rc}"

    return ToolResult(
        status=status,
        content=truncated_text,
        error=error,
        elapsed_ms=int((time.time() - started) * 1000),
        metadata={
            "returncode": rc,
            "truncated": was_truncated,
            "command": command,
        },
    )
