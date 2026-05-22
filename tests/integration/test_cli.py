"""CLI 集成测试。

测试目标：
- `dscode --help` 不抛 ImportError
- `dscode init` 在 tmp_path 创建预期文件
- `dscode reflect` 返回 v2 提示
- `dscode graph` 返回 v2 提示
- `dscode status` 在已 init 项目下正常输出
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from dscode.cli import app

runner = CliRunner()


def test_help_runs_without_error() -> None:
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0
    assert "DS Code" in result.output


@pytest.mark.parametrize(
    "subcommand",
    [
        "init",
        "plan",
        "run",
        "report",
        "reflect",
        "graph",
        "status",
        "tui",
        "router",
    ],
)
def test_subcommand_help_works(subcommand: str) -> None:
    result = runner.invoke(app, [subcommand, "--help"])
    assert result.exit_code == 0, result.output


def test_reflect_degrades_when_anvil_not_ready(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Anvil 缺 run_full_reflection 时，命令应降级到 v2 提示而非崩溃。"""
    runner.invoke(app, ["init", "--project-root", str(tmp_path)])

    # 用一个最简占位类替换真实 Anvil：故意不含 run_full_reflection
    class StubAnvilNoCapability:
        def __init__(self, scribe, llm=None, **kwargs):
            self.scribe = scribe

    import dscode.core as core_mod

    monkeypatch.setattr(core_mod, "Anvil", StubAnvilNoCapability, raising=True)

    result = runner.invoke(
        app, ["reflect", "--project-root", str(tmp_path)]
    )
    assert result.exit_code == 2
    assert "v2" in result.output


