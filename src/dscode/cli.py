"""DS Code 主 CLI。

设计：
- 所有外部模块（plan / magi / tui / deepseek 客户端等）使用 **延迟 import**——
  确保 `dscode --help` 在 plan/magi 还没写完的阶段也能跑。
- Rich + Typer 提供彩色友好输出。
- 命令清单：init / plan / run / report / reflect / graph / status / tui。
"""
from __future__ import annotations

import json
import shutil
from datetime import UTC
from importlib import resources
from pathlib import Path

import typer
from rich.console import Console
from rich.panel import Panel
from rich.table import Table

app = typer.Typer(
    help="DS Code — DeepSeek-native code agent",
    no_args_is_help=True,
    add_completion=False,
)
console = Console()


# ============================================================
# 初始化 .dscode/ 目录
# ============================================================

_DSCODE_SUBDIRS = [
    "spec",
    "tasks",
    "workspace",
    "snapshots",
    "memory",
    "memory/raw",
    "skills",
]

_DEFAULT_CONFIG_TOML = """\
# DS Code 项目配置
# 完整字段见 src/dscode/config.py

[model]
default = "deepseek-v4-flash"
router = "deepseek-v4-flash"
executor = "deepseek-v4-pro"

[magi]
max_rounds = 20
max_steps = 40

[safety]
unsafe = false

[telemetry]
cache_enabled = true

# [deepseek]
# api_key = "sk-..."           # 推荐用环境变量 DEEPSEEK_API_KEY
# base_url = "https://api.deepseek.com"
"""


def _copy_template(name: str, dest: Path, force: bool) -> bool:
    """从 dscode.templates 复制 markdown 模板到 dest；返回是否写入。"""
    if dest.exists() and not force:
        return False
    try:
        files = resources.files("dscode.templates")
        src = files.joinpath(name)
        text = src.read_text(encoding="utf-8")
    except (FileNotFoundError, OSError, ModuleNotFoundError):
        # 回退：尝试源码路径
        candidate = Path(__file__).parent / "templates" / name
        if not candidate.exists():
            return False
        text = candidate.read_text(encoding="utf-8")
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(text, encoding="utf-8")
    return True


@app.command()
def init(
    project_root: Path = typer.Option(
        Path.cwd(),
        "--project-root",
        "-p",
        help="项目根目录（默认当前目录）。",
    ),
    force: bool = typer.Option(False, "--force", "-f", help="覆盖已有配置 / spec 模板。"),
) -> None:
    """初始化 .dscode/ 目录契约。"""
    root = project_root.resolve()
    dscode = root / ".dscode"
    created: list[str] = []
    skipped: list[str] = []

    # 1) 创建子目录
    for sub in _DSCODE_SUBDIRS:
        p = dscode / sub
        if p.exists():
            skipped.append(f"dir  {p.relative_to(root)}")
        else:
            p.mkdir(parents=True, exist_ok=True)
            created.append(f"dir  {p.relative_to(root)}")

    # 2) 拷贝 spec 模板
    for name in ("conventions.md", "architecture.md", "safety.md"):
        dest = dscode / "spec" / name
        wrote = _copy_template(name, dest, force=force)
        label = f"spec {dest.relative_to(root)}"
        (created if wrote else skipped).append(label)

    # 3) 写示例 config.toml
    cfg_path = dscode / "config.toml"
    if not cfg_path.exists() or force:
        cfg_path.write_text(_DEFAULT_CONFIG_TOML, encoding="utf-8")
        created.append(f"file {cfg_path.relative_to(root)}")
    else:
        skipped.append(f"file {cfg_path.relative_to(root)}")

    # 4) 报告
    table = Table(title=f"dscode init @ {root}", show_lines=False)
    table.add_column("status", style="cyan", no_wrap=True)
    table.add_column("path")
    for c in created:
        table.add_row("created", c)
    for s in skipped:
        table.add_row("skipped", s)
    console.print(table)

    console.print(
        Panel.fit(
            "[green]Done.[/green]  设置 [bold]DEEPSEEK_API_KEY[/bold] 后试试 "
            "[cyan]dscode plan 'task description'[/cyan]。",
            border_style="green",
        )
    )


# ============================================================
# plan
# ============================================================

