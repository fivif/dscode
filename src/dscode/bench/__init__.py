"""DS Code 编码基准集 + 跨模型对比。

模块结构
========
- ``BenchmarkRunner``     在临时 sandbox 中跑单个任务并收集 metrics
- ``BenchmarkResult``     单任务执行结果（success / tokens / cost / cache / wall_time）
- ``BenchmarkComparator`` 跨模型 / 跨变体的报告聚合 + Markdown/HTML 渲染
- ``ComparisonReport``    聚合后的报告对象

CLI 入口见 ``dscode.cli.bench_run`` / ``dscode.cli.bench_compare``。
"""
from __future__ import annotations

from dscode.bench.comparator import (
    BenchmarkComparator,
    CategoryStats,
    ComparisonReport,
    ModelStats,
)
from dscode.bench.runner import BenchmarkResult, BenchmarkRunner

__all__ = [
    "BenchmarkComparator",
    "BenchmarkResult",
    "BenchmarkRunner",
    "CategoryStats",
    "ComparisonReport",
    "ModelStats",
]
