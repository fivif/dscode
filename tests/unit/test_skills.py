"""SkillLoader 单元测试。"""
from __future__ import annotations

from pathlib import Path

import pytest

from dscode.skills.loader import (
    Skill,
    SkillLoader,
    SkillSummary,
    parse_skill_file,
    parse_skill_summary,
)

SKILL_A = """---
name: search-docs
description: 当用户问到文档相关内容时触发。
---

# search-docs

1. 先在 docs/ 下 grep 关键词
2. 命中后读取上下文
"""

SKILL_B = """---
name: refactor-module
description: 拆分大文件为多个职责清晰的小模块。
---

按 SRP 切分；每个新模块 < 300 行；保持 import 边界清晰。
"""

SKILL_NO_FM = """没有 frontmatter 的文件应被跳过。"""

SKILL_WITH_QUOTES = """---
name: "quoted-name"
description: '带引号的描述'
---

body
"""


def _write_skill(root: Path, name: str, content: str) -> Path:
    folder = root / name
    folder.mkdir(parents=True, exist_ok=True)
    fp = folder / "SKILL.md"
    fp.write_text(content, encoding="utf-8")
    return fp


class TestParseSkillFile:
    def test_parse_valid_frontmatter(self, tmp_path: Path) -> None:
        fp = _write_skill(tmp_path, "search-docs", SKILL_A)
        skill = parse_skill_file(fp)
        assert skill is not None
        assert skill.name == "search-docs"
        assert "用户问到文档" in skill.description
        assert "grep" in skill.body

    def test_parse_missing_frontmatter_returns_none(self, tmp_path: Path) -> None:
        fp = tmp_path / "raw.md"
        fp.write_text(SKILL_NO_FM, encoding="utf-8")
        assert parse_skill_file(fp) is None

    def test_parse_handles_quoted_values(self, tmp_path: Path) -> None:
        fp = _write_skill(tmp_path, "qskill", SKILL_WITH_QUOTES)
        skill = parse_skill_file(fp)
        assert skill is not None
        assert skill.name == "quoted-name"
        assert skill.description == "带引号的描述"

    def test_parse_nonexistent_path_returns_none(self, tmp_path: Path) -> None:
        assert parse_skill_file(tmp_path / "nope.md") is None