@app.command()
def plan(
    task: str = typer.Argument(..., help="任务描述。"),
    interactive: bool = typer.Option(
        True,
        "--interactive/--no-interactive",
        help="是否进入 grill-me 深度访谈交互。",
    ),
    project_root: Path = typer.Option(
        Path.cwd(),
        "--project-root",
        "-p",
    ),
) -> None:
    """生成 PRD（grill-me 深度访谈）。"""
    from dscode.config import Config

    cfg = Config.load(project_root=project_root)

    # 延迟 import：plan 模块可能仍在开发
    try:
        from dscode.plan import PlanRunner  # type: ignore[attr-defined]
    except ImportError as e:
        console.print(f"[red]plan 模块尚未就绪：{e}[/red]")
        console.print("[yellow]提示：等待 plan 子团队完成后再用此命令。[/yellow]")
        raise typer.Exit(code=2) from None

    try:
        from dscode.deepseek import DeepSeekClient
    except ImportError as e:
        console.print(f"[red]deepseek 客户端不可用：{e}[/red]")
        raise typer.Exit(code=2) from None

    client = DeepSeekClient(
        api_key=cfg.deepseek_api_key,
        base_url=cfg.deepseek_base_url,
    )

    ask_user = _make_rich_prompt(console) if interactive else None

    import asyncio

    async def _run() -> None:
        runner = PlanRunner(llm=client, project_root=cfg.project_root)
        task_dir = await runner.run(task, ask_user=ask_user)
        task_id = task_dir.name
        prd_path = task_dir / "prd.md"
        console.print(
            Panel.fit(
                f"[green]PRD 已生成[/green]\n"
                f"task_id: [bold]{task_id}[/bold]\n"
                f"path: {prd_path}\n\n"
                f"下一步：[cyan]dscode run {task_id}[/cyan]",
                border_style="green",
            )
        )

    asyncio.run(_run())


def _make_rich_prompt(con: Console):
    """返回一个 async ask_user(prompt: str) -> str 函数，供 PlanRunner 用作访谈回调。"""
    async def ask(prompt: str) -> str:
        con.print(f"[cyan]>>>[/cyan] {prompt}")
        # typer.prompt 是同步的；放进线程里以保证 async 不阻塞 event loop
        import asyncio

        return await asyncio.to_thread(typer.prompt, "你的回答")
    return ask


# ============================================================
# run
# ============================================================

