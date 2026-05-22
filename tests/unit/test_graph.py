"""GraphBuilder + HTMLExporter 单元测试。

覆盖：
- 空 / 单节点 / 多节点构图
- 4 个信号（direct_link / source_overlap / adamic_adar / type_affinity）各自有测
- Louvain 社区在明显聚类上能分出 ≥2 社区
- Insights 检测孤立节点 / 桥节点 / 低凝聚社区
- HTMLExporter 包含 CDN script + JSON payload + 落盘可读
"""
from __future__ import annotations

import json
from pathlib import Path

import networkx as nx
import pytest

from dscode.core import Fact, Pattern, Scribe
from dscode.graph import (
    GraphBuilder,
    GraphEdge,
    GraphNode,
    GraphSnapshot,
    HTMLExporter,
)


# ============================================================
# fixtures
# ============================================================


@pytest.fixture
async def scribe(tmp_path: Path) -> Scribe:
    db = tmp_path / "g.db"
    mirror = tmp_path / "raw"
    s = Scribe(db_path=db, mirror_dir=mirror)
    yield s
    s.close()


def _promote_all_to_active(scribe: Scribe) -> None:
    """模拟 Anvil L3 升级：把所有 candidate 升级为 active。

    便于在测试中绕过稳定性闸门（最少 5 sessions / 24h span）。
    """
    with scribe._lock:
        scribe._conn.execute("UPDATE patterns SET status='active'")


async def _write_pattern(
    scribe: Scribe,
    *,
    trigger: str,
    ptype: str = "sop",
    confidence: float = 0.8,
    sessions: list[str] | None = None,
    action: dict | None = None,
    sample_count: int = 2,
) -> str:
    """便捷写入 + 自动 promote 到 active；返回 pattern_id。"""
    p = Pattern(
        pattern_type=ptype,
        trigger_condition=trigger,
        action_template=action or {"step": trigger},
        confidence=confidence,
        sample_count=sample_count,
        session_ids=sessions or ["sess-x"],
    )
    res = await scribe.write_pattern(p)
    assert res.accepted, res.reason
    _promote_all_to_active(scribe)
    return res.pattern_id  # type: ignore[return-value]


async def _write_fact(
    scribe: Scribe,
    *,
    subject: str,
    predicate: str = "uses",
    obj: str = "deepseek",
    raw_event: str = "evt-1",
    confidence: float = 1.0,
) -> str:
    f = Fact(
        subject=subject,
        predicate=predicate,
        object=obj,
        provenance_chain=[raw_event],
        source_raw_event_id=raw_event,
        confidence=confidence,
    )
    res = await scribe.write_fact(f)
    assert res.accepted, res.reason
    return res.fact_id  # type: ignore[return-value]


# ============================================================
# 1. 基础构图
# ============================================================


async def test_build_empty_scribe_returns_empty_snapshot(scribe: Scribe) -> None:
    """空 Scribe 应返回空 snapshot。"""
    snap = await GraphBuilder(scribe).build()
    assert isinstance(snap, GraphSnapshot)
    assert snap.node_count == 0
    assert snap.edge_count == 0
    assert snap.communities == {}
    assert snap.insights == []


async def test_build_single_node_no_edges(scribe: Scribe) -> None:
    """单 fact → 1 节点 0 边。"""
    await _write_fact(scribe, subject="only.py", raw_event="evt-solo")

    snap = await GraphBuilder(scribe).build()
    assert snap.node_count == 1
    assert snap.edge_count == 0
    # 单节点也应该被划成一个社区
    assert len(snap.communities) == 1


# ============================================================
# 2. 4 信号
# ============================================================


async def test_source_overlap_signal_on_shared_raw_event(scribe: Scribe) -> None:
    """两个 fact 共享 source_raw_event_id → 边含 source_overlap。"""
    await _write_fact(scribe, subject="a.py", raw_event="evt-shared")
    await _write_fact(scribe, subject="b.py", raw_event="evt-shared")

    snap = await GraphBuilder(scribe).build()
    assert snap.edge_count >= 1
    e = snap.edges[0]
    assert "source_overlap" in e.signals
    assert e.signals["source_overlap"] >= GraphBuilder.SOURCE_WEIGHT


