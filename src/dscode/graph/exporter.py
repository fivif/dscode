"""把 ``GraphSnapshot`` 渲染为单个静态 HTML（sigma.js + graphology + ForceAtlas2）。

HTML 自包含：
- 通过 CDN 加载 sigma.js / graphology / forceatlas2 layout
- 图数据嵌入为 JSON
- 节点颜色按 community 自动染色
- 鼠标悬停显示 label / type / pattern_type 等元数据
- 支持平移 / 缩放 / 拖拽

不引入新依赖（无 jinja2），用纯字符串拼接 + ``json.dumps`` 安全序列化。
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from dscode.graph.builder import GraphSnapshot


# ============================================================
# Exporter
# ============================================================


class HTMLExporter:
    """渲染 GraphSnapshot 为可在浏览器直接打开的 HTML 文件。"""

    # CDN —— 用版本锁，便于离线复现；不依赖私有 mirror
    SIGMA_CDN = "https://cdn.jsdelivr.net/npm/sigma@3.0.0/build/sigma.min.js"
    GRAPHOLOGY_CDN = "https://cdn.jsdelivr.net/npm/graphology@0.25.4/dist/graphology.umd.min.js"
    FA2_CDN = (
        "https://cdn.jsdelivr.net/npm/graphology-layout-forceatlas2@0.10.1/"
        "build/graphology-layout-forceatlas2.min.js"
    )

    # 调色板：HSL 黄金角，避免相邻 community 颜色相近
    GOLDEN_ANGLE = 137.508

    def __init__(self, title: str = "DS Code Memory Graph") -> None:
        self.title = title

    # -------- 主方法 --------

    def render(self, snapshot: GraphSnapshot) -> str:
        """返回完整 HTML 字符串。"""
        payload = self._to_payload(snapshot)
        data_json = json.dumps(payload, ensure_ascii=False, indent=None)
        title_safe = _html_escape(self.title)

        return _HTML_TEMPLATE.format(
            title=title_safe,
            sigma_cdn=self.SIGMA_CDN,
            graphology_cdn=self.GRAPHOLOGY_CDN,
            fa2_cdn=self.FA2_CDN,
            data_json=data_json,
            node_count=snapshot.node_count,
            edge_count=snapshot.edge_count,
            community_count=len(snapshot.communities),
            insights_html=self._render_insights(snapshot.insights),
        )

    def write(self, snapshot: GraphSnapshot, path: Path) -> Path:
        """渲染并写入文件。返回最终路径。"""
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        html = self.render(snapshot)
        path.write_text(html, encoding="utf-8")
        return path

    # -------- payload --------

    def _to_payload(self, snapshot: GraphSnapshot) -> dict[str, Any]:
        # 按 community id 染色；社区 -1（未分配）用灰色
        color_for = self._community_palette(snapshot)

        nodes_payload: list[dict[str, Any]] = []
        for n in snapshot.nodes:
            size = self._node_size(n.metadata)
            nodes_payload.append(
                {
                    "key": n.id,
                    "attributes": {
                        "label": n.label,
                        "type": n.type,
                        "pattern_type": n.pattern_type,
                        "community": n.community,
                        "size": size,
                        "color": color_for(n.community),
                        "metadata": _scrub(n.metadata),
                    },
                }
            )

        edges_payload: list[dict[str, Any]] = []
        for i, e in enumerate(snapshot.edges):
            edges_payload.append(
                {
                    "key": f"e{i}",
                    "source": e.source,
                    "target": e.target,
                    "attributes": {
                        "weight": e.weight,
                        "signals": e.signals,
                        "size": min(8.0, 0.4 + e.weight * 0.2),
                    },
                }
            )

        return {
            "nodes": nodes_payload,
            "edges": edges_payload,
            "communities": {
                str(cid): members for cid, members in snapshot.communities.items()
            },
            "insights": snapshot.insights,
        }

    @staticmethod
    def _node_size(metadata: dict[str, Any]) -> float:
        # 节点大小：pattern 用 confidence × sample_count；fact 用 confidence
        if "sample_count" in metadata:
            base = float(metadata.get("confidence", 0.0)) * (
                1.0 + float(metadata.get("sample_count", 0))
            )
            return max(4.0, min(18.0, 4.0 + base * 1.2))
        conf = float(metadata.get("confidence", 0.5))
        return max(3.5, min(10.0, 3.5 + conf * 4.0))

    def _community_palette(self, snapshot: GraphSnapshot) -> Any:
        cids = sorted({n.community for n in snapshot.nodes})

        def color(cid: int) -> str:
            if cid < 0:
                return "#888888"
            idx = cids.index(cid) if cid in cids else 0
            hue = (idx * self.GOLDEN_ANGLE) % 360
            return f"hsl({hue:.1f}, 65%, 55%)"

        return color

    @staticmethod
    def _render_insights(insights: list[str]) -> str:
        if not insights:
            return "<li><em>暂无洞察</em></li>"
        return "\n".join(f"<li>{_html_escape(s)}</li>" for s in insights)


# ============================================================
# helpers
# ============================================================


def _html_escape(s: str) -> str:
    return (
        s.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def _scrub(obj: Any) -> Any:
    """让 metadata 可序列化（dict / list / 标量）。任何非 JSON 友好类型转 str。"""
    if obj is None or isinstance(obj, (bool, int, float, str)):
        return obj
    if isinstance(obj, dict):
        return {str(k): _scrub(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple, set)):
        return [_scrub(x) for x in obj]
    return str(obj)


# ============================================================
# 模板
# ============================================================

# Curly braces 在 CSS / JS 中需要 doubled (escape) 才能与 .format 配合
_HTML_TEMPLATE = """<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<title>{title}</title>
<style>
  html, body {{ margin: 0; padding: 0; height: 100%; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif; background: #0f1117; color: #e6e8ed; }}
  #app {{ display: grid; grid-template-columns: 1fr 320px; height: 100vh; }}
  #graph {{ background: #0f1117; }}
  #sidebar {{ background: #161922; padding: 16px 18px; overflow-y: auto; border-left: 1px solid #2a2f3d; box-sizing: border-box; }}
  h1 {{ font-size: 16px; margin: 0 0 8px 0; color: #d6d9e0; }}
  h2 {{ font-size: 13px; margin: 18px 0 6px 0; color: #9ba1b0; text-transform: uppercase; letter-spacing: 0.05em; }}
  ul {{ padding-left: 18px; margin: 4px 0; font-size: 12px; line-height: 1.55; }}
  .stat {{ display: flex; justify-content: space-between; padding: 4px 0; font-size: 12.5px; color: #c2c6d0; border-bottom: 1px dotted #2a2f3d; }}
  .stat strong {{ color: #fff; font-weight: 600; }}
  #info {{ font-size: 12px; line-height: 1.55; word-break: break-word; color: #c2c6d0; min-height: 60px; }}
  #info .empty {{ color: #5b6072; font-style: italic; }}
  .pill {{ display: inline-block; padding: 1px 7px; border-radius: 8px; background: #2a2f3d; color: #b6bccc; font-size: 11px; margin-right: 4px; }}
  code {{ background: #1f2330; padding: 1px 5px; border-radius: 3px; font-size: 11.5px; }}
</style>
</head>
<body>
<div id="app">
  <div id="graph"></div>
  <aside id="sidebar">
    <h1>{title}</h1>
    <div class="stat"><span>节点</span><strong>{node_count}</strong></div>
    <div class="stat"><span>边</span><strong>{edge_count}</strong></div>
    <div class="stat"><span>社区</span><strong>{community_count}</strong></div>

    <h2>洞察</h2>
    <ul id="insights">
      {insights_html}
    </ul>

    <h2>当前节点</h2>
    <div id="info"><span class="empty">点击或悬停节点查看详情</span></div>

    <h2>说明</h2>
    <ul>
      <li>滚轮缩放 / 拖拽平移</li>
      <li>颜色 = Louvain 社区</li>
      <li>节点大小 = confidence × sample_count</li>
      <li>边粗细 = 4 信号加权</li>
    </ul>
  </aside>
</div>

<script src="{graphology_cdn}"></script>
<script src="{fa2_cdn}"></script>
<script src="{sigma_cdn}"></script>
<script id="graph-data" type="application/json">{data_json}</script>
<script>
(function () {{
  var raw;
  try {{
    raw = JSON.parse(document.getElementById('graph-data').textContent);
  }} catch (e) {{
    console.error('[dscode] graph data parse failed', e);
    return;
  }}

  if (typeof graphology === 'undefined' || typeof Sigma === 'undefined') {{
    console.warn('[dscode] sigma.js / graphology CDN not loaded');
    return;
  }}

  var Graph = graphology.Graph || graphology.default || graphology;
  var graph = new Graph();

  // 初始随机布局，让 ForceAtlas2 有发力空间
  raw.nodes.forEach(function (n, i) {{
    var theta = (i / raw.nodes.length) * Math.PI * 2;
    var r = 5 + Math.sqrt(i);
    var attrs = Object.assign({{
      x: Math.cos(theta) * r,
      y: Math.sin(theta) * r,
      size: 5,
      color: '#88a',
      label: n.key
    }}, n.attributes || {{}});
    graph.addNode(n.key, attrs);
  }});
  raw.edges.forEach(function (e) {{
    if (!graph.hasNode(e.source) || !graph.hasNode(e.target)) return;
    try {{
      graph.addEdgeWithKey(e.key, e.source, e.target, e.attributes || {{}});
    }} catch (_) {{ /* ignore duplicate */ }}
  }});

  // ForceAtlas2 几百轮 —— 静态导出，运行一次后定型
  if (typeof forceAtlas2 !== 'undefined') {{
    var settings = forceAtlas2.inferSettings(graph);
    forceAtlas2.assign(graph, {{ iterations: 200, settings: settings }});
  }}

  var renderer = new Sigma(graph, document.getElementById('graph'), {{
    renderEdgeLabels: false,
    defaultEdgeColor: '#3a4054',
    labelColor: {{ color: '#d6d9e0' }},
    labelSize: 11
  }});

  var info = document.getElementById('info');

  function showNode(nodeId) {{
    if (!nodeId) {{
      info.innerHTML = '<span class="empty">点击或悬停节点查看详情</span>';
      return;
    }}
    var a = graph.getNodeAttributes(nodeId);
    var parts = [];
    parts.push('<div><span class="pill">' + (a.type || '?') + '</span>');
    if (a.pattern_type) parts.push('<span class="pill">' + a.pattern_type + '</span>');
    parts.push('<span class="pill">c#' + (a.community != null ? a.community : '?') + '</span></div>');
    parts.push('<div style="margin-top:6px;"><code>' + nodeId + '</code></div>');
    parts.push('<div style="margin-top:6px;">' + (a.label || '') + '</div>');
    if (a.metadata) {{
      parts.push('<pre style="white-space:pre-wrap;font-size:11px;color:#9ba1b0;background:#1f2330;padding:6px;border-radius:4px;margin-top:8px;">' + JSON.stringify(a.metadata, null, 2) + '</pre>');
    }}
    info.innerHTML = parts.join('');
  }}

  renderer.on('enterNode', function (e) {{ showNode(e.node); }});
  renderer.on('clickNode', function (e) {{ showNode(e.node); }});
  renderer.on('leaveNode', function () {{ /* keep last */ }});
}})();
</script>
</body>
</html>
"""


__all__ = ["HTMLExporter"]
