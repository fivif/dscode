"""文件操作工具：do_file_read、do_file_write、do_file_patch。

设计原则：
- read 带行号；offset/limit 控制读取范围
- write 仅用于创建新文件，已存在拒绝（鼓励 patch）
- patch 永远不 fallback 覆盖全文件
"""
from __future__ import annotations

import asyncio
import os
import time
from pathlib import Path
from typing import Any

from dscode.core.types import ToolResult, ToolSpec, ToolStatus
from dscode.safety.file_guard import FileGuard

# ============================================================
# do_file_read
# ============================================================

READ_SPEC = ToolSpec(
    name="do_file_read",
    description=(
        "Read a file from local filesystem. Returns content with line numbers. "
        "Use offset/limit for large files."
    ),
    parameters={
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "File path."},
            "offset": {
                "type": "integer",
                "description": "0-based line offset.",
                "default": 0,
                "minimum": 0,
            },
            "limit": {
                "type": "integer",
                "description": "Maximum number of lines to read.",
                "default": 2000,
                "minimum": 1,
            },
        },
        "required": ["path"],
    },
    capability="file_read",
    timeout_s=20,
)


async def read_handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    path = args.get("path")
    if not isinstance(path, str) or not path:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="path is required",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    offset = int(args.get("offset", 0) or 0)
    limit = int(args.get("limit", 2000) or 2000)
    if offset < 0:
        offset = 0
    if limit <= 0:
        limit = 2000

    if not os.path.exists(path):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"file not found: {path}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    if not os.path.isfile(path):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"not a regular file: {path}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    try:
        body, total, end = await asyncio.to_thread(_sync_read, path, offset, limit)
    except OSError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"read failed: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    meta = {"total_lines": total, "returned_lines": end - offset, "offset": offset}
    truncated_note = ""
    if end < total:
        truncated_note = (
            f"\n... (showing lines {offset + 1}-{end} of {total}, "
            f"use offset={end} for next page)"
        )
    return ToolResult(
        status=ToolStatus.SUCCESS,
        content=body + truncated_note,
        elapsed_ms=int((time.time() - started) * 1000),
        metadata=meta,
    )


def _sync_read(path: str, offset: int, limit: int) -> tuple[str, int, int]:
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = f.readlines()
    total = len(lines)
    end = min(total, offset + limit)
    selected = lines[offset:end]
    out_parts: list[str] = []
    for i, line in enumerate(selected, start=offset):
        if not line.endswith("\n"):
            line = line + "\n"
        out_parts.append(f"{i + 1:>6}\t{line}")
    return "".join(out_parts), total, end


# ============================================================
# do_file_write
# ============================================================

WRITE_SPEC = ToolSpec(
    name="do_file_write",
    description=(
        "Write a new file. FAILS if file already exists — use do_file_patch instead. "
        "Creates parent directories as needed."
    ),
    parameters={
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "File path."},
            "content": {"type": "string", "description": "File content."},
        },
        "required": ["path", "content"],
    },
    capability="file_write",
    timeout_s=20,
)


def _sync_write_new(path: str, content: str) -> None:
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    # 'x' = exclusive create, fails if exists
    with open(p, "x", encoding="utf-8") as f:
        f.write(content)


async def write_handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    path = args.get("path")
    content = args.get("content")
    if not isinstance(path, str) or not path:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="path is required",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    if not isinstance(content, str):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="content must be a string",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    # file_guard 校验
    guard = FileGuard()
    decision = guard.check_write(path)
    if not decision.allowed:
        return ToolResult(
            status=ToolStatus.BLOCKED,
            content="",
            error=decision.reason or "write denied by file guard",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    if os.path.exists(path):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=(
                f"file already exists: {path} — use do_file_patch to modify, "
                "never overwrite via do_file_write"
            ),
            elapsed_ms=int((time.time() - started) * 1000),
        )

    try:
        await asyncio.to_thread(_sync_write_new, path, content)
    except FileExistsError:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"file already exists (race): {path}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    except OSError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"write failed: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    byte_len = len(content.encode("utf-8"))
    line_count = content.count("\n") + (0 if content.endswith("\n") or not content else 1)
    return ToolResult(
        status=ToolStatus.SUCCESS,
        content=f"wrote {path} ({line_count} lines, {byte_len} bytes)",
        elapsed_ms=int((time.time() - started) * 1000),
        metadata={"path": path, "bytes": byte_len, "lines": line_count},
    )


# ============================================================
# do_file_patch
# ============================================================

PATCH_SPEC = ToolSpec(
    name="do_file_patch",
    description=(
        "Replace exact string in a file. Multiple matches without replace_all -> error. "
        "old_string not found -> error. NEVER falls back to overwriting the entire file."
    ),
    parameters={
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "File path."},
            "old_string": {
                "type": "string",
                "description": "Exact text to replace.",
            },
            "new_string": {
                "type": "string",
                "description": "Replacement text.",
            },
            "replace_all": {
                "type": "boolean",
                "description": "Replace every occurrence; otherwise must be unique.",
                "default": False,
            },
        },
        "required": ["path", "old_string", "new_string"],
    },
    capability="file_write",
    timeout_s=20,
)


def _sync_patch(
    path: str,
    old_string: str,
    new_string: str,
    replace_all: bool,
) -> tuple[int, str]:
    with open(path, encoding="utf-8") as f:
        original = f.read()

    count = original.count(old_string)
    if count == 0:
        raise ValueError("old_string not found in file")
    if count > 1 and not replace_all:
        raise ValueError(
            f"old_string matches {count} locations; pass replace_all=True or expand context"
        )

    if replace_all:
        new_content = original.replace(old_string, new_string)
        replaced = count
    else:
        new_content = original.replace(old_string, new_string, 1)
        replaced = 1

    # 原子写入：先写 .tmp 再 rename
    tmp = path + ".dscode.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(new_content)
    os.replace(tmp, path)

    diff_summary = (
        f"- {old_string[:80]}{'...' if len(old_string) > 80 else ''}\n"
        f"+ {new_string[:80]}{'...' if len(new_string) > 80 else ''}"
    )
    return replaced, diff_summary


async def patch_handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    path = args.get("path")
    old_string = args.get("old_string")
    new_string = args.get("new_string")
    replace_all = bool(args.get("replace_all", False))

    if not isinstance(path, str) or not path:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="path is required",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    if not isinstance(old_string, str):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="old_string is required (string)",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    if not isinstance(new_string, str):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="new_string is required (string)",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    if old_string == new_string:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="old_string and new_string are identical",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    # file_guard
    guard = FileGuard()
    decision = guard.check_write(path)
    if not decision.allowed:
        return ToolResult(
            status=ToolStatus.BLOCKED,
            content="",
            error=decision.reason or "patch denied by file guard",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    if not os.path.exists(path):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"file not found: {path}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    try:
        replaced, diff = await asyncio.to_thread(
            _sync_patch, path, old_string, new_string, replace_all
        )
    except ValueError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=str(e),
            elapsed_ms=int((time.time() - started) * 1000),
        )
    except OSError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"patch failed: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    return ToolResult(
        status=ToolStatus.SUCCESS,
        content=f"patched {path} ({replaced} replacement{'s' if replaced != 1 else ''})\n{diff}",
        elapsed_ms=int((time.time() - started) * 1000),
        metadata={"path": path, "replacements": replaced, "replace_all": replace_all},
    )
