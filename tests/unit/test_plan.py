"""Plan 阶段单元测试。

- SpecLoader：默认模板复制、format 输出
- GrillMe：mock LLM 跑 5 轮、is_final 提前结束、空响应安全退出
- PRDGenerator：写盘文件存在、内容可解析
- PlanRunner：端到端组合
"""
from __future__ import annotations

import json
from collections.abc import AsyncGenerator
from pathlib import Path
from typing import Any

import pytest

from dscode.core.types import (
    ContextManifest,
    LLMResponse,
    Message,
    PRDDocument,
)
from dscode.plan import GrillMe, PlanRunner, PRDGenerator, SpecLoader

# ============================================================
# Fakes
# ============================================================

class FakeLLM:
    """脚本化 LLM；按预设字符串序列依次返回 content。"""

    def __init__(self, contents: list[str]) -> None:
        self._contents = list(contents)
        self.received: list[list[Message]] = []
        self.call_count = 0

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
        self.call_count += 1
        self.received.append(list(messages))
        if not self._contents:
            return LLMResponse(content="", finish_reason="stop", model=model)
        return LLMResponse(
            content=self._contents.pop(0),
            finish_reason="stop",
            model=model,
        )

    async def chat_stream(self, *args: Any, **kwargs: Any) -> AsyncGenerator[LLMResponse, None]:  # pragma: no cover
        if False:
            yield  # type: ignore[unreachable]
        raise NotImplementedError


def _grill_json(question: str, recommended: str, is_final: bool = False) -> str:
    return json.dumps(
        {
            "question": question,
            "recommended_answer": recommended,
            "rationale": "test",
            "is_final": is_final,
        },
        ensure_ascii=False,
    )


# ============================================================
# SpecLoader
# ============================================================

class TestSpecLoader:
    def test_bootstrap_copies_default_templates(self, tmp_path: Path) -> None:
        loader = SpecLoader(tmp_path)
        files = loader.load_all()
        # 默认模板：architecture / conventions / safety
        assert "architecture.md" in files
        assert "conventions.md" in files
        assert "safety.md" in files
        # 文件确实落盘了
        spec_dir = tmp_path / ".dscode" / "spec"
        assert (spec_dir / "architecture.md").exists()
        assert (spec_dir / "conventions.md").exists()
        assert (spec_dir / "safety.md").exists()

    def test_does_not_overwrite_existing(self, tmp_path: Path) -> None:
        spec_dir = tmp_path / ".dscode" / "spec"
        spec_dir.mkdir(parents=True)
        custom = spec_dir / "conventions.md"
        custom.write_text("CUSTOM CONTENT", encoding="utf-8")

        loader = SpecLoader(tmp_path)
        files = loader.load_all()
        assert files["conventions.md"] == "CUSTOM CONTENT"

    def test_format_for_prompt_includes_filenames(self, tmp_path: Path) -> None:
        loader = SpecLoader(tmp_path)
        loader.load_all()
        text = loader.format_for_prompt()
        assert "## spec: architecture.md" in text
        assert "## spec: conventions.md" in text
        assert "## spec: safety.md" in text

    def test_empty_spec_dir_returns_empty_string(self, tmp_path: Path) -> None:
        spec_dir = tmp_path / ".dscode" / "spec"
        spec_dir.mkdir(parents=True)  # 存在但为空 -> 不触发模板复制
        loader = SpecLoader(tmp_path)
        assert loader.load_all() == {}
        assert loader.format_for_prompt() == ""


# ============================================================
# GrillMe
# ============================================================

class TestGrillMe:
    async def test_interview_runs_max_rounds_without_user_callback(self) -> None:
        """非交互模式下，跑满 max_rounds（除非 is_final）。"""
        contents = [
            _grill_json(f"Q{i}", f"A{i}") for i in range(1, 6)
        ]
        llm = FakeLLM(contents)
        grill = GrillMe(llm=llm)
        qa = await grill.interview(
            task_description="refactor user service",
            spec_text="some spec",
            max_rounds=5,
            ask_user=None,
        )
        assert len(qa) == 5
        # 非交互模式 -> 直接采用 recommended_answer
        assert qa[0] == ("Q1", "A1")
        assert qa[4] == ("Q4", "A4") or qa[4] == ("Q5", "A5")
        assert llm.call_count == 5

    async def test_interview_stops_on_is_final(self) -> None:
        contents = [
            _grill_json("Q1", "A1", is_final=False),
            _grill_json("Q2", "A2", is_final=False),
            _grill_json("Q3", "A3", is_final=True),
            _grill_json("Q4", "should-not-reach"),
        ]
        llm = FakeLLM(contents)
        grill = GrillMe(llm=llm)
        qa = await grill.interview(
            task_description="task",
            spec_text="",
            max_rounds=10,
            ask_user=None,
        )
        assert len(qa) == 3
        assert qa[-1] == ("Q3", "A3")

    async def test_interview_uses_user_callback(self) -> None:
        contents = [
            _grill_json("what model?", "deepseek-v4-pro"),
            _grill_json("test framework?", "pytest", is_final=True),
        ]
        llm = FakeLLM(contents)
        captured: list[tuple[str, str]] = []

        async def ask(q: str, rec: str) -> str:
            captured.append((q, rec))
            return f"user-said-{q}"

        grill = GrillMe(llm=llm)
        qa = await grill.interview(
            task_description="x",
            spec_text="",
            max_rounds=5,
            ask_user=ask,
        )
        assert qa == [
            ("what model?", "user-said-what model?"),
            ("test framework?", "user-said-test framework?"),
        ]
        assert captured[0] == ("what model?", "deepseek-v4-pro")

    async def test_interview_handles_invalid_json_gracefully(self) -> None:
        """LLM 返回非法 JSON 时应安全退出，不抛异常。"""
        llm = FakeLLM(["this is not json at all !!!"])
        grill = GrillMe(llm=llm)
        qa = await grill.interview(
            task_description="x",
            spec_text="",
            max_rounds=5,
            ask_user=None,
        )
        assert qa == []

    async def test_interview_uses_recommended_when_user_returns_empty(self) -> None:
        """用户回调返回空串 → 回落到 recommended_answer。"""
        contents = [_grill_json("Q1", "rec-A1", is_final=True)]
        llm = FakeLLM(contents)

        async def empty(q: str, rec: str) -> str:
            return "   "

        grill = GrillMe(llm=llm)
        qa = await grill.interview(
            task_description="x",
            spec_text="",
            max_rounds=3,
            ask_user=empty,
        )
        assert qa == [("Q1", "rec-A1")]


