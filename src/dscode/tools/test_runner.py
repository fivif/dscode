"""do_test —— 测试框架适配器。

支持 pytest、unittest、npm test、go test。
"""
from __future__ import annotations

import asyncio
import re
import shutil
import time
from typing import Any

from dscode.core.types import ToolResult, ToolSpec, ToolStatus

SPEC = ToolSpec(
    name="do_test",
    description=(
        "Run a test suite (pytest/unittest/npm/go) and return pass/fail summary "
        "plus failure excerpts."
    ),
    parameters={
        "type": "object",
        "properties": {
            "framework": {
                "type": "string",
                "enum": ["pytest", "unittest", "npm", "go"],
                "description": "Test framework.",
                "default": "pytest",
            },
            "path": {
                "type": "string",
                "description": "Test target path.",
                "default": ".",
            },
            "pattern": {
                "type": ["string", "null"],
                "description": "Optional test name pattern.",
                "default": None,
            },
        },
        "required": [],
    },
    capability="test_execute",
    timeout_s=300,
)

_OUTPUT_LIMIT = 5000


def _truncate(text: str) -> str:
    if len(text) <= _OUTPUT_LIMIT:
        return text
    head_len = _OUTPUT_LIMIT - 200
    return text[:head_len] + "\n... [truncated] ...\n" + text[-200:]


def _build_cmd(framework: str, path: str, pattern: str | None) -> list[str] | None:
    if framework == "pytest":
        pytest = shutil.which("pytest") or shutil.which("py.test")
        if pytest:
            cmd = [pytest, path, "-q"]
        else:
            cmd = ["python", "-m", "pytest", path, "-q"]
        if pattern:
            cmd.extend(["-k", pattern])
        return cmd
    if framework == "unittest":
        cmd = ["python", "-m", "unittest", "discover", "-s", path]
        if pattern:
            cmd.extend(["-p", pattern])
        return cmd
    if framework == "npm":
        npm = shutil.which("npm")
        if not npm:
            return None
        cmd = [npm, "test", "--silent"]
        if pattern:
            cmd.extend(["--", "-t", pattern])
        return cmd
    if framework == "go":
        go = shutil.which("go")
        if not go:
            return None
        cmd = [go, "test", "./..." if path == "." else path]
        if pattern:
            cmd.extend(["-run", pattern])
        return cmd
    return None


_PYTEST_SUMMARY = re.compile(
    r"(?P<failed>\d+)\s+failed|"
    r"(?P<passed>\d+)\s+passed|"
    r"(?P<errors>\d+)\s+error"
)


def _parse_summary(framework: str, stdout: str, stderr: str) -> dict[str, int]:
    text = stdout + "\n" + stderr
    summary: dict[str, int] = {"passed": 0, "failed": 0, "errors": 0}
    if framework in ("pytest", "unittest"):
        for m in _PYTEST_SUMMARY.finditer(text):
            for key, val in m.groupdict().items():
                if val is not None:
                    summary[key] = int(val)
    elif framework == "go":
        # go test prints PASS / FAIL counts via lines
        summary["passed"] = len(re.findall(r"^--- PASS:", text, flags=re.MULTILINE))
        summary["failed"] = len(re.findall(r"^--- FAIL:", text, flags=re.MULTILINE))
    return summary


async def handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    framework = args.get("framework", "pytest")
    if framework not in ("pytest", "unittest", "npm", "go"):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"unsupported framework: {framework}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    path = args.get("path", ".") or "."
    pattern = args.get("pattern")

    cmd = _build_cmd(framework, path, pattern)
    if cmd is None:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"{framework} binary not found in PATH",
            elapsed_ms=int((time.time() - started) * 1000),
        )

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
                error=f"tests timed out after {SPEC.timeout_s}s",
                elapsed_ms=int((time.time() - started) * 1000),
                metadata={"framework": framework},
            )
    except FileNotFoundError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"failed to spawn: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    stdout = stdout_b.decode("utf-8", errors="replace")
    stderr = stderr_b.decode("utf-8", errors="replace")
    rc = proc.returncode if proc.returncode is not None else -1

    summary = _parse_summary(framework, stdout, stderr)
    header = (
        f"framework={framework} returncode={rc} "
        f"passed={summary['passed']} failed={summary['failed']} errors={summary['errors']}"
    )

    combined = _truncate(stdout + ("\n" + stderr if stderr else ""))
    content = header + "\n\n" + combined

    status = ToolStatus.SUCCESS if rc == 0 else ToolStatus.ERROR
    error = None if rc == 0 else f"tests failed (rc={rc})"

    return ToolResult(
        status=status,
        content=content,
        error=error,
        elapsed_ms=int((time.time() - started) * 1000),
        metadata={
            "framework": framework,
            "returncode": rc,
            "passed": summary["passed"],
            "failed": summary["failed"],
            "errors": summary["errors"],
        },
    )