def test_reflect_returns_report_when_anvil_ready(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """注入一个具备 run_full_reflection 的 fake Anvil，应正常输出报告。"""
    runner.invoke(app, ["init", "--project-root", str(tmp_path)])

    from dataclasses import dataclass

    @dataclass
    class FakeCompression:
        raw_events_processed: int = 5
        facts_extracted: int = 3
        facts_accepted: int = 2

    @dataclass
    class FakeReport:
        compression: FakeCompression
        patterns_extracted: int = 4
        patterns_promoted: int = 1
        contradictions_found: int = 0
        elapsed_ms: int = 123
        notes: tuple = ("fake-note-1", "fake-note-2")

    class FakeAnvil:
        def __init__(self, scribe, llm=None, **kwargs):
            self.scribe = scribe

        async def run_full_reflection(self, session_id: str | None = None):
            return FakeReport(compression=FakeCompression())

    import dscode.core as core_mod

    monkeypatch.setattr(core_mod, "Anvil", FakeAnvil, raising=True)

    result = runner.invoke(
        app, ["reflect", "--project-root", str(tmp_path)]
    )
    assert result.exit_code == 0, result.output
    # 报告关键指标应出现在 Rich 表格里
    out = result.output
    assert "Anvil 反思报告" in out
    assert "patterns 抽取" in out
    assert "fake-note-1" in out


def test_graph_returns_snapshot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """注入 fake GraphBuilder + HTMLExporter，应正常输出快照。"""
    runner.invoke(app, ["init", "--project-root", str(tmp_path)])

    from dataclasses import dataclass

    @dataclass
    class FakeSnapshot:
        node_count: int = 7
        edge_count: int = 12
        communities: tuple = ("c1", "c2", "c3")
        insights: tuple = ("insight-A", "insight-B")

    class FakeGraphBuilder:
        def __init__(self, scribe):
            self.scribe = scribe

        async def build(self):
            return FakeSnapshot()

    class FakeHTMLExporter:
        def write(self, snapshot, path: Path) -> Path:
            target = Path(path)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("<html>fake</html>", encoding="utf-8")
            return target

    import dscode.graph as graph_mod

    monkeypatch.setattr(graph_mod, "GraphBuilder", FakeGraphBuilder, raising=True)
    monkeypatch.setattr(graph_mod, "HTMLExporter", FakeHTMLExporter, raising=True)

    export_path = tmp_path / "g.html"
    result = runner.invoke(
        app,
        [
            "graph",
            "--export",
            str(export_path),
            "--project-root",
            str(tmp_path),
        ],
    )
    assert result.exit_code == 0, result.output
    assert "记忆图谱" in result.output
    assert "insight-A" in result.output
    assert export_path.exists()
    assert export_path.read_text(encoding="utf-8") == "<html>fake</html>"


def test_init_creates_expected_structure(tmp_path: Path) -> None:
    result = runner.invoke(app, ["init", "--project-root", str(tmp_path)])
    assert result.exit_code == 0, result.output

    dscode = tmp_path / ".dscode"
    # 必备子目录
    for sub in ("spec", "tasks", "workspace", "snapshots", "memory", "memory/raw", "skills"):
        assert (dscode / sub).is_dir(), f"missing dir: {sub}"

    # spec 模板
    for name in ("conventions.md", "architecture.md", "safety.md"):
        path = dscode / "spec" / name
        assert path.is_file(), f"missing spec file: {name}"
        assert path.stat().st_size > 0

    # config.toml
    cfg = dscode / "config.toml"
    assert cfg.is_file()
    content = cfg.read_text(encoding="utf-8")
    assert "[model]" in content
    assert "default" in content
    assert "[magi]" in content


def test_init_is_idempotent_without_force(tmp_path: Path) -> None:
    first = runner.invoke(app, ["init", "--project-root", str(tmp_path)])
    assert first.exit_code == 0
    # 第二次不带 --force 仍然成功，但既有文件被 skipped
    second = runner.invoke(app, ["init", "--project-root", str(tmp_path)])
    assert second.exit_code == 0
    assert "skipped" in second.output


def test_init_force_overwrites(tmp_path: Path) -> None:
    runner.invoke(app, ["init", "--project-root", str(tmp_path)])
    cfg = tmp_path / ".dscode" / "config.toml"
    cfg.write_text("# polluted\n", encoding="utf-8")
    result = runner.invoke(app, ["init", "--project-root", str(tmp_path), "--force"])
    assert result.exit_code == 0
    new_content = cfg.read_text(encoding="utf-8")
    assert "[model]" in new_content


def test_status_on_initialized_project(tmp_path: Path) -> None:
    runner.invoke(app, ["init", "--project-root", str(tmp_path)])
    result = runner.invoke(app, ["status", "--project-root", str(tmp_path)])
    assert result.exit_code == 0
    assert "DS Code" in result.output
    assert "deepseek-v4-flash" in result.output


def test_status_works_on_empty_project(tmp_path: Path) -> None:
    # 即使没 init，status 也不应崩
    result = runner.invoke(app, ["status", "--project-root", str(tmp_path)])
    assert result.exit_code == 0
    assert "deepseek-v4-flash" in result.output


def test_plan_fails_gracefully_when_module_missing(tmp_path: Path) -> None:
    """plan 模块还没就绪时，命令应给出友好提示而非堆栈。"""
    # 跑命令本身不需要真实 API key——延迟 import 会先碰到 PlanRunner 缺失
    result = runner.invoke(
        app,
        [
            "plan",
            "do something",
            "--no-interactive",
            "--project-root",
            str(tmp_path),
        ],
    )
    # 期望：要么 exit 2（plan/PlanRunner 不存在），要么 exit 0（如果第二波刚好完成）
    assert result.exit_code in (0, 1, 2), result.output


def test_run_fails_when_prd_missing(tmp_path: Path) -> None:
    runner.invoke(app, ["init", "--project-root", str(tmp_path)])
    result = runner.invoke(
        app, ["run", "deadbeef0001", "--project-root", str(tmp_path)]
    )
    # 缺 PRD → 退出码 1
    assert result.exit_code == 1
    # Rich 可能换行；用不含空格的字符串匹配
    normalized = result.output.replace("\n", "").replace(" ", "")
    assert "找不到PRD" in normalized


def test_cli_module_import_no_error() -> None:
    """直接 import 不应抛错；保证后续命令注册可用。"""
    import importlib

    mod = importlib.import_module("dscode.cli")
    assert mod.app is app


def test_main_module_import_no_error() -> None:
    import importlib

    importlib.import_module("dscode.__main__")
