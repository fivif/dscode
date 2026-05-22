"""BenchmarkComparator —— 跨模型对比报告。

输入：``dict[model_name, list[BenchmarkResult]]``——通常来自多次 ``run_suite``
（DS-Optimized / DS-Naive / Claude / GPT / Local）。

输出：
- ``ComparisonReport`` 数据类：按模型 × 类目聚合的平均指标。
- ``to_markdown()`` Markdown 表格（终端友好 / git 友好）。
- ``to_html()`` 自包含 HTML（含柱状图，浏览器直接看）。

无第三方依赖：HTML 用纯 inline SVG + CSS 渲染柱状图，避免引入 matplotlib。
"""
from __future__ import annotations

import html
import statistics
from collections.abc import Iterable
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field

from dscode.bench.runner import BenchmarkResult

# ============================================================
# 聚合数据类
# ============================================================

class CategoryStats(BaseModel):
    """某模型在某类任务上的聚合指标。"""

    model_config = ConfigDict(extra="allow")

    category: str
    task_count: int = 0
    success_count: int = 0
    success_rate: float = 0.0
    avg_quality: float = 0.0
    avg_tokens: float = 0.0
    avg_cost_cny: float = 0.0
    avg_cache_hit_rate: float = 0.0
    avg_wall_time_ms: float = 0.0


class ModelStats(BaseModel):
    """某模型在所有类目上的聚合 + 总分。"""

    model_config = ConfigDict(extra="allow")

    model: str
    overall: CategoryStats
    by_category: dict[str, CategoryStats] = Field(default_factory=dict)


class ComparisonReport(BaseModel):
    """完整对比报告。"""

    model_config = ConfigDict(extra="allow")

    models: list[str]
    per_model: dict[str, ModelStats]
    # 排行（按 cost / quality / cache_hit_rate 排序的模型名列表）
    ranking_by_cost: list[str] = Field(default_factory=list)
    ranking_by_quality: list[str] = Field(default_factory=list)
    ranking_by_cache_hit_rate: list[str] = Field(default_factory=list)


# ============================================================
# Comparator
# ============================================================