class TestSkillLoaderDiscover:
    def test_discovers_project_level_skills(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        skills_dir = proj / ".dscode" / "skills"
        skills_dir.mkdir(parents=True)
        _write_skill(skills_dir, "search-docs", SKILL_A)
        _write_skill(skills_dir, "refactor-module", SKILL_B)

        # 隔离 HOME，避免污染用户真实 skill 目录
        loader = SkillLoader(project_root=proj, user_home=tmp_path / "fake_home")
        skills = loader.discover()
        names = {s.name for s in skills}
        assert names == {"search-docs", "refactor-module"}

    def test_project_overrides_user(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        home = tmp_path / "home"
        proj_skills = proj / ".dscode" / "skills"
        home_skills = home / ".dscode" / "skills"
        proj_skills.mkdir(parents=True)
        home_skills.mkdir(parents=True)

        # user 级
        _write_skill(
            home_skills,
            "search-docs",
            """---
name: search-docs
description: user-level (should be overridden)
---
user body
""",
        )
        # project 级
        _write_skill(proj_skills, "search-docs", SKILL_A)

        loader = SkillLoader(project_root=proj, user_home=home)
        skills = loader.discover()
        assert len(skills) == 1
        assert "用户问到文档" in skills[0].description

    def test_empty_dirs_return_no_skills(self, tmp_path: Path) -> None:
        loader = SkillLoader(project_root=tmp_path, user_home=tmp_path / "no_home")
        assert loader.discover() == []

    def test_supports_flat_md_layout(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        flat_dir = proj / ".dscode" / "skills"
        flat_dir.mkdir(parents=True)
        # 直接 root/<name>.md 而非 root/<name>/SKILL.md
        (flat_dir / "search-docs.md").write_text(SKILL_A, encoding="utf-8")
        loader = SkillLoader(project_root=proj, user_home=tmp_path / "h")
        skills = loader.discover()
        assert {s.name for s in skills} == {"search-docs"}


class TestSkillLoaderFormat:
    def test_format_includes_name_and_description(self, tmp_path: Path) -> None:
        skill = Skill(
            name="x",
            description="desc",
            body="body",
            source_path=tmp_path / "fake",
        )
        loader = SkillLoader(project_root=tmp_path, user_home=tmp_path / "h")
        out = loader.format_for_system_prompt([skill])
        assert "**x**" in out
        assert "desc" in out
        assert "可用技能" in out
        assert "do_load_skill" in out

    def test_format_empty_returns_empty_string(self, tmp_path: Path) -> None:
        loader = SkillLoader(project_root=tmp_path, user_home=tmp_path / "h")
        assert loader.format_for_system_prompt([]) == ""

    def test_get_body_lazy(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        skills_dir = proj / ".dscode" / "skills"
        skills_dir.mkdir(parents=True)
        _write_skill(skills_dir, "search-docs", SKILL_A)
        loader = SkillLoader(project_root=proj, user_home=tmp_path / "h")
        body = loader.get_body("search-docs")
        assert body is not None
        assert "grep" in body
        assert loader.get_body("does-not-exist") is None


@pytest.mark.parametrize(
    "raw_value, expected",
    [
        ("simple", "simple"),
        ('"quoted"', "quoted"),
        ("'single'", "single"),
    ],
)
def test_parse_simple_yaml_quote_stripping(raw_value: str, expected: str) -> None:
    from dscode.skills.loader import _parse_simple_yaml

    parsed = _parse_simple_yaml(f"name: {raw_value}")
    assert parsed["name"] == expected


# ============================================================
# 懒激活 API（load_descriptions / activate_skill / format_descriptions_block）
# ============================================================


class TestLazyActivation:
    def test_parse_skill_summary_returns_summary_only(self, tmp_path: Path) -> None:
        fp = _write_skill(tmp_path, "search-docs", SKILL_A)
        summary = parse_skill_summary(fp)
        assert summary is not None
        assert isinstance(summary, SkillSummary)
        assert summary.name == "search-docs"
        assert "用户问到文档" in summary.description
        # SkillSummary 不包含 body 字段
        assert not hasattr(summary, "body")

    def test_load_descriptions_skips_body_io(self, tmp_path: Path) -> None:
        """load_descriptions 仅返回 name + description + path，不带 body。"""
        proj = tmp_path / "proj"
        skills_dir = proj / ".dscode" / "skills"
        skills_dir.mkdir(parents=True)
        _write_skill(skills_dir, "search-docs", SKILL_A)
        _write_skill(skills_dir, "refactor-module", SKILL_B)

        loader = SkillLoader(project_root=proj, user_home=tmp_path / "h")
        summaries = loader.load_descriptions()
        names = {s.name for s in summaries}
        assert names == {"search-docs", "refactor-module"}
        # 类型断言
        for s in summaries:
            assert isinstance(s, SkillSummary)

    def test_load_descriptions_respects_project_override(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        home = tmp_path / "home"
        proj_skills = proj / ".dscode" / "skills"
        home_skills = home / ".dscode" / "skills"
        proj_skills.mkdir(parents=True)
        home_skills.mkdir(parents=True)
        _write_skill(
            home_skills,
            "search-docs",
            """---
name: search-docs
description: user-level desc
---
user body
""",
        )
        _write_skill(proj_skills, "search-docs", SKILL_A)

        loader = SkillLoader(project_root=proj, user_home=home)
        summaries = loader.load_descriptions()
        assert len(summaries) == 1
        assert "用户问到文档" in summaries[0].description

    def test_activate_skill_returns_body(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        skills_dir = proj / ".dscode" / "skills"
        skills_dir.mkdir(parents=True)
        _write_skill(skills_dir, "search-docs", SKILL_A)
        loader = SkillLoader(project_root=proj, user_home=tmp_path / "h")
        body = loader.activate_skill("search-docs")
        assert body is not None
        assert "grep" in body

    def test_activate_skill_missing_returns_none(self, tmp_path: Path) -> None:
        loader = SkillLoader(project_root=tmp_path, user_home=tmp_path / "h")
        assert loader.activate_skill("ghost") is None

    def test_load_body_alias_matches_get_body(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        skills_dir = proj / ".dscode" / "skills"
        skills_dir.mkdir(parents=True)
        _write_skill(skills_dir, "search-docs", SKILL_A)
        loader = SkillLoader(project_root=proj, user_home=tmp_path / "h")
        assert loader.load_body("search-docs") == loader.get_body("search-docs")

    def test_format_descriptions_block_from_summaries(self, tmp_path: Path) -> None:
        proj = tmp_path / "proj"
        skills_dir = proj / ".dscode" / "skills"
        skills_dir.mkdir(parents=True)
        _write_skill(skills_dir, "search-docs", SKILL_A)
        _write_skill(skills_dir, "refactor-module", SKILL_B)
        loader = SkillLoader(project_root=proj, user_home=tmp_path / "h")

        # 不传 summaries 时内部自行加载
        block = loader.format_descriptions_block()
        assert "可用技能" in block
        assert "**search-docs**" in block
        assert "**refactor-module**" in block
        # 不应包含 body
        assert "grep" not in block
        assert "SRP" not in block

    def test_format_descriptions_block_empty(self, tmp_path: Path) -> None:
        loader = SkillLoader(project_root=tmp_path, user_home=tmp_path / "h")
        assert loader.format_descriptions_block() == ""

    def test_format_descriptions_block_with_explicit_list(self, tmp_path: Path) -> None:
        summary = SkillSummary(
            name="manual",
            description="手动构造",
            source_path=tmp_path / "fake",
        )
        loader = SkillLoader(project_root=tmp_path, user_home=tmp_path / "h")
        block = loader.format_descriptions_block([summary])
        assert "**manual**" in block
        assert "手动构造" in block