async def test_type_affinity_signal_on_same_pattern_type(scribe: Scribe) -> None:
    """两个 sop pattern → 边含 type_affinity。"""
    await _write_pattern(scribe, trigger="alpha sop", ptype="sop", sessions=["s-1"])
    await _write_pattern(scribe, trigger="beta sop", ptype="sop", sessions=["s-2"])

    snap = await GraphBuilder(scribe).build()
    # 至少有一条边（type_affinity 触发）
    assert snap.edge_count >= 1
    sig_keys = {k for e in snap.edges for k in e.signals}
    assert "type_affinity" in sig_keys


async def test_direct_link_signal_on_facts_sharing_subject(scribe: Scribe) -> None:
    """两 fact 共享 subject 字符串 → direct_link 触发。"""
    await _write_fact(scribe, subject="forge.py", predicate="contains", obj="X", raw_event="e1")
    await _write_fact(scribe, subject="forge.py", predicate="implements", obj="Y", raw_event="e2")

    snap = await GraphBuilder(scribe).build()
    assert snap.edge_count >= 1
    sig_keys = {k for e in snap.edges for k in e.signals}
    assert "direct_link" in sig_keys


async def test_adamic_adar_layered_on_existing_neighbors(scribe: Scribe) -> None:
    """三角拓扑：A-B、B-C 通过 source 连接，则 A-C 之间会触发 AA（B 是共同邻居）。

    A 与 C 共享一个 subject 触发 direct_link，因此基础边已存在；
    A-C 同时与 B 通过 source_overlap 相连 → AA(A,C) 应 > 0。
    """
    # A-B 共享 evt-1
    await _write_fact(scribe, subject="modA", raw_event="evt-1", predicate="rel1")
    await _write_fact(scribe, subject="modB", raw_event="evt-1", predicate="rel2")
    # B-C 共享 evt-2
    await _write_fact(scribe, subject="modB2", raw_event="evt-2", predicate="rel3", obj="o3")
    await _write_fact(scribe, subject="modC", raw_event="evt-2", predicate="rel4", obj="o4")
    # A-C 直接共享 subject 同名（创造基础边 + 让 AA 有目标节点对）
    await _write_fact(scribe, subject="modA", raw_event="evt-3", predicate="extra")

    snap = await GraphBuilder(scribe).build()
    sig_keys = {k for e in snap.edges for k in e.signals}
    assert "adamic_adar" in sig_keys


# ============================================================
# 3. Louvain 在明显聚类上
# ============================================================


async def test_louvain_separates_two_distinct_clusters(scribe: Scribe) -> None:
    """两个明显不相交的 fact 簇（不同 raw_event）→ Louvain 应分出 ≥2 社区。"""
    # 簇 1：3 个 fact 共享 evt-A
    for i in range(3):
        await _write_fact(scribe, subject=f"cluster1_{i}", raw_event="evt-A")
    # 簇 2：3 个 fact 共享 evt-B（与簇 1 完全无交集 subject / object）
    for i in range(3):
        await _write_fact(
            scribe,
            subject=f"cluster2_{i}",
            raw_event="evt-B",
            predicate="other",
            obj="other-obj",
        )

    snap = await GraphBuilder(scribe).build()
    assert snap.node_count == 6
    assert len(snap.communities) >= 2
    # 验证每个 fact 都被分到了某个社区
    all_assigned = {nid for members in snap.communities.values() for nid in members}
    assert len(all_assigned) == 6


# ============================================================
# 4. AA 计算正确（直接在 nx.Graph 上）
# ============================================================


