"""do_grep —— 文本搜索工具。

优先 ripgrep，fallback `grep -r`。
"""
from __future__ import annotations

import asyncio
import shutil
import time
from typing import Any

from dscode.core.types import ToolResult, ToolSpec, ToolStatus

SPEC = ToolSpec(
    name="do_grep",
    description=(
        "Search text patterns in files. Uses ripgrep (rg) when available, "
        "else falls back to `grep -r`. Returns at most 50 matching lines."
    ),
    parameters={
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": "Regex / literal pattern to search for.",
            },
            "path": {
                "type": "string",
                "description": "Root path to search in. Defaults to current directory.",
                "default": ".",
            },
            "glob": {
                "type": ["string", "null"],
                "description": "Optional glob filter (e.g. '*.py').",
                "default": None,
            },
            "case_insensitive": {
                "type": "boolean",
                "description": "Case-insensitive search.",
                "default": False,
            },
        },
        "required": ["pattern"],
    },
    capability="file_search",
    timeout_s=30,
)

_MAX_LINES = 50


async def handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    pattern = args.get("pattern")
    if not isinstance(pattern, str) or not pattern:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="pattern is required and must be a non-empty string",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    path = args.get("path", ".")
    glob = args.get("glob")
    case_insensitive = bool(args.get("case_insensitive", False))

    rg = shutil.which("rg")
    if rg:
        cmd = [rg, "--line-number", "--no-heading", "--color", "never"]
        if case_insensitive:
            cmd.append("-i")
        if glob:
            cmd.extend(["--glob", glob])
        cmd.extend([pattern, path])
    else:
        cmd = ["grep", "-rn", "--color=never"]
        if case_insensitive:
            cmd.append("-i")
        if glob:
            cmd.extend(["--include", glob])
        cmd.extend([pattern, path])

    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        try:
            stdout_b, stderr_b = await asyncio.wait_for(proc.communicate(), timeout=SPEC.timeout_s)
        except TimeoutError:
            proc.kill()
            await proc.wait()
            return ToolResult(
                status=ToolStatus.TIMEOUT,
                content="",
                error=f"grep timed out after {SPEC.timeout_s}s",
                elapsed_ms=int((time.time() - started) * 1000),
            )

        stdout = stdout_b.decode("utf-8", errors="replace")
        stderr = stderr_b.decode("utf-8", errors="replace")
        rc = proc.returncode

        lines = stdout.splitlines()
        truncated = False
        if len(lines) > _MAX_LINES:
            lines = lines[:_MAX_LINES]
            truncated = True

        # grep / rg: rc==1 means no matches; rc>1 means error
        if rc == 0 or rc == 1:
            content = "\n".join(lines)
            if truncated:
                content += f"\n... (truncated to {_MAX_LINES} lines)"
            if rc == 1 and not content:
                content = "(no matches)"
            return ToolResult(
                status=ToolStatus.SUCCESS,
                content=content,
                elapsed_ms=int((time.time() - started) * 1000),
                metadata={
                    "tool": "rg" if rg else "grep",
                    "match_count": len(lines),
                    "truncated": truncated,
                },
            )

        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"grep exited with code {rc}: {stderr.strip()}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    except FileNotFoundError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"binary not found: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    except Exception as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"grep failed: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
