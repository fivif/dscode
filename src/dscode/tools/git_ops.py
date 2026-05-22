"""do_git —— 受限 git 只读 / 状态查询。

仅允许查询类操作。破坏性操作返回 BLOCKED。
"""
from __future__ import annotations

import asyncio
import re
import shutil
import time
from typing import Any

from dscode.core.types import ToolResult, ToolSpec, ToolStatus

SPEC = ToolSpec(
    name="do_git",
    description=(
        "Read-only git operations: status / diff / log / show / branch. "
        "Destructive ops (push --force, reset --hard, checkout --, clean -f) are blocked."
    ),
    parameters={
        "type": "object",
        "properties": {
            "op": {
                "type": "string",
                "enum": ["status", "diff", "log", "show", "branch"],
                "description": "Git subcommand.",
            },
            "args": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Extra arguments to git subcommand.",
                "default": [],
            },
        },
        "required": ["op"],
    },
    capability="vcs_read",
    timeout_s=30,
)

_ALLOWED_OPS = {"status", "diff", "log", "show", "branch"}

# 即使 op 在允许列表内，也禁止以下危险 args 关键词
_FORBIDDEN_ARG_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"^--force$"),
    re.compile(r"^-f$"),
    re.compile(r"^--hard$"),
    re.compile(r"^--delete$"),
    re.compile(r"^-D$"),
)

_OUTPUT_LIMIT = 5000


def _truncate(text: str) -> str:
    if len(text) <= _OUTPUT_LIMIT:
        return text
    return text[: _OUTPUT_LIMIT - 50] + "\n... [truncated]"


async def handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    op = args.get("op")
    if op not in _ALLOWED_OPS:
        return ToolResult(
            status=ToolStatus.BLOCKED,
            content="",
            error=f"git op `{op}` not allowed; allowed: {sorted(_ALLOWED_OPS)}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    git_args = args.get("args", [])
    if not isinstance(git_args, list):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="args must be a list of strings",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    git_args = [str(a) for a in git_args]

    for arg in git_args:
        for pat in _FORBIDDEN_ARG_PATTERNS:
            if pat.search(arg):
                return ToolResult(
                    status=ToolStatus.BLOCKED,
                    content="",
                    error=f"forbidden git argument: `{arg}`",
                    elapsed_ms=int((time.time() - started) * 1000),
                    metadata={"blocked_arg": arg},
                )

    git_bin = shutil.which("git")
    if not git_bin:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="git binary not found",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    cmd = [git_bin, op, *git_args]
    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        try:
            stdout_b, stderr_b = await asyncio.wait_for(
                proc.communicate(), timeout=SPEC.timeout_s
            )
        except TimeoutError:
            try:
                proc.kill()
            except ProcessLookupError:
                pass
            await proc.wait()
            return ToolResult(
                status=ToolStatus.TIMEOUT,
                content="",
                error=f"git timed out after {SPEC.timeout_s}s",
                elapsed_ms=int((time.time() - started) * 1000),
            )
    except OSError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"failed to spawn git: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    stdout = stdout_b.decode("utf-8", errors="replace")
    stderr = stderr_b.decode("utf-8", errors="replace")
    rc = proc.returncode if proc.returncode is not None else -1

    body = stdout if stdout else "(empty)"
    if stderr:
        body += "\n[stderr]\n" + stderr
    body = _truncate(body)

    status = ToolStatus.SUCCESS if rc == 0 else ToolStatus.ERROR
    error = None if rc == 0 else f"git exited with code {rc}"

    return ToolResult(
        status=status,
        content=body,
        error=error,
        elapsed_ms=int((time.time() - started) * 1000),
        metadata={"op": op, "returncode": rc},
    )
