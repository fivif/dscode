"""BenchmarkRunner —— 在隔离 sandbox 中跑单个编码任务并采集指标。

设计要点
========
1. 每个任务在 ``tempfile.TemporaryDirectory()`` 中执行，结束后整体清理，
   不会污染 ``benchmarks/`` 或工作仓库。
2. 真实的 MAGI 调度 **不会** 在 ``run_task`` 中触发——直接调 ``MAGIScheduler.run``
   会消耗大量 token 并阻塞测试。我们把执行包装成 ``_execute_task``，
   测试默认 monkeypatch 它；只有用户显式提供 ``execute_fn`` 时才走真实路径。
3. ``_execute_task`` 的默认实现可以接入真实 MAGI（``max_rounds=3`` 防炸内存），
   但需要 caller 提供 forge / scribe / spec_loader 等重量级依赖。
4. 结果以 ``BenchmarkResult`` Pydantic 模型返回，便于 JSON 持久化。

公共字段
========
``BenchmarkResult`` 暴露 ``success / tokens / cost_cny / cache_hit_rate /
wall_time_ms / steps / errors``，与 ``comparator.py`` 的聚合输入对齐。
"""
from __future__ import annotations

import asyncio
import tempfile
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from pydantic import BaseModel, ConfigDict

from dscode.core.types import (
    ExecutionResult,
    LLMProviderProtocol,
)

# ============================================================
# 成本表（CNY/百万 token，2026-05 价目）
# ============================================================

# 用粗略均价兜底；真实定价应由 Config 注入，这里仅供 bench 自洽估算。
_DEFAULT_COST_TABLE_CNY_PER_M = {
    "deepseek-v4-flash": {"prompt_hit": 0.5, "prompt_miss": 2.0, "completion": 8.0},
    "deepseek-v4-pro": {"prompt_hit": 1.0, "prompt_miss": 4.0, "completion": 16.0},
    # 跨模型对比时由 CLI 提供 override
    "claude-sonnet-4.6": {"prompt_hit": 22.0, "prompt_miss": 22.0, "completion": 110.0},
    "gpt-5": {"prompt_hit": 36.0, "prompt_miss": 36.0, "completion": 144.0},
    "qwen3-32b": {"prompt_hit": 0.0, "prompt_miss": 0.0, "completion": 0.0},
}


def estimate_cost_cny(
    model: str,
    cache_hit_tokens: int,
    cache_miss_tokens: int,
    completion_tokens: int,
    table: dict[str, dict[str, float]] | None = None,
) -> float:
    """根据 token 使用粗估 CNY 成本（百万 token 单价）。"""
    pricing = (table or _DEFAULT_COST_TABLE_CNY_PER_M).get(model)
    if pricing is None:
        # 未知模型按 flash 估，避免炸；统计页会提示。
        pricing = _DEFAULT_COST_TABLE_CNY_PER_M["deepseek-v4-flash"]
    cost = (
        cache_hit_tokens * pricing["prompt_hit"]
        + cache_miss_tokens * pricing["prompt_miss"]
        + completion_tokens * pricing["completion"]
    ) / 1_000_000
    return round(cost, 6)


# ============================================================
# 数据类
# ============================================================

class BenchmarkResult(BaseModel):
    """单个任务的执行结果，可序列化为 JSON。"""

    model_config = ConfigDict(extra="allow")

    task_id: str
    category: str
    model: str
    success: bool
    tokens_used: int = 0
    completion_tokens: int = 0
    cache_hit_tokens: int = 0
    cache_miss_tokens: int = 0
    cost_cny: float = 0.0
    wall_time_ms: int = 0
    steps_taken: int = 0
    tool_call_count: int = 0
    error_count: int = 0
    quality_score: float = 0.0  # 0..100，由 acceptance 命中率决定
    summary: str = ""
    isomorphic_group: str | None = None
    error_message: str | None = None

    @property
    def cache_hit_rate(self) -> float:
        total = self.cache_hit_tokens + self.cache_miss_tokens
        return self.cache_hit_tokens / total if total > 0 else 0.0


# ============================================================
# 执行回调签名
# ============================================================

# 真正的"跑一个任务"逻辑被抽成一个可注入的函数，方便测试 mock。
# 签名：(task_dict, sandbox_path, provider, model) -> ExecutionResult
ExecuteFn = Callable[[dict, Path, LLMProviderProtocol, str], Awaitable[ExecutionResult]]


async def _default_execute_task(
    task: dict[str, Any],
    sandbox: Path,
    provider: LLMProviderProtocol,
    model: str,
) -> ExecutionResult:
    """默认实现 —— 占位的"空跑"。

    不调用真实 MAGI（会炸 token / 卡循环）。仅返回一个 success=False 的占位
    ExecutionResult，提示调用方注入真实 ``execute_fn``。
    生产环境应替换为：
        runner = BenchmarkRunner(provider, root, execute_fn=my_real_executor)
    """
    return ExecutionResult(
        success=False,
        summary=(
            "[BenchmarkRunner] default _execute_task is a placeholder; "
            "inject a real execute_fn for production runs."
        ),
        steps_taken=0,
        tokens_used=0,
        wall_time_ms=0,
        tool_call_count=0,
        error_count=1,
    )


# ============================================================
# Runner
# ============================================================