@app.command()
def run(
    task_id: str = typer.Argument(..., help="任务 ID（dscode plan 输出）。"),
    hours: float = typer.Option(
        None, "--hours", help="时长上限（小时）。默认由 PRD.estimated_hours 决定。"
    ),
    max_rounds: int = typer.Option(20, "--max-rounds", help="MAGI 轮数上限。"),
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
) -> None:
    """运行 MAGI 三脑螺旋上升。"""
    from dscode.config import Config

    cfg = Config.load(project_root=project_root)

    task_dir = cfg.tasks_dir / task_id
    prd_json_path = task_dir / "prd.json"
    if not prd_json_path.exists():
        console.print(f"[red]找不到 PRD JSON：{prd_json_path}[/red]")
        console.print("[yellow]先跑 dscode plan 生成 PRD。[/yellow]")
        raise typer.Exit(code=1)

    try:
        from dscode.magi import Executor, MAGIScheduler, Promoter, Scrutinizer
        from dscode.plan import load_prd
    except ImportError as e:
        console.print(f"[red]magi/plan 模块尚未就绪：{e}[/red]")
        raise typer.Exit(code=2) from None

    try:
        from dscode.core import Forge, Scribe
        from dscode.deepseek import DeepSeekClient
        from dscode.tools import build_default_registry
    except ImportError as e:
        console.print(f"[red]核心模块导入失败：{e}[/red]")
        raise typer.Exit(code=2) from None

    client = DeepSeekClient(
        api_key=cfg.deepseek_api_key,
        base_url=cfg.deepseek_base_url,
    )
    scribe = Scribe(db_path=cfg.effective_db_path)
    registry = build_default_registry()
    forge = Forge(
        llm=client,
        scribe=scribe,
        tool_registry=registry,
        model=cfg.default_executor_model,
    )

    prd = load_prd(prd_json_path)

    # spec 文本（注入到 scrutinize / promote 的 prompt）
    spec_text = ""
    try:
        from dscode.plan import SpecLoader
        spec_text = SpecLoader(cfg.project_root).format_for_prompt()
    except Exception:
        pass

    # side_git 快照 handler（可选）
    side_git_handler = None
    try:
        snap_spec = registry.get_handler("do_snapshot")
        if snap_spec is not None:
            side_git_handler = snap_spec
    except Exception:
        pass

    import asyncio
    from datetime import datetime, timedelta

    deadline: datetime | None = None
    if hours is not None:
        deadline = datetime.now(UTC) + timedelta(hours=hours)
    elif prd.estimated_hours:
        deadline = datetime.now(UTC) + timedelta(hours=prd.estimated_hours)

    session_id = f"sess-{task_id}"

    async def _run_magi() -> list:
        scrutinizer = Scrutinizer(llm=client, model=cfg.default_router_model)
        executor = Executor(forge=forge)
        promoter = Promoter(llm=client, model=cfg.default_executor_model)

        scheduler = MAGIScheduler(
            scrutinizer=scrutinizer,
            executor=executor,
            promoter=promoter,
            scribe=scribe,
            side_git_handler=side_git_handler,
        )
        return await scheduler.run(
            prd=prd,
            session_id=session_id,
            deadline=deadline,
            max_rounds=max_rounds,
            spec_text=spec_text,
        )

    console.print(
        Panel.fit(
            f"[cyan]MAGI 启动[/cyan]\n"
            f"task_id: [bold]{task_id}[/bold]\n"
            f"session: {session_id}\n"
            f"max_rounds: {max_rounds}\n"
            f"deadline: {deadline}",
            border_style="cyan",
        )
    )

    history = asyncio.run(_run_magi())

    # 写 magi-log.md
    log_path = task_dir / "magi-log.md"
    log_path.write_text(_format_magi_history(history, task_id), encoding="utf-8")

    # 终端表格
    table = Table(title=f"MAGI 完成 — {len(history)} 轮")
    table.add_column("round", style="cyan", no_wrap=True)
    table.add_column("quality", justify="right")
    table.add_column("tokens", justify="right")
    table.add_column("cache_hit", justify="right")
    table.add_column("stopped?", justify="center")
    for h in history:
        q = h.promote.quality_score if h.promote else 0.0
        t = h.execute_result.tokens_used if h.execute_result else 0
        cache_rate = (
            h.execute_result.cache_hit_rate * 100 if h.execute_result else 0.0
        )
        stopped = "✓" if (h.promote and h.promote.should_stop) else ""
        table.add_row(
            str(h.round_number),
            f"{q:.1f}",
            str(t),
            f"{cache_rate:.1f}%",
            stopped,
        )
    console.print(table)
    console.print(
        Panel.fit(
            f"[green]MAGI 完成[/green]\nlog: {log_path}\n下一步：[cyan]dscode report {task_id}[/cyan]",
            border_style="green",
        )
    )


def _format_magi_history(history: list, task_id: str) -> str:
    """把 MAGIRoundLog 列表渲染为 magi-log.md。"""
    lines: list[str] = [
        f"# MAGI Log — {task_id}",
        "",
        f"轮次数: {len(history)}",
        "",
    ]
    for h in history:
        lines.append(f"## Round {h.round_number}")
        lines.append("")
        if h.scrutinize:
            lines.append("### Scrutinize")
            for q in h.scrutinize.questions:
                lines.append(f"- Q: {q}")
            lines.append(f"- next_action: {h.scrutinize.next_action}")
            if h.scrutinize.risk_flags:
                lines.append(f"- risks: {h.scrutinize.risk_flags}")
            lines.append("")
        if h.execute_result:
            lines.append("### Execute")
            lines.append(f"- success: {h.execute_result.success}")
            lines.append(f"- summary: {h.execute_result.summary}")
            lines.append(f"- steps_taken: {h.execute_result.steps_taken}")
            lines.append(f"- tokens_used: {h.execute_result.tokens_used}")
            lines.append(f"- cache_hit_rate: {h.execute_result.cache_hit_rate:.3f}")
            lines.append(f"- wall_time_ms: {h.execute_result.wall_time_ms}")
            lines.append("")
        if h.promote:
            lines.append("### Promote")
            lines.append(f"- quality_score: {h.promote.quality_score}")
            lines.append(f"- should_stop: {h.promote.should_stop}")
            if h.promote.stop_reason:
                lines.append(f"- stop_reason: {h.promote.stop_reason}")
            if h.promote.next_round_focus:
                lines.append(f"- next_round_focus: {h.promote.next_round_focus}")
            lines.append("")
        if h.snapshot_id:
            lines.append(f"snapshot_id: `{h.snapshot_id}`")
            lines.append("")
    return "\n".join(lines)