class BenchmarkComparator:
    """聚合 + 渲染。"""

    def compare(
        self, results: dict[str, list[BenchmarkResult]]
    ) -> ComparisonReport:
        """把多模型结果聚合成 ``ComparisonReport``。"""
        per_model: dict[str, ModelStats] = {}
        for model, items in results.items():
            per_model[model] = self._aggregate_one(model, items)

        models = list(results.keys())
        return ComparisonReport(
            models=models,
            per_model=per_model,
            ranking_by_cost=sorted(
                models, key=lambda m: per_model[m].overall.avg_cost_cny
            ),
            ranking_by_quality=sorted(
                models,
                key=lambda m: per_model[m].overall.avg_quality,
                reverse=True,
            ),
            ranking_by_cache_hit_rate=sorted(
                models,
                key=lambda m: per_model[m].overall.avg_cache_hit_rate,
                reverse=True,
            ),
        )

    # ------------------------------------------------------------
    # Markdown
    # ------------------------------------------------------------

    def to_markdown(self, report: ComparisonReport) -> str:
        """渲染 Markdown 报告。"""
        lines: list[str] = []
        lines.append("# DS Code Benchmark — Comparison Report")
        lines.append("")
        lines.append(f"Models: {', '.join(report.models)}")
        lines.append("")

        # 总体表
        lines.append("## Overall")
        lines.append("")
        lines.append(
            "| Model | Tasks | Success | Quality | Tokens | Cost (CNY) | Cache Hit | Wall (ms) |"
        )
        lines.append(
            "|---|---:|---:|---:|---:|---:|---:|---:|"
        )
        for model in report.models:
            s = report.per_model[model].overall
            lines.append(
                f"| {model} | {s.task_count} "
                f"| {s.success_rate * 100:.1f}% "
                f"| {s.avg_quality:.1f} "
                f"| {s.avg_tokens:.0f} "
                f"| {s.avg_cost_cny:.4f} "
                f"| {s.avg_cache_hit_rate * 100:.1f}% "
                f"| {s.avg_wall_time_ms:.0f} |"
            )
        lines.append("")

        # 类目分表
        all_categories: set[str] = set()
        for ms in report.per_model.values():
            all_categories.update(ms.by_category.keys())
        for cat in sorted(all_categories):
            lines.append(f"## Category — {cat}")
            lines.append("")
            lines.append(
                "| Model | Tasks | Success | Quality | Tokens | Cost (CNY) | Cache Hit |"
            )
            lines.append("|---|---:|---:|---:|---:|---:|---:|")
            for model in report.models:
                s = report.per_model[model].by_category.get(cat)
                if s is None:
                    lines.append(f"| {model} | — | — | — | — | — | — |")
                    continue
                lines.append(
                    f"| {model} | {s.task_count} "
                    f"| {s.success_rate * 100:.1f}% "
                    f"| {s.avg_quality:.1f} "
                    f"| {s.avg_tokens:.0f} "
                    f"| {s.avg_cost_cny:.4f} "
                    f"| {s.avg_cache_hit_rate * 100:.1f}% |"
                )
            lines.append("")

        # 排行
        lines.append("## Ranking")
        lines.append("")
        lines.append(f"- **Cheapest first**: {' → '.join(report.ranking_by_cost)}")
        lines.append(f"- **Highest quality first**: {' → '.join(report.ranking_by_quality)}")
        lines.append(
            f"- **Best cache hit first**: {' → '.join(report.ranking_by_cache_hit_rate)}"
        )
        lines.append("")

        return "\n".join(lines)

    # ------------------------------------------------------------
    # HTML
    # ------------------------------------------------------------

    def to_html(self, report: ComparisonReport, output_path: Path) -> Path:
        """渲染 HTML 报告（自包含、可在浏览器查看，带柱状图）。"""
        output_path = Path(output_path).resolve()
        output_path.parent.mkdir(parents=True, exist_ok=True)

        models = report.models
        # 柱状图数据
        costs = [report.per_model[m].overall.avg_cost_cny for m in models]
        qualities = [report.per_model[m].overall.avg_quality for m in models]
        caches = [report.per_model[m].overall.avg_cache_hit_rate * 100 for m in models]

        bars_cost = self._render_svg_bars(models, costs, label="CNY", color="#3b82f6")
        bars_quality = self._render_svg_bars(
            models, qualities, label="score", color="#10b981"
        )
        bars_cache = self._render_svg_bars(
            models, caches, label="%", color="#f59e0b"
        )

        # 总体表
        overall_rows = []
        for m in models:
            s = report.per_model[m].overall
            overall_rows.append(
                f"<tr><td>{html.escape(m)}</td>"
                f"<td>{s.task_count}</td>"
                f"<td>{s.success_rate * 100:.1f}%</td>"
                f"<td>{s.avg_quality:.1f}</td>"
                f"<td>{s.avg_tokens:.0f}</td>"
                f"<td>{s.avg_cost_cny:.4f}</td>"
                f"<td>{s.avg_cache_hit_rate * 100:.1f}%</td>"
                f"<td>{s.avg_wall_time_ms:.0f}</td></tr>"
            )

        # 类目表
        all_categories: set[str] = set()
        for ms in report.per_model.values():
            all_categories.update(ms.by_category.keys())
        category_sections: list[str] = []
        for cat in sorted(all_categories):
            rows = []
            for m in models:
                s = report.per_model[m].by_category.get(cat)
                if s is None:
                    rows.append(
                        f"<tr><td>{html.escape(m)}</td>"
                        f"<td colspan='6'>—</td></tr>"
                    )
                    continue
                rows.append(
                    f"<tr><td>{html.escape(m)}</td>"
                    f"<td>{s.task_count}</td>"
                    f"<td>{s.success_rate * 100:.1f}%</td>"
                    f"<td>{s.avg_quality:.1f}</td>"
                    f"<td>{s.avg_tokens:.0f}</td>"
                    f"<td>{s.avg_cost_cny:.4f}</td>"
                    f"<td>{s.avg_cache_hit_rate * 100:.1f}%</td></tr>"
                )
            category_sections.append(
                f"<h3>Category — {html.escape(cat)}</h3>"
                f"<table><thead><tr>"
                f"<th>Model</th><th>Tasks</th><th>Success</th><th>Quality</th>"
                f"<th>Tokens</th><th>Cost (CNY)</th><th>Cache Hit</th>"
                f"</tr></thead><tbody>"
                + "".join(rows)
                + "</tbody></table>"
            )

        ranking_html = (
            f"<ul>"
            f"<li><strong>Cheapest first:</strong> "
            f"{' → '.join(html.escape(m) for m in report.ranking_by_cost)}</li>"
            f"<li><strong>Highest quality first:</strong> "
            f"{' → '.join(html.escape(m) for m in report.ranking_by_quality)}</li>"
            f"<li><strong>Best cache hit first:</strong> "
            f"{' → '.join(html.escape(m) for m in report.ranking_by_cache_hit_rate)}</li>"
            f"</ul>"
        )

        doc = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>DS Code Benchmark Comparison</title>
