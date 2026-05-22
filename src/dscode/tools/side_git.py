"""do_snapshot / do_rollback —— side-git 工作树快照（不污染业务 .git）。

使用 tar + zstd 打包到 .dscode/snapshots/<round_id>.tar.zst。
"""
from __future__ import annotations

import asyncio
import io
import os
import tarfile
import time
from pathlib import Path
from typing import Any

import zstandard as zstd

from dscode.core.types import ToolResult, ToolSpec, ToolStatus

SNAPSHOT_DIR = ".dscode/snapshots"

# 默认排除目录前缀（相对于 cwd 计算 archive name 后的前缀比较）
_DEFAULT_EXCLUDES = (
    ".git",
    "node_modules",
    ".dscode/snapshots",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
)


SNAPSHOT_SPEC = ToolSpec(
    name="do_snapshot",
    description=(
        "Snapshot the current working tree into .dscode/snapshots/<round_id>.tar.zst. "
        "Excludes .git, node_modules, __pycache__, .venv, .dscode/snapshots."
    ),
    parameters={
        "type": "object",
        "properties": {
            "round_id": {
                "type": "string",
                "description": "Unique snapshot identifier (e.g. MAGI round id).",
            },
            "message": {
                "type": "string",
                "description": "Free-form annotation.",
                "default": "",
            },
            "paths": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Paths to include; default = current dir.",
                "default": ["."],
            },
        },
        "required": ["round_id"],
    },
    capability="snapshot",
    timeout_s=120,
)

ROLLBACK_SPEC = ToolSpec(
    name="do_rollback",
    description=(
        "Rollback to a snapshot. Without confirm=True, returns a dry-run summary "
        "of files that would be restored."
    ),
    parameters={
        "type": "object",
        "properties": {
            "snapshot_id": {
                "type": "string",
                "description": "round_id of an existing snapshot.",
            },
            "confirm": {
                "type": "boolean",
                "description": "Set True to actually overwrite files.",
                "default": False,
            },
        },
        "required": ["snapshot_id"],
    },
    capability="snapshot",
    timeout_s=120,
    requires_confirmation=True,
)


def _is_excluded(rel: str) -> bool:
    rel_norm = rel.replace("\\", "/").lstrip("./")
    for ex in _DEFAULT_EXCLUDES:
        if rel_norm == ex or rel_norm.startswith(ex + "/"):
            return True
    return False


def _sync_snapshot(round_id: str, message: str, paths: list[str], cwd: str) -> dict[str, Any]:
    snap_dir = Path(cwd) / SNAPSHOT_DIR
    snap_dir.mkdir(parents=True, exist_ok=True)
    out_path = snap_dir / f"{round_id}.tar.zst"

    files_added = 0
    bytes_uncompressed = 0

    tar_buf = io.BytesIO()
    with tarfile.open(fileobj=tar_buf, mode="w") as tar:
        # 写入元数据
        meta = f"round_id={round_id}\nmessage={message}\ntimestamp={time.time()}\n".encode()
        meta_info = tarfile.TarInfo(name=".dscode_meta")
        meta_info.size = len(meta)
        meta_info.mtime = int(time.time())
        tar.addfile(meta_info, io.BytesIO(meta))

        cwd_resolved = Path(cwd).resolve()
        for p in paths:
            root = Path(cwd) / p if not os.path.isabs(p) else Path(p)
            root = root.resolve()
            if not root.exists():
                continue
            if root.is_file():
                rel = root.relative_to(cwd_resolved)
                tar.add(str(root), arcname=str(rel), recursive=False)
                files_added += 1
                bytes_uncompressed += root.stat().st_size
                continue
            for dirpath, dirnames, filenames in os.walk(root):
                try:
                    rel_dir = Path(dirpath).resolve().relative_to(cwd_resolved)
                except ValueError:
                    continue
                rel_dir_str = str(rel_dir).replace("\\", "/")
                if rel_dir_str != "." and _is_excluded(rel_dir_str):
                    dirnames[:] = []
                    continue
                dirnames[:] = [
                    d
                    for d in dirnames
                    if not _is_excluded(
                        (rel_dir_str + "/" + d) if rel_dir_str != "." else d
                    )
                ]
                for fname in filenames:
                    rel_file = (
                        (rel_dir_str + "/" + fname) if rel_dir_str != "." else fname
                    )
                    if _is_excluded(rel_file):
                        continue
                    full = Path(dirpath) / fname
                    if full.is_symlink():
                        try:
                            tar.add(str(full), arcname=rel_file, recursive=False)
                            files_added += 1
                        except OSError:
                            pass
                        continue
                    if not full.is_file():
                        continue
                    try:
                        tar.add(str(full), arcname=rel_file, recursive=False)
                        files_added += 1
                        bytes_uncompressed += full.stat().st_size
                    except OSError:
                        continue

    raw_tar = tar_buf.getvalue()
    cctx = zstd.ZstdCompressor(level=3)
    compressed = cctx.compress(raw_tar)
    out_path.write_bytes(compressed)

    return {
        "snapshot_id": round_id,
        "path": str(out_path),
        "files": files_added,
        "compressed_bytes": len(compressed),
        "uncompressed_bytes": bytes_uncompressed,
    }


