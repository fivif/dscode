"""BenchmarkRunner / BenchmarkComparator 单元测试。

约束：绝不真跑 MAGI。所有 execute 行为通过 mock execute_fn 注入。
"""
from __future__ import annotations

import json
from collections.abc import AsyncGenerator
from pathlib import Path
from typing import Any

import pytest

from dscode.bench import (
    BenchmarkComparator,
    BenchmarkResult,
    BenchmarkRunner,
    ComparisonReport,
)
from dscode.bench.runner import estimate_cost_cny
from dscode.core.types import ExecutionResult, LLMResponse, Message

# ============================================================
# Fakes
# ============================================================

class FakeProvider:
    """满足 LLMProviderProtocol 但绝不联网的桩。"""

    async def chat(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        stream: bool = False,
        thinking: bool = False,
        reasoning_effort: Any = None,
        **kwargs: Any,
    ) -> LLMResponse:
        return LLMResponse(content="", finish_reason="stop", model=model)

    async def chat_stream(
        self, *args: Any, **kwargs: Any
    ) -> AsyncGenerator[LLMResponse, None]:  # pragma: no cover
        if False:
            yield  # type: ignore[unreachable]
        raise NotImplementedError


def _make_task(
    task_id: str = "T1",
    category: str = "project_ops",
    files: list[dict] | None = None,
    must_contain: list[str] | None = None,
    must_not_contain: list[str] | None = None,
    iso_group: str | None = None,
    token_budget: int = 8000,
) -> dict:
    return {
        "id": task_id,
        "category": category,
        "title": f"task {task_id}",
        "description": "test",
        "setup": {
            "files": files
            or [{"path": "app.py", "content": "def f():\n    return 1\n"}],
            "deps": [],
        },
        "tools_expected": ["do_file_read"],
        "acceptance": {
            "test_command": "pytest -q",
            "must_contain_files": must_contain or ["app.py"],
            "must_not_contain_patterns": must_not_contain or [],
        },
        "token_budget": token_budget,
        **({"isomorphic_group": iso_group} if iso_group else {}),
    }


# ============================================================
# Mock execute_fn 工厂
# ============================================================

def _mock_execute(success: bool = True, **overrides):
    base = dict(
        success=success,
        summary="ok" if success else "fail",
        steps_taken=3,
        tokens_used=1200,
        cache_hit_tokens=800,
        cache_miss_tokens=200,
        wall_time_ms=42,
        tool_call_count=2,
        error_count=0 if success else 1,
    )
    base.update(overrides)

    async def fn(task, sandbox, provider, model):
        return ExecutionResult(**base)

    return fn


# ============================================================
# BenchmarkRunner
# ============================================================