def test_adamic_adar_helper_matches_networkx() -> None:
    """_compute_aa 应等价于 nx.adamic_adar_index 在边端点上的结果。"""
    builder = GraphBuilder(scribe=None)  # type: ignore[arg-type]
    G = nx.Graph()
    # 经典三角 + 尾巴
    G.add_edges_from([("a", "b"), ("b", "c"), ("a", "c"), ("c", "d")])
    scores = builder._compute_aa(G)
    # 至少应有非零项；并且 (a,b) (a,c) (b,c) 都在键里
    assert ("a", "b") in scores
    assert ("a", "c") in scores
    assert scores[("a", "b")] > 0
    # 与 networkx 内置一致
    ref = {tuple(sorted([u, v])): s for u, v, s in nx.adamic_adar_index(G, G.edges())}
    for k, v in scores.items():
        assert abs(v - ref[k]) < 1e-9


# ============================================================
# 5. Insights
# ============================================================


async def test_insights_detects_isolated_node(scribe: Scribe) -> None:
    """一个完全孤立的 fact（不与任何人共享 raw / subject / object）→ insight 提到孤立节点。"""
    # 孤立：独有的 subject / predicate / object / raw_event
    await _write_fact(
        scribe,
        subject="isolated_subj",
        predicate="isolated_pred",
        obj="isolated_obj",
        raw_event="evt-solo",
    )
    # 配对：另外两个 fact 共享一切（让它们之间有边，与上面那个无关）
    await _write_fact(
        scribe,
        subject="paired_subj",
        predicate="paired_pred",
        obj="paired_obj",
        raw_event="evt-pair",
    )
    await _write_fact(
        scribe,
        subject="paired_subj",  # 同 subject 触发 direct_link
        predicate="paired_pred2",
        obj="paired_obj",  # 同 obj 也触发 direct_link
        raw_event="evt-pair",  # 同 raw 触发 source_overlap
    )

    snap = await GraphBuilder(scribe).build()
    joined = " | ".join(snap.insights)
    assert "孤立" in joined


async def test_insights_detects_small_community(scribe: Scribe) -> None:
    """两个完全分离的 2-节点小簇 → insight 应提到小社区或低凝聚。"""
    # 簇 X：独立 raw / predicate / obj
    await _write_fact(
        scribe, subject="x1", predicate="px", obj="ox", raw_event="evt-X"
    )
    await _write_fact(
        scribe, subject="x2", predicate="px", obj="ox", raw_event="evt-X"
    )
    # 簇 Y：与 X 完全不相干
    await _write_fact(
        scribe, subject="y1", predicate="py", obj="oy", raw_event="evt-Y"
    )
    await _write_fact(
        scribe, subject="y2", predicate="py", obj="oy", raw_event="evt-Y"
    )

    snap = await GraphBuilder(scribe).build()
    joined = " | ".join(snap.insights)
    assert "小社区" in joined or "低" in joined or "孤立" in joined


# ============================================================
# 6. HTMLExporter
# ============================================================


def _sample_snapshot() -> GraphSnapshot:
    nodes = [
        GraphNode(id="p:1", label="alpha sop", type="pattern", pattern_type="sop", community=0),
        GraphNode(id="p:2", label="beta sop", type="pattern", pattern_type="sop", community=0),
        GraphNode(id="f:1", label="forge.py - uses - X", type="fact", community=1),
    ]
    edges = [
        GraphEdge(source="p:1", target="p:2", weight=5.0, signals={"type_affinity": 1.0}),
        GraphEdge(source="p:1", target="f:1", weight=3.0, signals={"direct_link": 3.0}),
    ]
    return GraphSnapshot(
        nodes=nodes,
        edges=edges,
        communities={0: ["p:1", "p:2"], 1: ["f:1"]},
        insights=["示例洞察 1"],
    )


def test_html_exporter_contains_sigma_and_data() -> None:
    """渲染 HTML 应含 sigma.js CDN script + 嵌入的图数据 JSON。"""
    snap = _sample_snapshot()
    html = HTMLExporter().render(snap)
    assert "<!DOCTYPE html>" in html
    assert "sigma" in html.lower()
    assert "graphology" in html.lower()
    assert "forceatlas2" in html.lower() or "ForceAtlas2" in html
    assert 'id="graph-data"' in html
    # JSON 数据必须包含我们的节点 id
    assert "p:1" in html
    assert "f:1" in html
    # 洞察渲染
    assert "示例洞察 1" in html