<style>
  body {{ font-family: -apple-system, "Helvetica Neue", sans-serif; max-width: 980px;
         margin: 2em auto; color: #1f2937; padding: 0 1em; }}
  h1 {{ color: #0f172a; }}
  h2 {{ margin-top: 2em; border-bottom: 1px solid #e5e7eb; padding-bottom: 4px; }}
  h3 {{ margin-top: 1.5em; color: #334155; }}
  table {{ border-collapse: collapse; width: 100%; margin: 1em 0; font-size: 14px; }}
  th, td {{ border: 1px solid #e5e7eb; padding: 6px 10px; text-align: right; }}
  th:first-child, td:first-child {{ text-align: left; }}
  th {{ background: #f3f4f6; }}
  tr:nth-child(even) td {{ background: #fafafa; }}
  .chart {{ display: flex; gap: 24px; flex-wrap: wrap; margin: 1em 0; }}
  .chart > div {{ flex: 1; min-width: 280px; }}
  .chart svg {{ width: 100%; height: 180px; }}
  .meta {{ color: #6b7280; font-size: 13px; }}
  ul {{ line-height: 1.7; }}
</style>
</head>
<body>
<h1>DS Code Benchmark — Comparison Report</h1>
<p class="meta">Models: {html.escape(", ".join(models))}</p>

<h2>Charts</h2>
<div class="chart">
  <div><h3>Avg Cost (CNY)</h3>{bars_cost}</div>
  <div><h3>Avg Quality (0-100)</h3>{bars_quality}</div>
  <div><h3>Avg Cache Hit Rate (%)</h3>{bars_cache}</div>
</div>

<h2>Overall</h2>
<table>
  <thead><tr>
    <th>Model</th><th>Tasks</th><th>Success</th><th>Quality</th>
    <th>Tokens</th><th>Cost (CNY)</th><th>Cache Hit</th><th>Wall (ms)</th>
  </tr></thead>
  <tbody>{''.join(overall_rows)}</tbody>
</table>

<h2>By Category</h2>
{''.join(category_sections) or '<p class="meta">No category data.</p>'}

<h2>Ranking</h2>
{ranking_html}
</body>
</html>
"""
        output_path.write_text(doc, encoding="utf-8")
        return output_path

    # ------------------------------------------------------------
    # 私有
    # ------------------------------------------------------------

    @staticmethod
    def _aggregate_one(model: str, items: list[BenchmarkResult]) -> ModelStats:
        overall = _aggregate_results(items, category="overall")
        by_cat: dict[str, CategoryStats] = {}
        cats: set[str] = {r.category for r in items}
        for c in cats:
            bucket = [r for r in items if r.category == c]
            by_cat[c] = _aggregate_results(bucket, category=c)
        return ModelStats(model=model, overall=overall, by_category=by_cat)

    @staticmethod
    def _render_svg_bars(
        labels: list[str], values: list[float], label: str, color: str
    ) -> str:
        if not values:
            return "<p class='meta'>No data.</p>"

        width = 380
        height = 160
        padding_left = 100
        padding_right = 16
        padding_top = 12
        padding_bottom = 24

        max_val = max(values) if max(values) > 0 else 1.0
        chart_w = width - padding_left - padding_right
        chart_h = height - padding_top - padding_bottom
        bar_h = max(8, min(28, chart_h / max(1, len(values)) - 6))
        row_gap = (chart_h - bar_h * len(values)) / max(1, len(values))

        bars: list[str] = []
        for i, (lbl, v) in enumerate(zip(labels, values, strict=False)):
            y = padding_top + i * (bar_h + row_gap)
            w = (v / max_val) * chart_w if max_val > 0 else 0
            bars.append(
                f'<rect x="{padding_left}" y="{y:.1f}" width="{w:.1f}" '
                f'height="{bar_h:.1f}" fill="{color}" rx="3"/>'
            )
            bars.append(
                f'<text x="{padding_left - 6}" y="{y + bar_h / 2 + 4:.1f}" '
                f'text-anchor="end" font-size="11" fill="#1f2937">'
                f"{html.escape(lbl)}</text>"
            )
            bars.append(
                f'<text x="{padding_left + w + 4:.1f}" '
                f'y="{y + bar_h / 2 + 4:.1f}" font-size="11" fill="#374151">'
                f"{v:.2f} {html.escape(label)}</text>"
            )

        return (
            f'<svg viewBox="0 0 {width} {height}" xmlns="http://www.w3.org/2000/svg">'
            f'<rect x="0" y="0" width="{width}" height="{height}" fill="#ffffff"/>'
            + "".join(bars)
            + "</svg>"
        )


# ============================================================
# 工具函数
# ============================================================

def _safe_mean(values: Iterable[float]) -> float:
    vs = list(values)
    return statistics.fmean(vs) if vs else 0.0


def _aggregate_results(items: list[BenchmarkResult], category: str) -> CategoryStats:
    if not items:
        return CategoryStats(category=category)
    success_count = sum(1 for r in items if r.success)
    return CategoryStats(
        category=category,
        task_count=len(items),
        success_count=success_count,
        success_rate=success_count / len(items),
        avg_quality=_safe_mean(r.quality_score for r in items),
        avg_tokens=_safe_mean(r.tokens_used for r in items),
        avg_cost_cny=_safe_mean(r.cost_cny for r in items),
        avg_cache_hit_rate=_safe_mean(r.cache_hit_rate for r in items),
        avg_wall_time_ms=_safe_mean(r.wall_time_ms for r in items),
    )


__all__ = [
    "BenchmarkComparator",
    "CategoryStats",
    "ComparisonReport",
    "ModelStats",
]