# ============================================================
# report
# ============================================================

@app.command()
def report(
    task_id: str = typer.Argument(..., help="任务 ID。"),
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
) -> None:
    """生成验收报告。"""
    from dscode.config import Config

    cfg = Config.load(project_root=project_root)
    log_path = cfg.tasks_dir / task_id / "magi-log.md"

    if not log_path.exists():
        console.print(f"[red]找不到 magi-log：{log_path}[/red]")
        raise typer.Exit(code=1)

    # 简化：把每行展示在表格里；真正解析留给 magi 子团队的格式约定
    table = Table(title=f"MAGI 报告 — {task_id}")
    table.add_column("行号", style="dim", no_wrap=True)
    table.add_column("内容")
    for i, line in enumerate(log_path.read_text(encoding="utf-8").splitlines(), 1):
        table.add_row(str(i), line.rstrip())
    console.print(table)


# ============================================================
# reflect / graph / status
# ============================================================

@app.command()
def reflect(
    session_id: str = typer.Option(
        None, "--session", help="指定 session（默认所有 session）。"
    ),
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
) -> None:
    """触发 Anvil 反思：压缩 + 模式提取 + 矛盾检测 + 升级 candidates。"""
    from dscode.config import Config

    cfg = Config.load(project_root=project_root)

    try:
        from dscode.core import Anvil, Scribe
        from dscode.deepseek import DeepSeekClient
    except ImportError as e:
        console.print(f"[red]Anvil 模块尚未就绪：{e}[/red]")
        console.print("[yellow]Anvil 反思引擎在 v2 实现[/yellow]")
        raise typer.Exit(code=2) from None

    # 能力检测：v1 Anvil 只是占位，缺 run_full_reflection
    if not hasattr(Anvil, "run_full_reflection"):
        console.print("[yellow]Anvil 反思引擎在 v2 实现[/yellow]")
        raise typer.Exit(code=2)

    client = DeepSeekClient(
        api_key=cfg.deepseek_api_key,
        base_url=cfg.deepseek_base_url,
    )
    scribe = Scribe(db_path=cfg.effective_db_path)
    anvil = Anvil(scribe=scribe, llm=client)

    import asyncio

    async def _do() -> None:
        report = await anvil.run_full_reflection(session_id=session_id)  # type: ignore[attr-defined]
        # Rich table 展示报告
        table = Table(title="Anvil 反思报告")
        table.add_column("项")
        table.add_column("值", style="cyan", justify="right")
        compression = getattr(report, "compression", None)
        if compression is not None:
            table.add_row(
                "raw_events 处理",
                str(getattr(compression, "raw_events_processed", 0)),
            )
            table.add_row(
                "facts 抽取",
                str(getattr(compression, "facts_extracted", 0)),
            )
            table.add_row(
                "facts 接受",
                str(getattr(compression, "facts_accepted", 0)),
            )
        table.add_row(
            "patterns 抽取", str(getattr(report, "patterns_extracted", 0))
        )
        table.add_row(
            "patterns 升级", str(getattr(report, "patterns_promoted", 0))
        )
        table.add_row(
            "矛盾发现", str(getattr(report, "contradictions_found", 0))
        )
        table.add_row("耗时 (ms)", str(getattr(report, "elapsed_ms", 0)))
        console.print(table)
        for note in getattr(report, "notes", []) or []:
            console.print(f"[dim]· {note}[/dim]")

    asyncio.run(_do())