async def snapshot_handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    round_id = args.get("round_id")
    if not isinstance(round_id, str) or not round_id:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="round_id is required",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    message = args.get("message", "") or ""
    paths = args.get("paths") or ["."]
    if not isinstance(paths, list):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="paths must be a list of strings",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    paths = [str(p) for p in paths]

    cwd = os.getcwd()
    try:
        info = await asyncio.to_thread(_sync_snapshot, round_id, message, paths, cwd)
    except OSError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"snapshot failed: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    return ToolResult(
        status=ToolStatus.SUCCESS,
        content=(
            f"snapshot {info['snapshot_id']} written: {info['files']} files, "
            f"{info['compressed_bytes']} bytes compressed "
            f"(uncompressed {info['uncompressed_bytes']} bytes)\n"
            f"path: {info['path']}"
        ),
        elapsed_ms=int((time.time() - started) * 1000),
        metadata=info,
    )


def _sync_rollback_list(snap_path: Path) -> list[str]:
    """列出 tar 内成员（不含 meta 文件）。"""
    dctx = zstd.ZstdDecompressor()
    raw = dctx.decompress(snap_path.read_bytes(), max_output_size=512 * 1024 * 1024)
    members: list[str] = []
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as tar:
        for m in tar:
            if m.name == ".dscode_meta":
                continue
            members.append(m.name)
    return members


def _sync_rollback_apply(snap_path: Path, cwd: str) -> int:
    dctx = zstd.ZstdDecompressor()
    raw = dctx.decompress(snap_path.read_bytes(), max_output_size=512 * 1024 * 1024)
    cwd_resolved = Path(cwd).resolve()
    restored = 0
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as tar:
        for member in tar:
            if member.name == ".dscode_meta":
                continue
            target = (cwd_resolved / member.name).resolve()
            try:
                target.relative_to(cwd_resolved)
            except ValueError:
                continue
            tar.extract(member, path=str(cwd_resolved), set_attrs=False, filter="data")
            restored += 1
    return restored


async def rollback_handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    snapshot_id = args.get("snapshot_id")
    if not isinstance(snapshot_id, str) or not snapshot_id:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="snapshot_id is required",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    confirm = bool(args.get("confirm", False))

    cwd = os.getcwd()
    snap_path = Path(cwd) / SNAPSHOT_DIR / f"{snapshot_id}.tar.zst"
    if not snap_path.exists():
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"snapshot not found: {snap_path}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    if not confirm:
        try:
            members = await asyncio.to_thread(_sync_rollback_list, snap_path)
        except (OSError, tarfile.TarError, zstd.ZstdError) as e:
            return ToolResult(
                status=ToolStatus.ERROR,
                content="",
                error=f"failed to inspect snapshot: {e}",
                elapsed_ms=int((time.time() - started) * 1000),
            )
        preview = members[:30]
        more = "" if len(members) <= 30 else f"\n... +{len(members) - 30} more"
        return ToolResult(
            status=ToolStatus.SUCCESS,
            content=(
                "DRY-RUN: would restore the following files. "
                "Re-call with confirm=true to apply.\n"
                + "\n".join(preview) + more
            ),
            elapsed_ms=int((time.time() - started) * 1000),
            metadata={
                "dry_run": True,
                "snapshot_id": snapshot_id,
                "file_count": len(members),
            },
        )

    try:
        restored = await asyncio.to_thread(_sync_rollback_apply, snap_path, cwd)
    except (OSError, tarfile.TarError, zstd.ZstdError) as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"rollback failed: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    return ToolResult(
        status=ToolStatus.SUCCESS,
        content=f"rolled back snapshot {snapshot_id}: restored {restored} files",
        elapsed_ms=int((time.time() - started) * 1000),
        metadata={"snapshot_id": snapshot_id, "restored": restored},
    )
