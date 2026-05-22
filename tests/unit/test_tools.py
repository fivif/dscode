"""Tools 单元测试。

每个工具的：成功路径 + 错误路径 + 安全拒绝路径。
"""
from __future__ import annotations

from pathlib import Path

import pytest

from dscode.core.types import ToolStatus
from dscode.safety.fail_closed import fail_closed_check
from dscode.safety.file_guard import FileGuard
from dscode.tools import build_default_registry
from dscode.tools.bash import handler as bash_handler
from dscode.tools.file_ops import patch_handler, read_handler, write_handler
from dscode.tools.git_ops import handler as git_handler
from dscode.tools.grep import handler as grep_handler
from dscode.tools.lsp_query import handler as lsp_handler
from dscode.tools.side_git import rollback_handler, snapshot_handler
from dscode.tools.test_runner import handler as run_test_suite

# ============================================================
# Registry
# ============================================================

def test_default_registry_has_all_tools():
    reg = build_default_registry()
    names = {s.name for s in reg.list_specs()}
    expected = {
        "do_grep",
        "do_file_read",
        "do_file_write",
        "do_file_patch",
        "do_bash",
        "do_test",
        "do_git",
        "do_snapshot",
        "do_rollback",
        "do_lsp_query",
    }
    assert expected.issubset(names)
    assert len(reg.list_specs()) >= 8


def test_registry_to_openai_tools_shape():
    reg = build_default_registry()
    tools = reg.to_openai_tools()
    assert all(t["type"] == "function" for t in tools)
    sample = tools[0]
    assert "name" in sample["function"]
    assert "description" in sample["function"]
    assert "parameters" in sample["function"]


def test_registry_get_handler():
    reg = build_default_registry()
    assert reg.get_handler("do_grep") is not None
    assert reg.get_handler("nonexistent") is None


# ============================================================
# grep
# ============================================================

@pytest.mark.asyncio
async def test_grep_success(tmp_path: Path):
    f = tmp_path / "sample.txt"
    f.write_text("alpha\nbeta\ngamma\nalpha-two\n")
    res = await grep_handler({"pattern": "alpha", "path": str(tmp_path)})
    assert res.status == ToolStatus.SUCCESS
    assert "alpha" in res.content


@pytest.mark.asyncio
async def test_grep_no_match(tmp_path: Path):
    f = tmp_path / "sample.txt"
    f.write_text("hello world\n")
    res = await grep_handler({"pattern": "xyz_not_here", "path": str(tmp_path)})
    assert res.status == ToolStatus.SUCCESS
    # 没匹配时给个友好提示
    assert "no matches" in res.content or res.content == ""


@pytest.mark.asyncio
async def test_grep_missing_pattern():
    res = await grep_handler({})
    assert res.status == ToolStatus.ERROR
    assert "pattern" in (res.error or "").lower()


# ============================================================
# file_read
# ============================================================

@pytest.mark.asyncio
async def test_file_read_success(tmp_path: Path):
    f = tmp_path / "t.txt"
    f.write_text("line1\nline2\nline3\n")
    res = await read_handler({"path": str(f)})
    assert res.status == ToolStatus.SUCCESS
    assert "line1" in res.content
    assert "line3" in res.content


@pytest.mark.asyncio
async def test_file_read_with_offset_limit(tmp_path: Path):
    f = tmp_path / "t.txt"
    f.write_text("\n".join(f"line{i}" for i in range(20)) + "\n")
    res = await read_handler({"path": str(f), "offset": 5, "limit": 3})
    assert res.status == ToolStatus.SUCCESS
    assert "line5" in res.content
    assert "line7" in res.content
    assert "line8" not in res.content
    assert "line4" not in res.content


@pytest.mark.asyncio
async def test_file_read_missing():
    res = await read_handler({"path": "/nonexistent/path/xxx.txt"})
    assert res.status == ToolStatus.ERROR
    assert "not found" in (res.error or "")


# ============================================================
# file_write
# ============================================================

@pytest.mark.asyncio
async def test_file_write_new(tmp_path: Path):
    target = tmp_path / "new.txt"
    res = await write_handler({"path": str(target), "content": "hello\n"})
    assert res.status == ToolStatus.SUCCESS
    assert target.read_text() == "hello\n"


@pytest.mark.asyncio
async def test_file_write_existing_rejected(tmp_path: Path):
    target = tmp_path / "exists.txt"
    target.write_text("old\n")
    res = await write_handler({"path": str(target), "content": "new\n"})
    assert res.status == ToolStatus.ERROR
    assert "already exists" in (res.error or "")


@pytest.mark.asyncio
async def test_file_write_blocked_system_path():
    res = await write_handler({"path": "/etc/passwd", "content": "x"})
    assert res.status == ToolStatus.BLOCKED