@app.command()
def graph(
    export: Path | None = typer.Option(
        None, "--export", help="导出静态 HTML 路径。"
    ),
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
) -> None:
    """构建记忆图谱：4 信号关联 + 社区检测 + 可选 sigma.js HTML 导出。"""
    from dscode.config import Config

    cfg = Config.load(project_root=project_root)

    try:
        from dscode.core import Scribe
        from dscode.graph import GraphBuilder, HTMLExporter  # type: ignore[attr-defined]
    except ImportError as e:
        console.print(f"[red]graph 模块尚未就绪：{e}[/red]")
        console.print("[yellow]记忆图谱可视化在 v2 实现[/yellow]")
        raise typer.Exit(code=2) from None

    scribe = Scribe(db_path=cfg.effective_db_path)
    builder = GraphBuilder(scribe=scribe)

    import asyncio

    async def _do() -> None:
        snapshot = await builder.build()

        # 摘要表
        table = Table(title="记忆图谱")
        table.add_column("指标")
        table.add_column("值", style="cyan", justify="right")
        table.add_row("节点数", str(getattr(snapshot, "node_count", 0)))
        table.add_row("边数", str(getattr(snapshot, "edge_count", 0)))
        communities = getattr(snapshot, "communities", []) or []
        table.add_row("社区数", str(len(communities)))
        console.print(table)

        insights = getattr(snapshot, "insights", []) or []
        if insights:
            console.print("\n[bold]Insights[/bold]")
            for insight in insights:
                console.print(f"  · {insight}")

        # 导出
        if export is not None:
            exporter = HTMLExporter()
            target = exporter.write(snapshot, export.resolve())
            console.print(
                Panel.fit(
                    f"[green]HTML 已导出[/green]\n{target}",
                    border_style="green",
                )
            )
        else:
            default_path = cfg.dscode_dir / "graph.html"
            exporter = HTMLExporter()
            exporter.write(snapshot, default_path)
            console.print(f"[dim]默认导出: {default_path}[/dim]")

    asyncio.run(_do())


@app.command()
def router(
    task: str = typer.Argument(..., help="测试任务描述。"),
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
) -> None:
    """测试 Auto 路由：输入任务描述，看 router 推荐哪个模型。"""
    from dscode.config import Config

    cfg = Config.load(project_root=project_root)

    try:
        from dscode.deepseek import AutoRouter, DeepSeekClient
    except ImportError as e:
        console.print(f"[red]router 模块导入失败：{e}[/red]")
        raise typer.Exit(code=2) from None

    client = DeepSeekClient(
        api_key=cfg.deepseek_api_key,
        base_url=cfg.deepseek_base_url,
    )
    auto_router = AutoRouter(client=client, router_model=cfg.default_router_model)

    import asyncio

    async def _do() -> None:
        decision = await auto_router.route(task)
        table = Table(title=f"Auto 路由决策 — {task[:50]}")
        table.add_column("项")
        table.add_column("值", style="cyan")
        table.add_row("推荐模型", decision.recommended_model)
        table.add_row("thinking", str(decision.thinking))
        table.add_row(
            "reasoning_effort", str(decision.reasoning_effort or "—")
        )
        table.add_row("rationale", decision.rationale or "—")
        console.print(table)

    asyncio.run(_do())


@app.command()
def status(
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
) -> None:
    """显示当前项目状态：spec / tasks 数 / 缓存命中率。"""
    from dscode.config import Config

    cfg = Config.load(project_root=project_root)

    spec_count = (
        sum(1 for _ in cfg.spec_dir.glob("*.md")) if cfg.spec_dir.exists() else 0
    )
    task_count = (
        sum(1 for p in cfg.tasks_dir.iterdir() if p.is_dir())
        if cfg.tasks_dir.exists()
        else 0
    )

    # 缓存遥测
    tel_path = cfg.telemetry_path
    hit_rate_pct = "n/a"
    saved_cny = "n/a"
    call_count: int | str = "n/a"
    if tel_path.exists():
        try:
            data = json.loads(tel_path.read_text(encoding="utf-8"))
            hit_rate_pct = f"{(data.get('hit_rate', 0.0) * 100):.1f}%"
            saved_cny = f"¥{data.get('total_saved_cny', 0.0):.2f}"
            call_count = int(data.get("call_count", 0))
        except (OSError, json.JSONDecodeError, ValueError):
            pass

    table = Table(title=f"DS Code 项目状态 — {cfg.project_root}")
    table.add_column("项")
    table.add_column("值", style="cyan")
    table.add_row("spec 文件数", str(spec_count))
    table.add_row("tasks 数", str(task_count))
    table.add_row("默认模型", cfg.default_model)
    table.add_row("缓存命中率", hit_rate_pct)
    table.add_row("已节省成本", saved_cny)
    table.add_row("LLM 调用次数", str(call_count))
    table.add_row("unsafe 模式", "ON" if cfg.safety_unsafe_mode else "off")
    console.print(table)


# ============================================================
# tui
# ============================================================

@app.command()
def tui(
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
) -> None:
    """Launch the interactive Textual TUI."""
    import os
    import sys

    if sys.stdout.encoding and "utf" not in sys.stdout.encoding.lower():
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    if sys.stderr.encoding and "utf" not in sys.stderr.encoding.lower():
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    os.environ.setdefault("PYTHONIOENCODING", "utf-8")

    from dscode.tui.app import DSCodeApp

    DSCodeApp(project_root=project_root.resolve()).run()