# ============================================================
# PRDGenerator
# ============================================================

def _prd_payload() -> str:
    return json.dumps(
        {
            "prd": {
                "task_description": "refactor user service",
                "goals": ["拆分模块", "增加测试"],
                "constraints": ["保持 API 向后兼容"],
                "acceptance_criteria": ["pytest 通过", "覆盖率 >= 80%"],
                "related_files": ["src/user_service.py"],
                "estimated_hours": 3.5,
                "risk_notes": ["可能影响登录流程"],
            },
            "implement_manifest": {
                "files": ["src/user_service.py", "src/auth.py"],
                "snippets": [{"path": "src/user_service.py", "reason": "main module"}],
                "related_facts": [],
            },
            "check_manifest": {
                "files": ["tests/test_user_service.py"],
                "snippets": [],
                "related_facts": [],
            },
        },
        ensure_ascii=False,
    )


class TestPRDGenerator:
    async def test_generate_returns_three_objects(self) -> None:
        llm = FakeLLM([_prd_payload()])
        gen = PRDGenerator(llm=llm)
        prd, impl, chk = await gen.generate(
            task_description="refactor user service",
            interview_qa=[("Q1", "A1")],
            spec_text="spec",
            related_files=["src/user_service.py"],
        )
        assert isinstance(prd, PRDDocument)
        assert isinstance(impl, ContextManifest)
        assert isinstance(chk, ContextManifest)
        assert prd.estimated_hours == pytest.approx(3.5)
        assert "拆分模块" in prd.goals
        assert impl.phase == "implement"
        assert chk.phase == "check"
        assert "src/auth.py" in impl.files
        assert "tests/test_user_service.py" in chk.files

    async def test_generate_falls_back_on_missing_fields(self) -> None:
        """LLM 返回字段不全时仍要安全构造 PRD。"""
        thin_payload = json.dumps({"prd": {}, "implement_manifest": {}, "check_manifest": {}})
        llm = FakeLLM([thin_payload])
        gen = PRDGenerator(llm=llm)
        prd, impl, chk = await gen.generate(
            task_description="trivial bug fix",
            interview_qa=[],
            spec_text="",
        )
        assert prd.task_description == "trivial bug fix"
        assert prd.goals  # 至少 1 条
        assert prd.acceptance_criteria
        assert impl.files == []
        assert chk.files == []

    def test_write_to_disk_creates_all_files(self, tmp_path: Path) -> None:
        gen = PRDGenerator(llm=FakeLLM([]))
        prd = PRDDocument(
            task_id="abc123",
            task_description="t",
            goals=["g"],
            constraints=[],
            acceptance_criteria=["a"],
        )
        impl = ContextManifest(phase="implement", files=["f.py"])
        chk = ContextManifest(phase="check", files=["t.py"])

        task_dir = gen.write_to_disk(prd, impl, chk, tmp_path)
        assert task_dir == tmp_path / ".dscode" / "tasks" / "abc123"
        assert (task_dir / "prd.md").exists()
        assert (task_dir / "prd.json").exists()
        assert (task_dir / "implement.jsonl").exists()
        assert (task_dir / "check.jsonl").exists()

        # prd.json 可解析
        parsed = json.loads((task_dir / "prd.json").read_text(encoding="utf-8"))
        assert parsed["task_id"] == "abc123"

        # implement.jsonl 每行都是合法 JSON
        for line in (task_dir / "implement.jsonl").read_text().splitlines():
            json.loads(line)


# ============================================================
# PlanRunner（端到端）
# ============================================================

class TestPlanRunner:
    async def test_run_end_to_end(self, tmp_path: Path) -> None:
        # 顺序：4 轮 grill（最后 is_final） + 1 轮 PRD
        scripted = [
            _grill_json("Q1", "A1"),
            _grill_json("Q2", "A2"),
            _grill_json("Q3", "A3"),
            _grill_json("Q4", "A4", is_final=True),
            _prd_payload(),
        ]
        llm = FakeLLM(scripted)
        runner = PlanRunner(llm=llm, project_root=tmp_path, max_grill_rounds=5)
        task_dir = await runner.run("refactor user service")
        assert task_dir.exists()
        assert (task_dir / "prd.md").exists()
        # spec 已经被 bootstrap
        assert (tmp_path / ".dscode" / "spec" / "conventions.md").exists()
        # 总共应调用 LLM 5 次（4 grill + 1 prd）
        assert llm.call_count == 5