# ============================================================
# file_patch
# ============================================================

@pytest.mark.asyncio
async def test_file_patch_success(tmp_path: Path):
    f = tmp_path / "p.txt"
    f.write_text("hello world\n")
    res = await patch_handler(
        {"path": str(f), "old_string": "world", "new_string": "claude"}
    )
    assert res.status == ToolStatus.SUCCESS
    assert f.read_text() == "hello claude\n"


@pytest.mark.asyncio
async def test_file_patch_multiple_matches_without_replace_all(tmp_path: Path):
    f = tmp_path / "p.txt"
    f.write_text("foo foo foo\n")
    res = await patch_handler(
        {"path": str(f), "old_string": "foo", "new_string": "bar"}
    )
    assert res.status == ToolStatus.ERROR
    assert "matches" in (res.error or "").lower()
    # 文件未被修改
    assert f.read_text() == "foo foo foo\n"


@pytest.mark.asyncio
async def test_file_patch_replace_all(tmp_path: Path):
    f = tmp_path / "p.txt"
    f.write_text("foo foo foo\n")
    res = await patch_handler(
        {
            "path": str(f),
            "old_string": "foo",
            "new_string": "bar",
            "replace_all": True,
        }
    )
    assert res.status == ToolStatus.SUCCESS
    assert f.read_text() == "bar bar bar\n"


@pytest.mark.asyncio
async def test_file_patch_not_found(tmp_path: Path):
    f = tmp_path / "p.txt"
    f.write_text("hello\n")
    res = await patch_handler(
        {"path": str(f), "old_string": "MISSING", "new_string": "x"}
    )
    assert res.status == ToolStatus.ERROR
    assert "not found" in (res.error or "").lower()


@pytest.mark.asyncio
async def test_file_patch_file_missing(tmp_path: Path):
    res = await patch_handler(
        {
            "path": str(tmp_path / "nope.txt"),
            "old_string": "a",
            "new_string": "b",
        }
    )
    assert res.status == ToolStatus.ERROR


# ============================================================
# bash
# ============================================================

@pytest.mark.asyncio
async def test_bash_success():
    res = await bash_handler({"command": "echo hello-dscode"})
    assert res.status == ToolStatus.SUCCESS
    assert "hello-dscode" in res.content


@pytest.mark.asyncio
async def test_bash_blocks_rm_rf_root():
    res = await bash_handler({"command": "rm -rf /"})
    assert res.status == ToolStatus.BLOCKED


@pytest.mark.asyncio
async def test_bash_blocks_sudo():
    res = await bash_handler({"command": "sudo ls"})
    assert res.status == ToolStatus.BLOCKED


@pytest.mark.asyncio
async def test_bash_blocks_dd_if():
    res = await bash_handler({"command": "dd if=/dev/zero of=/tmp/x"})
    assert res.status == ToolStatus.BLOCKED


@pytest.mark.asyncio
async def test_bash_timeout():
    res = await bash_handler({"command": "sleep 5", "timeout": 1})
    assert res.status == ToolStatus.TIMEOUT


@pytest.mark.asyncio
async def test_bash_exit_code_error():
    res = await bash_handler({"command": "false"})
    assert res.status == ToolStatus.ERROR


# ============================================================
# test runner
# ============================================================

@pytest.mark.asyncio
async def test_test_runner_pytest_pass(tmp_path: Path):
    test_file = tmp_path / "test_smoke.py"
    test_file.write_text(
        "def test_one():\n    assert 1 == 1\n"
    )
    res = await run_test_suite({"framework": "pytest", "path": str(test_file)})
    # 不强求 SUCCESS（pytest 可能不在环境中），但至少不应崩溃
    assert res.status in (ToolStatus.SUCCESS, ToolStatus.ERROR)


@pytest.mark.asyncio
async def test_test_runner_unsupported_framework():
    res = await run_test_suite({"framework": "mocha"})
    assert res.status == ToolStatus.ERROR


# ============================================================
# git ops
# ============================================================

@pytest.mark.asyncio
async def test_git_status_runs():
    # 不假设当前在 git repo，仅检查不被 BLOCKED
    res = await git_handler({"op": "status"})
    assert res.status in (ToolStatus.SUCCESS, ToolStatus.ERROR)


@pytest.mark.asyncio
async def test_git_blocks_unknown_op():
    res = await git_handler({"op": "push"})
    assert res.status == ToolStatus.BLOCKED


@pytest.mark.asyncio
async def test_git_blocks_force_arg():
    res = await git_handler({"op": "branch", "args": ["--force"]})
    assert res.status == ToolStatus.BLOCKED


@pytest.mark.asyncio
async def test_git_blocks_hard_arg():
    res = await git_handler({"op": "diff", "args": ["--hard"]})
    assert res.status == ToolStatus.BLOCKED


