"""记忆图谱构建器。

从 Scribe 的 active patterns + active facts 构建一张加权无向图，节点为 Pattern / Fact，
边权由四个信号合成（思路源自 nashsu/llm_wiki）：

1. ``direct_link`` (权 3.0)：A 的 metadata 显式引用 B（pattern 的 ``action_template``
   或 ``trigger_condition`` 中直接出现对方 id / fact subject 等）。
2. ``source_overlap`` (权 4.0)：A 与 B 共享某个上游 raw_event —— 对 facts 看
   ``source_raw_event_id`` 与 ``provenance_chain``；对 patterns 看 ``session_ids``。
3. ``adamic_adar`` (权 1.5)：在前述基础图上计算共同邻居打分，仅对**已经被前两种信号连接**
   或共享类型的节点对叠加（不引入凭空的新边）。
4. ``type_affinity`` (权 1.0)：两个 pattern 同 ``pattern_type``。

在最终加权图上跑 Louvain 社区检测 (``networkx.algorithms.community.louvain_communities``)
并产出洞察列表（孤立节点、桥节点、低凝聚社区、大社区）。
"""
from __future__ import annotations

import math
from collections import defaultdict
from typing import Any

import networkx as nx
from networkx.algorithms.community import louvain_communities
from pydantic import BaseModel, Field

from dscode.core.types import Fact, Pattern, ScribeProtocol


# ============================================================
# 数据模型
# ============================================================


class GraphNode(BaseModel):
    """图节点。id 加前缀 ``p:`` / ``f:`` 区分 pattern / fact。"""

    id: str
    label: str
    type: str  # 'pattern' / 'fact'
    pattern_type: str | None = None
    community: int = -1
    metadata: dict[str, Any] = Field(default_factory=dict)


class GraphEdge(BaseModel):
    """图边。weight 是 4 信号加权和，signals 保留细节供调试。"""

    source: str
    target: str
    weight: float
    signals: dict[str, float] = Field(default_factory=dict)


class GraphSnapshot(BaseModel):
    """图谱快照。"""

    nodes: list[GraphNode]
    edges: list[GraphEdge]
    communities: dict[int, list[str]] = Field(default_factory=dict)
    insights: list[str] = Field(default_factory=list)

    @property
    def node_count(self) -> int:
        return len(self.nodes)

    @property
    def edge_count(self) -> int:
        return len(self.edges)


# ============================================================
# Builder
# ============================================================