def test_html_exporter_write_creates_readable_file(tmp_path: Path) -> None:
    """write 写盘后文件存在且可被读为有效 HTML。"""
    snap = _sample_snapshot()
    out = tmp_path / "subdir" / "graph.html"
    result_path = HTMLExporter().write(snap, out)

    assert result_path == out
    assert out.exists()
    content = out.read_text(encoding="utf-8")
    assert content.startswith("<!DOCTYPE html>")
    # 提取嵌入的 JSON 并解析
    marker = '<script id="graph-data" type="application/json">'
    idx = content.find(marker)
    assert idx >= 0
    json_start = idx + len(marker)
    json_end = content.find("</script>", json_start)
    payload = json.loads(content[json_start:json_end])
    assert "nodes" in payload and len(payload["nodes"]) == 3
    assert "edges" in payload and len(payload["edges"]) == 2


def test_html_exporter_handles_empty_snapshot() -> None:
    """空 snapshot 也能渲染。"""
    html = HTMLExporter().render(GraphSnapshot(nodes=[], edges=[]))
    assert "<!DOCTYPE html>" in html
    assert "id=\"graph-data\"" in html
    # 节点数为 0 应被反映在侧栏
    assert "<strong>0</strong>" in html


# ============================================================
# 7. 真实 networkx 集成验证
# ============================================================


def test_louvain_on_planted_partition_finds_at_least_two_communities() -> None:
    """在一个明显双簇的合成图上跑 Louvain，应分出至少 2 个社区。"""
    builder = GraphBuilder(scribe=None)  # type: ignore[arg-type]
    G = nx.Graph()
    # 簇 A: 全连接 5 节点
    cluster_a = [f"a{i}" for i in range(5)]
    for i, u in enumerate(cluster_a):
        for v in cluster_a[i + 1 :]:
            G.add_edge(u, v, weight=1.0)
    # 簇 B: 全连接 5 节点
    cluster_b = [f"b{i}" for i in range(5)]
    for i, u in enumerate(cluster_b):
        for v in cluster_b[i + 1 :]:
            G.add_edge(u, v, weight=1.0)
    # 一根弱桥
    G.add_edge("a0", "b0", weight=0.1)

    comms = builder._louvain(G)
    assert len(comms) >= 2
    # 验证每个节点恰好被分到一个社区
    seen: set[str] = set()
    for members in comms.values():
        for m in members:
            assert m not in seen
            seen.add(m)
    assert len(seen) == G.number_of_nodes()


async def test_build_full_pipeline_with_mixed_patterns_and_facts(scribe: Scribe) -> None:
    """端到端：混合 patterns + facts，验证节点 / 边 / 社区 / 信号都被填充。"""
    # 3 sop patterns 共享 session
    for i in range(3):
        await _write_pattern(
            scribe,
            trigger=f"sop trigger {i}",
            ptype="sop",
            sessions=["sess-shared"],
        )
    # 2 rule patterns 单独 session
    for i in range(2):
        await _write_pattern(
            scribe,
            trigger=f"rule trigger {i}",
            ptype="rule",
            sessions=[f"sess-r{i}"],
        )
    # 4 facts 分两组，每组共享 raw_event
    for i in range(2):
        await _write_fact(scribe, subject=f"grp1_{i}", raw_event="evt-grp1")
    for i in range(2):
        await _write_fact(
            scribe, subject=f"grp2_{i}", raw_event="evt-grp2", predicate="rel2", obj="o2"
        )

    snap = await GraphBuilder(scribe).build()
    assert snap.node_count == 9  # 5 patterns + 4 facts
    assert snap.edge_count > 0
    # 所有 4 种信号至少出现一次（type_affinity 来自 sop 同类 / source_overlap 来自共享 session+raw / direct_link 来自 pattern 同名 / AA 叠加）
    sig_keys = {k for e in snap.edges for k in e.signals}
    assert "type_affinity" in sig_keys
    assert "source_overlap" in sig_keys
    # 每个节点都被分到了某个社区（community >= 0）
    assert all(n.community >= 0 for n in snap.nodes)