class TestBenchmarkRunner:
    async def test_run_task_materializes_sandbox_and_scores(
        self, tmp_path: Path
    ) -> None:
        captured: dict[str, Any] = {}

        async def execute_fn(task, sandbox, provider, model):
            captured["sandbox"] = sandbox
            captured["files_present"] = [
                p.name for p in sandbox.rglob("*") if p.is_file()
            ]
            return ExecutionResult(
                success=True,
                summary="materialized",
                steps_taken=2,
                tokens_used=900,
                cache_hit_tokens=600,
                cache_miss_tokens=200,
                wall_time_ms=20,
            )

        runner = BenchmarkRunner(
            provider=FakeProvider(),
            project_root=tmp_path,
            execute_fn=execute_fn,
        )
        task = _make_task(
            files=[
                {"path": "src/foo.py", "content": "x=1\n"},
                {"path": "tests/test_foo.py", "content": "def test_x(): assert True\n"},
            ],
            must_contain=["src/foo.py", "tests/test_foo.py"],
        )
        result = await runner.run_task(task)
        assert isinstance(result, BenchmarkResult)
        assert result.success is True
        # sandbox 在 with 之外应已清理
        assert not captured["sandbox"].exists()
        assert "foo.py" in captured["files_present"]
        assert "test_foo.py" in captured["files_present"]
        assert result.quality_score >= 70.0
        # 缓存命中率：600 / (600+200) = 0.75
        assert abs(result.cache_hit_rate - 0.75) < 1e-6
        # cost > 0
        assert result.cost_cny > 0

    async def test_run_task_handles_execute_exception(self, tmp_path: Path) -> None:
        async def explode(task, sandbox, provider, model):
            raise RuntimeError("boom")

        runner = BenchmarkRunner(
            provider=FakeProvider(),
            project_root=tmp_path,
            execute_fn=explode,
        )
        result = await runner.run_task(_make_task("T-explode"))
        assert result.success is False
        assert result.error_count == 1
        assert "RuntimeError" in (result.error_message or "")

    async def test_run_task_must_not_contain_pattern_violation(
        self, tmp_path: Path
    ) -> None:
        """if sandbox 中残留违禁 pattern (如 print()), quality_score 会扣分."""
        runner = BenchmarkRunner(
            provider=FakeProvider(),
            project_root=tmp_path,
            execute_fn=_mock_execute(success=True),
        )
        task = _make_task(
            files=[
                {"path": "bad.py", "content": "print('hi')\n"},
                {"path": "good.py", "content": "x = 1\n"},
            ],
            must_contain=["bad.py", "good.py"],
            must_not_contain=[r"print\("],
        )
        result = await runner.run_task(task)
        # 命中违禁模式 → -10；命中两个 must_contain → +20；基础 50 = 60
        assert result.quality_score == 60.0

    async def test_run_suite_collects_all_results(self, tmp_path: Path) -> None:
        runner = BenchmarkRunner(
            provider=FakeProvider(),
            project_root=tmp_path,
            execute_fn=_mock_execute(success=True),
        )
        tasks = [
            _make_task("T1", category="project_ops"),
            _make_task("T2", category="bugfix_refactor"),
            _make_task("T3", category="isomorphic", iso_group="C1"),
        ]
        results = await runner.run_suite(tasks)
        assert len(results) == 3
        assert {r.task_id for r in results} == {"T1", "T2", "T3"}
        # iso_group 透传
        iso = next(r for r in results if r.task_id == "T3")
        assert iso.isomorphic_group == "C1"

    async def test_default_execute_returns_placeholder(self, tmp_path: Path) -> None:
        """默认 execute_fn 必须是占位（避免误跑真实 MAGI）。"""
        runner = BenchmarkRunner(provider=FakeProvider(), project_root=tmp_path)
        result = await runner.run_task(_make_task("T-default"))
        # 占位是 success=False / error_count=1
        assert result.success is False
        assert result.error_count >= 1


# ============================================================
# BenchmarkComparator
# ============================================================

def _make_result(
    task_id: str,
    model: str,
    category: str = "project_ops",
    quality: float = 80.0,
    tokens: int = 1000,
    cost: float = 0.01,
    cache_hit: int = 700,
    cache_miss: int = 200,
    wall: int = 50,
    success: bool = True,
) -> BenchmarkResult:
    return BenchmarkResult(
        task_id=task_id,
        category=category,
        model=model,
        success=success,
        tokens_used=tokens,
        cache_hit_tokens=cache_hit,
        cache_miss_tokens=cache_miss,
        completion_tokens=tokens - cache_hit - cache_miss,
        cost_cny=cost,
        wall_time_ms=wall,
        steps_taken=3,
        quality_score=quality,
        summary="ok",
    )