class GraphBuilder:
    """从 Scribe 构图。

    Example::

        builder = GraphBuilder(scribe)
        snap = await builder.build()
        print(snap.node_count, snap.edge_count, snap.insights)
    """

    DIRECT_WEIGHT = 3.0
    SOURCE_WEIGHT = 4.0
    AA_WEIGHT = 1.5
    TYPE_WEIGHT = 1.0

    # 低凝聚 / 大社区阈值
    LOW_COHESION_DENSITY = 0.15
    LARGE_COMMUNITY_SIZE = 20
    SMALL_COMMUNITY_SIZE = 3
    BRIDGE_BETWEENNESS_PCT = 0.85  # top 15%

    def __init__(self, scribe: ScribeProtocol) -> None:
        self.scribe = scribe

    # -------- 公共入口 --------

    async def build(self, min_pattern_confidence: float = 0.0) -> GraphSnapshot:
        """主入口：拉数据 → 建节点 → 4 信号打边 → Louvain → 洞察。"""
        patterns, facts = await self._load_raw(min_pattern_confidence)
        nodes = self._build_nodes(patterns, facts)
        if not nodes:
            return GraphSnapshot(nodes=[], edges=[], communities={}, insights=[])

        edges = self._compute_edges(nodes, patterns, facts)

        G = self._to_nx(nodes, edges)
        communities = self._louvain(G)

        # 把 community id 回填到节点
        node_by_id = {n.id: n for n in nodes}
        for cid, members in communities.items():
            for nid in members:
                if nid in node_by_id:
                    node_by_id[nid].community = cid

        insights = self._insights(G, communities)

        return GraphSnapshot(
            nodes=nodes,
            edges=edges,
            communities=communities,
            insights=insights,
        )

    # -------- 数据加载 --------

    async def _load_raw(
        self,
        min_pattern_confidence: float,
    ) -> tuple[list[Pattern], list[Fact]]:
        """直接从 Scribe 拉所有 active patterns + active facts。

        优先使用 ``list_all_patterns`` / ``list_all_facts``（DS Code 自加方法）；
        如果运行时 Scribe 实现没有这些方法，则回退到 ``search_facts("")`` 等公开接口。
        """
        patterns: list[Pattern]
        facts: list[Fact]

        list_p = getattr(self.scribe, "list_all_patterns", None)
        if callable(list_p):
            patterns = await list_p(status="active")
        else:  # pragma: no cover - 兼容老 Scribe
            patterns = await self.scribe.patterns_for_task("", min_confidence=0.0)

        if min_pattern_confidence > 0.0:
            patterns = [p for p in patterns if p.confidence >= min_pattern_confidence]

        list_f = getattr(self.scribe, "list_all_facts", None)
        if callable(list_f):
            facts = await list_f(status="active")
        else:  # pragma: no cover
            facts = await self.scribe.search_facts("", top_k=10000)

        return patterns, facts

    # -------- 建节点 --------

    @staticmethod
    def _build_nodes(patterns: list[Pattern], facts: list[Fact]) -> list[GraphNode]:
        nodes: list[GraphNode] = []
        for p in patterns:
            nodes.append(
                GraphNode(
                    id=f"p:{p.id}",
                    label=p.trigger_condition[:80] or p.id,
                    type="pattern",
                    pattern_type=p.pattern_type,
                    metadata={
                        "confidence": p.confidence,
                        "success_rate": p.success_rate,
                        "sample_count": p.sample_count,
                        "session_ids": list(p.session_ids),
                        "action_template": p.action_template,
                    },
                )
            )
        for f in facts:
            label = f"{f.subject} — {f.predicate} — {f.object}"
            nodes.append(
                GraphNode(
                    id=f"f:{f.id}",
                    label=label[:120],
                    type="fact",
                    pattern_type=None,
                    metadata={
                        "subject": f.subject,
                        "predicate": f.predicate,
                        "object": f.object,
                        "confidence": f.confidence,
                        "source_raw_event_id": f.source_raw_event_id,
                        "provenance_chain": list(f.provenance_chain),
                    },
                )
            )
        return nodes

    # -------- 边计算（4 信号） --------

    def _compute_edges(
        self,
        nodes: list[GraphNode],
        patterns: list[Pattern],
        facts: list[Fact],
    ) -> list[GraphEdge]:
        """合成 4 信号边权。返回去重后的边列表（无向：source < target）。"""
        pattern_by_node_id = {f"p:{p.id}": p for p in patterns}
        fact_by_node_id = {f"f:{f.id}": f for f in facts}

        # 边累计 buffer：(a, b) -> {signal_name: contribution}
        accum: dict[tuple[str, str], dict[str, float]] = defaultdict(
            lambda: defaultdict(float)
        )

        def _key(a: str, b: str) -> tuple[str, str] | None:
            if a == b:
                return None
            return (a, b) if a < b else (b, a)

        def _add(a: str, b: str, signal: str, contribution: float) -> None:
            k = _key(a, b)
            if k is None:
                return
            accum[k][signal] += contribution

        # --- 1. source_overlap ---
        # Facts: 按 source_raw_event_id 与 provenance_chain 元素分桶
        raw_event_to_facts: dict[str, list[str]] = defaultdict(list)
        for f in facts:
            sources: set[str] = set()
            if f.source_raw_event_id:
                sources.add(f.source_raw_event_id)
            for ev in f.provenance_chain:
                if ev:
                    sources.add(ev)
            for ev_id in sources:
                raw_event_to_facts[ev_id].append(f"f:{f.id}")
        for nid_list in raw_event_to_facts.values():
            if len(nid_list) < 2:
                continue
            for i, a in enumerate(nid_list):
                for b in nid_list[i + 1 :]:
                    _add(a, b, "source_overlap", self.SOURCE_WEIGHT)

        # Patterns: 共享 session_id
        session_to_patterns: dict[str, list[str]] = defaultdict(list)
        for p in patterns:
            for sid in p.session_ids:
                if sid:
                    session_to_patterns[sid].append(f"p:{p.id}")
        for nid_list in session_to_patterns.values():
            if len(nid_list) < 2:
                continue
            for i, a in enumerate(nid_list):
                for b in nid_list[i + 1 :]:
                    _add(a, b, "source_overlap", self.SOURCE_WEIGHT)

        # --- 2. direct_link ---
        # Pattern.action_template 字符串化后若包含其它节点的关键 token，则视为引用
        all_node_ids_short: dict[str, str] = {}
        for p in patterns:
            all_node_ids_short[p.id] = f"p:{p.id}"
        for f in facts:
            all_node_ids_short[f.id] = f"f:{f.id}"

        # Pattern -> Pattern / Pattern -> Fact 通过 action_template 引用
        for p in patterns:
            blob = _stringify(p.action_template)
            if not blob:
                continue
            src_node = f"p:{p.id}"
            for other_short, other_full in all_node_ids_short.items():
                if other_full == src_node:
                    continue
                if other_short and other_short in blob:
                    _add(src_node, other_full, "direct_link", self.DIRECT_WEIGHT)

        # Fact.subject ⇄ Fact.object 字面相等的视为弱直链（subject == object 字符串）
        # 经验：同主语 / 同宾语的 fact 通常讲同一对象。
        subject_to_facts: dict[str, list[str]] = defaultdict(list)
        object_to_facts: dict[str, list[str]] = defaultdict(list)
        for f in facts:
            subject_to_facts[f.subject].append(f"f:{f.id}")
            object_to_facts[f.object].append(f"f:{f.id}")
        for bucket in (subject_to_facts, object_to_facts):
            for nid_list in bucket.values():
                if len(nid_list) < 2:
                    continue
                for i, a in enumerate(nid_list):
                    for b in nid_list[i + 1 :]:
                        _add(a, b, "direct_link", self.DIRECT_WEIGHT)

        # --- 3. type_affinity（仅 patterns） ---
        type_to_patterns: dict[str, list[str]] = defaultdict(list)
        for p in patterns:
            type_to_patterns[p.pattern_type].append(f"p:{p.id}")
        for nid_list in type_to_patterns.values():
            if len(nid_list) < 2:
                continue
            for i, a in enumerate(nid_list):
                for b in nid_list[i + 1 :]:
                    _add(a, b, "type_affinity", self.TYPE_WEIGHT)

        # --- 4. adamic_adar（在已生成的基础图上叠加） ---
        # 先用前三种信号构建临时图，再为已有节点对加 AA 分量
        base_G = nx.Graph()
        base_G.add_nodes_from(n.id for n in nodes)
        for (a, b), sigs in accum.items():
            base_G.add_edge(a, b, weight=sum(sigs.values()))

        if base_G.number_of_edges() > 0:
            aa_scores = self._compute_aa(base_G)
            for (a, b), aa in aa_scores.items():
                if aa <= 0:
                    continue
                k = _key(a, b)
                if k is None:
                    continue
                accum[k]["adamic_adar"] += self.AA_WEIGHT * aa

        # 汇总成 GraphEdge 列表
        edges: list[GraphEdge] = []
        for (a, b), sigs in accum.items():
            weight = sum(sigs.values())
            if weight <= 0:
                continue
            edges.append(
                GraphEdge(
                    source=a,
                    target=b,
                    weight=weight,
                    signals=dict(sigs),
                )
            )
        return edges

    # -------- Adamic-Adar --------

    def _compute_aa(self, G: nx.Graph) -> dict[tuple[str, str], float]:
        """对图中所有已存在边的端点计算 AA。

        AA(u, v) = sum_{w in N(u) ∩ N(v)} 1 / log(deg(w))

        我们只对已存在的边端点叠加 AA（不引入新边）—— 这让 AA 起到"放大已知相邻关系"
        的作用，避免凭空连接图中遥远的节点对。
        """
        scores: dict[tuple[str, str], float] = {}
        if G.number_of_edges() == 0:
            return scores
        edges = list(G.edges())
        try:
            iterator = nx.adamic_adar_index(G, edges)
        except ZeroDivisionError:
            return scores
        for u, v, score in iterator:
            if not math.isfinite(score) or score <= 0:
                continue
            a, b = (u, v) if u < v else (v, u)
            scores[(a, b)] = float(score)
        return scores

    # -------- Louvain --------

    def _to_nx(self, nodes: list[GraphNode], edges: list[GraphEdge]) -> nx.Graph:
        G = nx.Graph()
        for n in nodes:
            G.add_node(n.id, label=n.label, type=n.type)
        for e in edges:
            G.add_edge(e.source, e.target, weight=e.weight)
        return G

    def _louvain(self, G: nx.Graph) -> dict[int, list[str]]:
        if G.number_of_nodes() == 0:
            return {}
        if G.number_of_edges() == 0:
            # 没有边时每个节点单独成一个社区，便于下游展示
            return {i: [n] for i, n in enumerate(G.nodes())}
        try:
            comms = louvain_communities(G, weight="weight", seed=42)
        except Exception:  # pragma: no cover - networkx 行为兜底
            comms = [set(G.nodes())]
        out: dict[int, list[str]] = {}
        for i, members in enumerate(comms):
            out[i] = sorted(members)
        return out

    # -------- Insights --------

    def _insights(
        self,
        G: nx.Graph,
        communities: dict[int, list[str]],
    ) -> list[str]:
        """生成洞察清单（孤立 / 桥 / 低凝聚 / 大社区）。"""
        insights: list[str] = []
        if G.number_of_nodes() == 0:
            return insights

        # 1. 孤立节点
        isolated = [n for n in G.nodes() if G.degree(n) == 0]
        if isolated:
            preview = ", ".join(isolated[:5])
            extra = "" if len(isolated) <= 5 else f" 等 {len(isolated)} 个"
            insights.append(f"孤立节点（度数=0）: {preview}{extra}")

        # 2. 桥节点（betweenness 高）
        if G.number_of_edges() > 0 and G.number_of_nodes() >= 3:
            try:
                bc = nx.betweenness_centrality(G, weight="weight")
            except Exception:  # pragma: no cover
                bc = {}
            if bc:
                sorted_bc = sorted(bc.items(), key=lambda kv: kv[1], reverse=True)
                # 取 top 15% 且 score > 0
                top_n = max(1, int(len(sorted_bc) * (1 - self.BRIDGE_BETWEENNESS_PCT)))
                bridges = [
                    (nid, score)
                    for nid, score in sorted_bc[:top_n]
                    if score > 0.0
                ]
                if bridges:
                    bridge_labels = ", ".join(f"{nid}({score:.2f})" for nid, score in bridges[:5])
                    insights.append(f"桥节点（高介数中心性）: {bridge_labels}")

        # 3. 低凝聚社区 / 大社区
        for cid, members in communities.items():
            size = len(members)
            if size == 0:
                continue
            if size < self.SMALL_COMMUNITY_SIZE:
                insights.append(
                    f"小社区 #{cid} 仅 {size} 个节点，建议合并或归档"
                )
                continue
            if size > self.LARGE_COMMUNITY_SIZE:
                insights.append(
                    f"大社区 #{cid} 含 {size} 个节点，可能需要拆分"
                )
            # 计算社区内部边密度
            sub = G.subgraph(members)
            if sub.number_of_nodes() >= 2:
                possible = sub.number_of_nodes() * (sub.number_of_nodes() - 1) / 2
                density = sub.number_of_edges() / possible if possible > 0 else 0.0
                if density < self.LOW_COHESION_DENSITY and size >= self.SMALL_COMMUNITY_SIZE:
                    insights.append(
                        f"社区 #{cid} 内部边密度 {density:.2f} 偏低（< {self.LOW_COHESION_DENSITY}）"
                    )

        return insights


# ============================================================
# 工具函数
# ============================================================


def _stringify(obj: Any) -> str:
    """把任意 JSON-like 对象拍平成字符串，用于 direct_link 子串匹配。"""
    if obj is None:
        return ""
    if isinstance(obj, str):
        return obj
    if isinstance(obj, (int, float, bool)):
        return str(obj)
    if isinstance(obj, dict):
        return " ".join(
            f"{_stringify(k)} {_stringify(v)}" for k, v in obj.items()
        )
    if isinstance(obj, (list, tuple, set)):
        return " ".join(_stringify(x) for x in obj)
    return str(obj)


__all__ = [
    "GraphBuilder",
    "GraphEdge",
    "GraphNode",
    "GraphSnapshot",
]
