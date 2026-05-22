"""记忆图谱模块：从 Scribe 构图 + sigma.js HTML 导出。

Public API：
- ``GraphBuilder`` — 从 Scribe 加载 patterns + facts，构建加权无向图
- ``GraphSnapshot`` / ``GraphNode`` / ``GraphEdge`` — 不可变快照数据类
- ``HTMLExporter`` — 渲染 snapshot 为单文件 HTML（sigma.js + ForceAtlas2）
"""

from dscode.graph.builder import GraphBuilder, GraphEdge, GraphNode, GraphSnapshot
from dscode.graph.exporter import HTMLExporter

__all__ = [
    "GraphBuilder",
    "GraphEdge",
    "GraphNode",
    "GraphSnapshot",
    "HTMLExporter",
]