class BenchmarkRunner:
    """跑 benchmark 任务并采集指标。

    Args:
        provider: LLM Provider（满足 LLMProviderProtocol）。
        project_root: 项目根目录（dscode init 过的目录）。
        model: 默认执行模型名。
        execute_fn: 任务执行函数，默认是占位（测试用 mock 替换）。
        cost_table: 成本表 override（CNY / 百万 token）。
    """

    def __init__(
        self,
        provider: LLMProviderProtocol,
        project_root: Path,
        model: str = "deepseek-v4-flash",
        execute_fn: ExecuteFn | None = None,
        cost_table: dict[str, dict[str, float]] | None = None,
    ) -> None:
        self.provider = provider
        self.project_root = Path(project_root).resolve()
        self.model = model
        # 关键扩展点：测试 / production 都通过这个 hook 注入真实执行
        self._execute_task: ExecuteFn = execute_fn or _default_execute_task
        self.cost_table = cost_table

    # ------------------------------------------------------------
    # 单任务
    # ------------------------------------------------------------

    async def run_task(
        self,
        task: dict[str, Any],
        model: str | None = None,
    ) -> BenchmarkResult:
        """在临时 sandbox 中跑单个任务。"""
        effective_model = model or self.model
        task_id = str(task.get("id", "?"))
        category = str(task.get("category", "unknown"))
        iso_group = task.get("isomorphic_group")

        started = time.time()
        try:
            with tempfile.TemporaryDirectory(prefix=f"dscode-bench-{task_id}-") as tmp:
                sandbox = Path(tmp)
                self._materialize_setup(task, sandbox)
                exec_result = await self._execute_task(
                    task, sandbox, self.provider, effective_model
                )
                quality = self._score_acceptance(task, sandbox, exec_result)
        except Exception as exc:  # 任何异常都包装成失败结果
            wall_ms = int((time.time() - started) * 1000)
            return BenchmarkResult(
                task_id=task_id,
                category=category,
                model=effective_model,
                success=False,
                wall_time_ms=wall_ms,
                isomorphic_group=iso_group,
                error_count=1,
                error_message=f"{type(exc).__name__}: {exc}",
            )

        cost = estimate_cost_cny(
            model=effective_model,
            cache_hit_tokens=exec_result.cache_hit_tokens,
            cache_miss_tokens=exec_result.cache_miss_tokens,
            completion_tokens=max(
                0,
                exec_result.tokens_used
                - exec_result.cache_hit_tokens
                - exec_result.cache_miss_tokens,
            ),
            table=self.cost_table,
        )

        return BenchmarkResult(
            task_id=task_id,
            category=category,
            model=effective_model,
            success=exec_result.success and quality >= 60.0,
            tokens_used=exec_result.tokens_used,
            cache_hit_tokens=exec_result.cache_hit_tokens,
            cache_miss_tokens=exec_result.cache_miss_tokens,
            completion_tokens=max(
                0,
                exec_result.tokens_used
                - exec_result.cache_hit_tokens
                - exec_result.cache_miss_tokens,
            ),
            cost_cny=cost,
            wall_time_ms=exec_result.wall_time_ms,
            steps_taken=exec_result.steps_taken,
            tool_call_count=exec_result.tool_call_count,
            error_count=exec_result.error_count,
            quality_score=quality,
            summary=exec_result.summary,
            isomorphic_group=iso_group,
        )

    # ------------------------------------------------------------
    # 套件
    # ------------------------------------------------------------

    async def run_suite(
        self,
        tasks: list[dict[str, Any]],
        model: str | None = None,
        concurrency: int = 1,
    ) -> list[BenchmarkResult]:
        """跑整个套件。

        ``concurrency=1`` 时严格串行（默认，避免 sandbox 冲突）。
        ``concurrency>1`` 时用 ``asyncio.Semaphore`` 限流并发。
        """
        if concurrency <= 1:
            results: list[BenchmarkResult] = []
            for t in tasks:
                results.append(await self.run_task(t, model=model))
            return results

        sem = asyncio.Semaphore(concurrency)

        async def _bounded(t: dict[str, Any]) -> BenchmarkResult:
            async with sem:
                return await self.run_task(t, model=model)

        return await asyncio.gather(*(_bounded(t) for t in tasks))

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    @staticmethod
    def _materialize_setup(task: dict[str, Any], sandbox: Path) -> None:
        """把 task.setup.files 写到 sandbox 下。"""
        setup = task.get("setup") or {}
        for f in setup.get("files", []):
            rel = f.get("path")
            content = f.get("content", "")
            if not rel:
                continue
            dest = sandbox / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(content, encoding="utf-8")

    @staticmethod
    def _score_acceptance(
        task: dict[str, Any], sandbox: Path, exec_result: ExecutionResult
    ) -> float:
        """根据 acceptance.must_contain_files / must_not_contain_patterns 打分。

        - 不会真正去跑 test_command（mock 时跑不到，真实模式也由 execute_fn 决定）。
        - 这里只做 cheap static check：文件存在 + 反模式不命中。
        - quality_score 满分 100：基础分 50 + 每条断言通过 +10，封顶 100。
        """
        if not exec_result.success:
            return 0.0

        acceptance = task.get("acceptance") or {}
        score = 50.0

        for relpath in acceptance.get("must_contain_files", []) or []:
            if (sandbox / relpath).exists():
                score += 10.0
            else:
                score -= 10.0

        import re

        for pat in acceptance.get("must_not_contain_patterns", []) or []:
            try:
                rx = re.compile(pat)
            except re.error:
                continue
            hit = False
            for p in sandbox.rglob("*.py"):
                try:
                    if rx.search(p.read_text(encoding="utf-8", errors="ignore")):
                        hit = True
                        break
                except OSError:
                    continue
            if not hit:
                score += 10.0
            else:
                score -= 10.0

        return max(0.0, min(100.0, score))


__all__ = [
    "BenchmarkResult",
    "BenchmarkRunner",
    "ExecuteFn",
    "estimate_cost_cny",
]