# ============================================================
# side_git: snapshot + rollback
# ============================================================

@pytest.mark.asyncio
async def test_snapshot_and_rollback_roundtrip(tmp_path: Path, monkeypatch):
    project = tmp_path / "proj"
    project.mkdir()
    (project / "a.txt").write_text("alpha\n")
    sub = project / "sub"
    sub.mkdir()
    (sub / "b.txt").write_text("beta\n")
    # 排除目录
    (project / ".git").mkdir()
    (project / ".git" / "ignored").write_text("ignored\n")

    monkeypatch.chdir(project)

    snap_res = await snapshot_handler({"round_id": "round1", "message": "initial"})
    assert snap_res.status == ToolStatus.SUCCESS
    snap_file = project / ".dscode" / "snapshots" / "round1.tar.zst"
    assert snap_file.exists()

    # 修改后回滚
    (project / "a.txt").write_text("MODIFIED\n")

    # 不带 confirm 应当返回 dry-run
    dry = await rollback_handler({"snapshot_id": "round1"})
    assert dry.status == ToolStatus.SUCCESS
    assert "DRY-RUN" in dry.content

    # 带 confirm
    apply_res = await rollback_handler({"snapshot_id": "round1", "confirm": True})
    assert apply_res.status == ToolStatus.SUCCESS
    assert (project / "a.txt").read_text() == "alpha\n"
    assert (project / "sub" / "b.txt").read_text() == "beta\n"
    # .git 没被回滚（因为被排除了）
    assert (project / ".git" / "ignored").read_text() == "ignored\n"


@pytest.mark.asyncio
async def test_rollback_missing_snapshot(tmp_path: Path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    res = await rollback_handler({"snapshot_id": "nope"})
    assert res.status == ToolStatus.ERROR


# ============================================================
# lsp
# ============================================================

@pytest.mark.asyncio
async def test_lsp_query_python(tmp_path: Path):
    f = tmp_path / "mod.py"
    f.write_text(
        "X = 1\n"
        "def hello():\n    pass\n"
        "class Foo:\n    def bar(self):\n        pass\n"
    )
    res = await lsp_handler({"path": str(f)})
    assert res.status == ToolStatus.SUCCESS
    assert "hello" in res.content
    assert "Foo" in res.content
    assert "X" in res.content


@pytest.mark.asyncio
async def test_lsp_query_nonpython(tmp_path: Path):
    f = tmp_path / "x.txt"
    f.write_text("hello")
    res = await lsp_handler({"path": str(f)})
    assert res.status == ToolStatus.ERROR


@pytest.mark.asyncio
async def test_lsp_query_syntax_error(tmp_path: Path):
    f = tmp_path / "bad.py"
    f.write_text("def : invalid\n")
    res = await lsp_handler({"path": str(f)})
    assert res.status == ToolStatus.ERROR


# ============================================================
# safety: file_guard
# ============================================================

def test_file_guard_blocks_etc():
    guard = FileGuard()
    decision = guard.check_write("/etc/passwd")
    assert decision.denied


def test_file_guard_blocks_ssh():
    guard = FileGuard()
    decision = guard.check_write(str(Path.home() / ".ssh" / "config"))
    assert decision.denied


def test_file_guard_blocks_path_escape(tmp_path: Path):
    guard = FileGuard(cwd=str(tmp_path))
    decision = guard.check_write("../../etc/whatever")
    assert decision.denied


def test_file_guard_allows_in_cwd(tmp_path: Path):
    guard = FileGuard(cwd=str(tmp_path))
    decision = guard.check_write(str(tmp_path / "sub" / "ok.txt"))
    assert decision.allowed


def test_file_guard_blocks_dscode_spec(tmp_path: Path):
    guard = FileGuard(cwd=str(tmp_path))
    decision = guard.check_write(str(tmp_path / ".dscode" / "spec" / "x.yaml"))
    assert decision.denied


def test_file_guard_unsafe_overrides(tmp_path: Path):
    guard = FileGuard(cwd=str(tmp_path), unsafe=True)
    decision = guard.check_write(str(tmp_path / ".dscode" / "spec" / "x.yaml"))
    assert decision.allowed


# ============================================================
# safety: fail_closed
# ============================================================

def test_fail_closed_unknown_tool():
    reg = build_default_registry()
    decision = fail_closed_check("nonexistent_tool", {}, reg)
    assert decision.denied


def test_fail_closed_missing_required_args():
    reg = build_default_registry()
    decision = fail_closed_check("do_grep", {}, reg)  # missing pattern
    assert decision.denied


def test_fail_closed_ok():
    reg = build_default_registry()
    decision = fail_closed_check("do_grep", {"pattern": "x"}, reg)
    assert decision.allowed