# ============================================================
# bench (A3 — Phase 3)
# ============================================================
# 仅追加：bench_run / bench_compare。不修改任何现有命令。

_BENCH_DEFAULT_SUITE = Path(__file__).resolve().parent.parent.parent / "benchmarks" / "coding_tasks.json"


@app.command("bench-run")
def bench_run(
    suite: str = typer.Option(
        "ALL",
        "--suite",
        help="任务过滤：ALL / A / B / C / 具体 task id 列表（逗号分隔）。",
    ),
    provider: str = typer.Option(
        "deepseek-v4-flash",
        "--provider",
        help="LLM 提供方 / 模型名（用作 cost 估算键）。",
    ),
    output: Path = typer.Option(
        Path("bench_report.json"),
        "--output",
        "-o",
        help="结果 JSON 输出路径。",
    ),
    suite_path: Path = typer.Option(
        _BENCH_DEFAULT_SUITE,
        "--suite-path",
        help="基准任务 JSON 路径（默认 benchmarks/coding_tasks.json）。",
    ),
    project_root: Path = typer.Option(Path.cwd(), "--project-root", "-p"),
    dry_run: bool = typer.Option(
        True,
        "--dry-run/--real-run",
        help="dry-run（默认）：仅 materialize sandbox，不真跑 MAGI。real-run 需要显式指定。",
    ),
) -> None:
    """跑编码基准集，输出 BenchmarkResult JSON。"""
    if not suite_path.exists():
        console.print(f"[red]找不到基准集：{suite_path}[/red]")
        raise typer.Exit(code=1)

    all_tasks: list[dict] = json.loads(suite_path.read_text(encoding="utf-8"))
    tasks = _filter_bench_tasks(all_tasks, suite)
    if not tasks:
        console.print(f"[yellow]suite 过滤后任务为空：{suite}[/yellow]")
        raise typer.Exit(code=1)

    console.print(
        Panel.fit(
            f"[cyan]Bench Run[/cyan]\n"
            f"suite     : {suite} ({len(tasks)} tasks)\n"
            f"provider  : {provider}\n"
            f"mode      : {'dry-run (no MAGI)' if dry_run else 'REAL (max_rounds=3)'}\n"
            f"output    : {output}",
            border_style="cyan",
        )
    )

    # 延迟 import：bench 模块在 Phase 3 才接上
    try:
        from dscode.bench import BenchmarkRunner
    except ImportError as e:
        console.print(f"[red]bench 模块导入失败：{e}[/red]")
        raise typer.Exit(code=2) from None

    # provider 实例：dry-run 用 stub；real-run 用 DeepSeekClient
    if dry_run:
        llm_provider = _StubBenchProvider()
        execute_fn = _bench_dry_execute
    else:
        from dscode.config import Config

        cfg = Config.load(project_root=project_root)
        try:
            from dscode.deepseek import DeepSeekClient
        except ImportError as e:
            console.print(f"[red]deepseek 客户端不可用：{e}[/red]")
            raise typer.Exit(code=2) from None
        llm_provider = DeepSeekClient(
            api_key=cfg.deepseek_api_key,
            base_url=cfg.deepseek_base_url,
        )
        execute_fn = None  # 走默认占位（真实接 MAGI 需要 caller 注入）

    runner = BenchmarkRunner(
        provider=llm_provider,
        project_root=project_root.resolve(),
        model=provider,
        execute_fn=execute_fn,
    )

    import asyncio

    results = asyncio.run(runner.run_suite(tasks))

    # 序列化
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(
            {
                "provider": provider,
                "results": [r.model_dump() for r in results],
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )

    # 终端表格
    table = Table(title=f"Bench Run — {len(results)} tasks")
    table.add_column("task", style="cyan", no_wrap=True)
    table.add_column("cat", justify="left")
    table.add_column("ok", justify="center")
    table.add_column("quality", justify="right")
    table.add_column("tokens", justify="right")
    table.add_column("cost", justify="right")
    table.add_column("cache%", justify="right")
    for r in results:
        table.add_row(
            r.task_id,
            r.category,
            "✓" if r.success else "✗",
            f"{r.quality_score:.0f}",
            str(r.tokens_used),
            f"{r.cost_cny:.4f}",
            f"{r.cache_hit_rate * 100:.0f}%",
        )
    console.print(table)
    console.print(
        Panel.fit(
            f"[green]Bench done.[/green]  Report saved to: {output}",
            border_style="green",
        )
    )


@app.command("bench-compare")
def bench_compare(
    inputs: list[Path] = typer.Option(
        ...,
        "--inputs",
        "-i",
        help="多个 bench_run 输出的 JSON 路径（每个对应一个模型）。",
    ),
    output: Path = typer.Option(
        Path("bench_compare.html"),
        "--output",
        "-o",
        help="HTML 报告输出路径。",
    ),
    markdown: Path | None = typer.Option(
        None, "--markdown", help="（可选）同时输出 Markdown 版报告。"
    ),
) -> None:
    """跨多个 bench-run 报告，生成 HTML / Markdown 对比页。"""
    try:
        from dscode.bench import BenchmarkComparator, BenchmarkResult
    except ImportError as e:
        console.print(f"[red]bench 模块导入失败：{e}[/red]")
        raise typer.Exit(code=2) from None

    bundle: dict[str, list] = {}
    for path in inputs:
        if not path.exists():
            console.print(f"[red]找不到输入：{path}[/red]")
            raise typer.Exit(code=1)
        data = json.loads(path.read_text(encoding="utf-8"))
        provider = data.get("provider") or path.stem
        bundle[provider] = [
            BenchmarkResult.model_validate(r) for r in data.get("results", [])
        ]

    comparator = BenchmarkComparator()
    report = comparator.compare(bundle)

    html_path = comparator.to_html(report, output)
    console.print(f"[green]HTML report:[/green] {html_path}")

    if markdown is not None:
        markdown.parent.mkdir(parents=True, exist_ok=True)
        markdown.write_text(comparator.to_markdown(report), encoding="utf-8")
        console.print(f"[green]Markdown report:[/green] {markdown.resolve()}")

    # 终端排行
    table = Table(title="Ranking")
    table.add_column("axis")
    table.add_column("ordering", style="cyan")
    table.add_row("cost ↑", " → ".join(report.ranking_by_cost))
    table.add_row("quality ↓", " → ".join(report.ranking_by_quality))
    table.add_row("cache ↓", " → ".join(report.ranking_by_cache_hit_rate))
    console.print(table)


def _filter_bench_tasks(tasks: list[dict], suite: str) -> list[dict]:
    """按 ALL / A / B / C / 显式 id 列表过滤。"""
    if not suite or suite.upper() == "ALL":
        return list(tasks)
    if suite.upper() in {"A", "B", "C"}:
        prefix = suite.upper()
        return [t for t in tasks if str(t.get("id", "")).upper().startswith(prefix)]
    wanted = {s.strip() for s in suite.split(",") if s.strip()}
    return [t for t in tasks if t.get("id") in wanted]


class _StubBenchProvider:
    """dry-run 用的零成本 stub provider。绝不真正联网。"""

    async def chat(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        from dscode.core.types import LLMResponse

        return LLMResponse(content="", finish_reason="stop", model=kwargs.get("model", ""))

    async def chat_stream(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        if False:  # pragma: no cover
            yield None  # type: ignore[misc]
        raise NotImplementedError


async def _bench_dry_execute(task, sandbox, provider, model):  # type: ignore[no-untyped-def]
    """dry-run 默认 execute_fn：把 setup.files 视为"已落地的产物"，给一个保底分。"""
    from dscode.core.types import ExecutionResult

    return ExecutionResult(
        success=True,
        summary=f"[dry-run] task={task.get('id')} model={model}",
        steps_taken=1,
        tokens_used=int(task.get("token_budget", 8000) // 10),
        cache_hit_tokens=int(task.get("token_budget", 8000) // 15),
        cache_miss_tokens=int(task.get("token_budget", 8000) // 25),
        wall_time_ms=50,
        tool_call_count=1,
        error_count=0,
    )


# ============================================================
# entry helper
# ============================================================

def _ensure_path_exists(p: Path) -> None:
    """工具方法：保证目录存在；测试 / 内部使用。"""
    p.mkdir(parents=True, exist_ok=True)


# 让 `dscode` 可执行入口与 `python -m dscode` 一致
def main() -> None:  # pragma: no cover
    app()


# 抑制未使用警告
_ = shutil  # 保留 import 以便后续 init 扩展直接用


if __name__ == "__main__":  # pragma: no cover
    main()