class TestBenchmarkComparator:
    def test_compare_aggregates_per_model_per_category(self) -> None:
        bundle = {
            "DS-Optimized": [
                _make_result("A1", "DS-Optimized", "project_ops", quality=90, cost=0.005),
                _make_result(
                    "B1", "DS-Optimized", "bugfix_refactor", quality=70, cost=0.01
                ),
            ],
            "Claude": [
                _make_result("A1", "Claude", "project_ops", quality=95, cost=0.5),
                _make_result(
                    "B1", "Claude", "bugfix_refactor", quality=80, cost=0.6
                ),
            ],
        }
        report = BenchmarkComparator().compare(bundle)
        assert isinstance(report, ComparisonReport)
        assert set(report.models) == {"DS-Optimized", "Claude"}

        ds = report.per_model["DS-Optimized"]
        assert ds.overall.task_count == 2
        assert pytest.approx(ds.overall.avg_quality, abs=0.01) == 80.0
        assert pytest.approx(ds.overall.avg_cost_cny, abs=1e-6) == 0.0075
        assert "project_ops" in ds.by_category
        assert "bugfix_refactor" in ds.by_category

        # 排行：DS 比 Claude 便宜 → DS 排第一
        assert report.ranking_by_cost[0] == "DS-Optimized"
        # 质量：Claude 更高 → 第一
        assert report.ranking_by_quality[0] == "Claude"

    def test_to_markdown_contains_models_and_tables(self) -> None:
        bundle = {
            "M1": [_make_result("A1", "M1")],
            "M2": [_make_result("A1", "M2", cost=0.5)],
        }
        report = BenchmarkComparator().compare(bundle)
        md = BenchmarkComparator().to_markdown(report)
        assert "# DS Code Benchmark — Comparison Report" in md
        assert "M1" in md and "M2" in md
        assert "## Overall" in md
        assert "## Ranking" in md
        # markdown 表格头
        assert "| Model | Tasks |" in md

    def test_to_html_writes_self_contained_file(self, tmp_path: Path) -> None:
        bundle = {
            "DS-Optimized": [_make_result("A1", "DS-Optimized", quality=85)],
            "Claude": [_make_result("A1", "Claude", quality=90, cost=0.5)],
        }
        report = BenchmarkComparator().compare(bundle)
        out = tmp_path / "report.html"
        result_path = BenchmarkComparator().to_html(report, out)
        assert result_path.exists()
        content = result_path.read_text(encoding="utf-8")
        # 自包含：含 <html> / <svg> / 模型名 / 排行
        assert "<html" in content
        assert "<svg" in content
        assert "DS-Optimized" in content
        assert "Claude" in content
        assert "Cheapest first" in content

    def test_compare_handles_empty_model(self) -> None:
        bundle = {"Empty": [], "M1": [_make_result("A1", "M1")]}
        report = BenchmarkComparator().compare(bundle)
        assert report.per_model["Empty"].overall.task_count == 0
        assert report.per_model["Empty"].overall.avg_quality == 0.0
        assert report.per_model["M1"].overall.task_count == 1


# ============================================================
# 成本估算
# ============================================================

class TestCostEstimation:
    def test_estimate_cost_known_model(self) -> None:
        cost = estimate_cost_cny(
            model="deepseek-v4-flash",
            cache_hit_tokens=1_000_000,
            cache_miss_tokens=0,
            completion_tokens=0,
        )
        # flash hit 0.5 CNY/M → 1M 命中 = 0.5 CNY
        assert pytest.approx(cost, abs=1e-6) == 0.5

    def test_estimate_cost_unknown_model_falls_back(self) -> None:
        cost = estimate_cost_cny(
            model="never-heard-of",
            cache_hit_tokens=0,
            cache_miss_tokens=1_000_000,
            completion_tokens=0,
        )
        # 兜底用 flash miss 价 2.0 → 1M miss = 2.0
        assert pytest.approx(cost, abs=1e-6) == 2.0


# ============================================================
# 基准 JSON 文件验收
# ============================================================

class TestBenchmarkJSON:
    def test_coding_tasks_json_well_formed(self) -> None:
        path = Path(__file__).resolve().parent.parent.parent / "benchmarks" / "coding_tasks.json"
        assert path.exists(), f"benchmarks file missing: {path}"
        tasks = json.loads(path.read_text(encoding="utf-8"))
        assert isinstance(tasks, list)
        # 5 + 5 + 10 = 20
        assert len(tasks) == 20
        ids = [t["id"] for t in tasks]
        # A1..A5
        for i in range(1, 6):
            assert f"A{i}" in ids
        # B1..B5
        for i in range(1, 6):
            assert f"B{i}" in ids
        # C1a/b ... C5a/b
        for i in range(1, 6):
            assert f"C{i}a" in ids
            assert f"C{i}b" in ids
        # 每个任务字段齐全
        for t in tasks:
            assert "id" in t
            assert "category" in t
            assert "description" in t
            assert "setup" in t and "files" in t["setup"]
            assert "acceptance" in t
            assert "token_budget" in t

    def test_isomorphic_pairs_share_group(self) -> None:
        path = Path(__file__).resolve().parent.parent.parent / "benchmarks" / "coding_tasks.json"
        tasks = json.loads(path.read_text(encoding="utf-8"))
        c_tasks = [t for t in tasks if t["category"] == "isomorphic"]
        for t in c_tasks:
            assert "isomorphic_group" in t, f"{t['id']} missing group"
        # 每组 2 个变体
        from collections import Counter

        counts = Counter(t["isomorphic_group"] for t in c_tasks)
        assert all(c == 2 for c in counts.values()), counts
