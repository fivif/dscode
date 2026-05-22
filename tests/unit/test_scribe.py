"""Scribe 单元测试。"""
from __future__ import annotations

from pathlib import Path

import pytest

from dscode.core import Fact, Pattern, RawEvent, Scribe


@pytest.fixture
async def scribe(tmp_path: Path) -> Scribe:
    db = tmp_path / "state.db"
    mirror = tmp_path / "raw"
    s = Scribe(db_path=db, mirror_dir=mirror)
    yield s
    s.close()


async def test_write_raw_and_recent(scribe: Scribe) -> None:
    sid = "sess-1"
    for i in range(5):
        await scribe.write_raw(
            RawEvent(
                session_id=sid,
                step_number=i,
                event_type="user_message",
                data={"step": i, "text": f"hello {i}"},
            )
        )
    out = await scribe.recent(n=3, session_id=sid)
    assert len(out) == 3
    # 时间正向：旧 → 新
    assert [e.step_number for e in out] == [2, 3, 4]


async def test_recent_filter_by_session(scribe: Scribe) -> None:
    await scribe.write_raw(
        RawEvent(session_id="a", step_number=1, event_type="user_message", data={})
    )
    await scribe.write_raw(
        RawEvent(session_id="b", step_number=1, event_type="user_message", data={})
    )
    only_a = await scribe.recent(n=10, session_id="a")
    assert len(only_a) == 1
    assert only_a[0].session_id == "a"


async def test_mirror_file_written(scribe: Scribe, tmp_path: Path) -> None:
    sid = "mirror-test"
    await scribe.write_raw(
        RawEvent(session_id=sid, step_number=1, event_type="user_message", data={"x": 1})
    )
    mirror_file = scribe.mirror_dir / f"{sid}.jsonl"
    assert mirror_file.exists()
    content = mirror_file.read_text(encoding="utf-8").strip().splitlines()
    assert len(content) == 1
    assert '"x": 1' in content[0] or '"x":1' in content[0]


async def test_write_fact_gates_missing_provenance(scribe: Scribe) -> None:
    fact = Fact(subject="foo.py", predicate="contains", object="def main")
    result = await scribe.write_fact(fact)
    assert result.accepted is False
    assert result.reason is not None
    assert "provenance" in result.reason.lower()


async def test_write_fact_with_provenance_and_search(scribe: Scribe) -> None:
    raw_event_id = "evt-001"
    fact = Fact(
        subject="parser.py",
        predicate="contains",
        object="DeepSeek client",
        provenance_chain=[raw_event_id],
        source_raw_event_id=raw_event_id,
    )
    result = await scribe.write_fact(fact)
    assert result.accepted is True
    assert result.fact_id == fact.id

    found = await scribe.search_facts("parser")
    assert len(found) >= 1
    assert any(f.id == fact.id for f in found)


async def test_search_facts_empty_query_returns_recent(scribe: Scribe) -> None:
    for i in range(3):
        await scribe.write_fact(
            Fact(
                subject=f"f{i}",
                predicate="rel",
                object=f"o{i}",
                provenance_chain=["pe"],
            )
        )
    found = await scribe.search_facts("", top_k=5)
    assert len(found) == 3


async def test_write_pattern_and_search(scribe: Scribe) -> None:
    """write_pattern 成功写入；不满足稳定性 → candidate（不被 patterns_for_task 返回）。"""
    p = Pattern(
        pattern_type="sop",
        trigger_condition="when implementing a new tool registry",
        action_template={"steps": ["draft spec", "write handler", "test"]},
        confidence=0.9,
        sample_count=5,
        success_rate=0.9,
    )
    result = await scribe.write_pattern(p)
    assert result.accepted is True
    # 单 session 写入 → 默认 candidate，patterns_for_task 只返回 active
    candidates = await scribe.list_pattern_candidates()
    assert any(x.id == p.id for x in candidates)
    # 升级后才能查到
    await scribe.update_pattern_status(p.id, "active")
    matches = await scribe.patterns_for_task("implementing tool registry", min_confidence=0.5)
    assert any(x.id == p.id for x in matches)


async def test_context_packet_returns_packet(scribe: Scribe) -> None:
    # 一条 fact
    await scribe.write_fact(
        Fact(
            subject="forge.py",
            predicate="implements",
            object="ReAct loop",
            provenance_chain=["e-1"],
        )
    )
    # 一条 pattern
    await scribe.write_pattern(
        Pattern(
            pattern_type="rule",
            trigger_condition="ReAct execution",
            action_template={"note": "stream events"},
            confidence=0.8,
            sample_count=3,
            success_rate=0.8,
        )
    )
    # 一条 raw event
    await scribe.write_raw(
        RawEvent(session_id="x", step_number=1, event_type="user_message", data={"q": "build"})
    )

    packet = await scribe.context_packet("ReAct forge", token_budget=8000)
    assert packet.total_tokens_estimate >= 0
    assert len(packet.facts) + len(packet.patterns) + len(packet.recent_events) >= 1


async def test_context_packet_respects_budget(scribe: Scribe) -> None:
    """极小预算下应返回较少内容。"""
    for i in range(10):
        await scribe.write_fact(
            Fact(
                subject=f"file_{i}.py",
                predicate="contains",
                object="some long content " * 50,
                provenance_chain=["e"],
            )
        )
    packet = await scribe.context_packet("file", token_budget=50)
    # 极小预算下，facts 总数应明显少于 10
    assert len(packet.facts) <= 5
    assert packet.total_tokens_estimate <= 100  # 留点余量
