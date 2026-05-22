"""SKILL 协议加载器（懒激活版）。

兼容 Anthropic / mattpocock trellis 风格的 SKILL.md：

    ---
    name: <skill-name>
    description: <一行触发说明>
    ---
    <skill body markdown>

发现路径（优先级从高到低）：
1. 项目级 `.dscode/skills/<name>/SKILL.md`
2. 用户级 `~/.dscode/skills/<name>/SKILL.md`
3. （未来）插件级—— v1 不实现

同名 skill：项目级覆盖用户级。

懒激活策略（v2）：
- 启动时只把 ``name + description`` 注入 system prompt（``format_descriptions_block``）。
- 模型按描述判断匹配后，由调用方读取 body 并以独立 message 注入下一轮。
- 旧 API（``discover`` / ``format_for_system_prompt`` / ``get_body``）保留，行为不变，
  以兼容现有测试与调用点。
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from pydantic import BaseModel, ConfigDict

_FRONTMATTER_RE = re.compile(
    r"^---\s*\n(?P<fm>.*?)\n---\s*\n?(?P<body>.*)\Z",
    re.DOTALL,
)


def _parse_simple_yaml(text: str) -> dict[str, str]:
    """极简 YAML 解析：仅支持 key: value 一层平铺。

    不引入 PyYAML 依赖（虽然 pyproject 已列入，但本模块要尽量轻）。
    支持值带引号；支持注释 `#`。
    """
    out: dict[str, str] = {}
    for raw_line in text.splitlines():
        line = raw_line.split("#", 1)[0].rstrip()
        if not line.strip():
            continue
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        key = key.strip()
        value = value.strip()
        # 剥引号
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {'"', "'"}:
            value = value[1:-1]
        if key:
            out[key] = value
    return out


class Skill(BaseModel):
    """一个加载好的 SKILL（包含 body）。"""

    model_config = ConfigDict(arbitrary_types_allowed=True)

    name: str
    description: str
    body: str
    source_path: Path

    @property
    def short_id(self) -> str:
        """用于日志的简短 id。"""
        return self.name.replace(" ", "_").lower()


class SkillSummary(BaseModel):
    """轻量 SKILL 摘要（不含 body），用于注入 system prompt。"""

    model_config = ConfigDict(arbitrary_types_allowed=True)

    name: str
    description: str
    source_path: Path


def parse_skill_file(path: Path) -> Skill | None:
    """解析单个 SKILL.md 文件。

    失败时返回 None（让上层决定是否报错）。
    """
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None

    m = _FRONTMATTER_RE.match(text)
    if not m:
        # 没有 frontmatter：跳过
        return None

    fm = _parse_simple_yaml(m.group("fm"))
    body = (m.group("body") or "").strip()

    name = fm.get("name") or path.parent.name
    description = fm.get("description") or ""

    if not name:
        return None

    return Skill(name=name, description=description, body=body, source_path=path)


def parse_skill_summary(path: Path) -> SkillSummary | None:
    """只解析 frontmatter，跳过 body（用于懒加载时减少 IO）。"""
    skill = parse_skill_file(path)
    if skill is None:
        return None
    return SkillSummary(
        name=skill.name,
        description=skill.description,
        source_path=skill.source_path,
    )


class SkillLoader:
    """扫描 .dscode/skills/ 和 ~/.dscode/skills/，解析 SKILL.md 格式。

    懒激活 API（推荐）::

        loader = SkillLoader(project_root=Path.cwd())
        summaries = loader.load_descriptions()
        block = loader.format_descriptions_block()       # 注入 system prompt
        body = loader.activate_skill("search-docs")     # 命中后取 body 单独注入

    旧 API（兼容保留）::

        skills = loader.discover()
        prompt_block = loader.format_for_system_prompt(skills)
        body = loader.get_body("search-docs")
    """

    SKILL_FILENAME = "SKILL.md"

    def __init__(
        self,
        project_root: Path,
        user_home: Path | None = None,
    ) -> None:
        self.project_root = project_root
        self.user_home = user_home or Path.home()

    # ------------------------------------------------------------
    # 发现
    # ------------------------------------------------------------

    def _search_roots(self) -> list[Path]:
        """返回扫描根目录列表（user 先，project 后；后者覆盖）。"""
        return [
            self.user_home / ".dscode" / "skills",
            self.project_root / ".dscode" / "skills",
        ]

    def _iter_skill_files(self, root: Path) -> list[Path]:
        if not root.exists() or not root.is_dir():
            return []
        # 支持两种布局：
        #   root/<name>/SKILL.md
        #   root/<name>.md（备选，直接当 SKILL）
        out: list[Path] = []
        for entry in sorted(root.iterdir()):
            if entry.is_dir():
                candidate = entry / self.SKILL_FILENAME
                if candidate.exists():
                    out.append(candidate)
            elif entry.is_file() and entry.suffix == ".md":
                out.append(entry)
        return out

    def discover(self) -> list[Skill]:
        """扫描所有源目录，返回去重（按 name）的 Skill 列表，项目级覆盖用户级。

        兼容 API：会同时读取 body，因此对大量 SKILL 不如 ``load_descriptions`` 高效。
        """
        bucket: dict[str, Skill] = {}
        for root in self._search_roots():
            for fp in self._iter_skill_files(root):
                skill = parse_skill_file(fp)
                if skill is None:
                    continue
                bucket[skill.name] = skill  # 后扫到的覆盖（项目级最后）
        return list(bucket.values())

    def load_descriptions(self) -> list[SkillSummary]:
        """只加载 frontmatter（name + description），不读 body。

        适合启动时一次性扫盘获得"可用技能清单"。
        """
        bucket: dict[str, SkillSummary] = {}
        for root in self._search_roots():
            for fp in self._iter_skill_files(root):
                summary = parse_skill_summary(fp)
                if summary is None:
                    continue
                bucket[summary.name] = summary
        return list(bucket.values())

    def load_body(self, name: str) -> str | None:
        """懒加载指定 SKILL 的 body。

        与 ``get_body`` 同名同语义，但作为"懒激活"API 的命名约定保留。
        """
        return self.get_body(name)

    # ------------------------------------------------------------
    # 注入系统提示
    # ------------------------------------------------------------

    def format_for_system_prompt(self, skills: list[Skill]) -> str:
        """把所有 skill 的 name + description 拼成一段，加在 system prompt 里。

        兼容 API：调用方需先 ``discover()``。
        触发条件由模型自己判断。Skill body 在被触发后用 ``get_body(name)`` 读取。
        """
        if not skills:
            return ""
        lines: list[str] = ["## 可用技能（SKILL）", ""]
        for s in skills:
            desc = s.description or "（无描述）"
            lines.append(f"- **{s.name}** — {desc}")
        lines.append("")
        lines.append(
            "若任务匹配上述技能描述，调用 `do_load_skill(name=...)` 取得完整 body 后再执行。"
        )
        return "\n".join(lines)

    def format_descriptions_block(
        self,
        summaries: list[SkillSummary] | None = None,
    ) -> str:
        """懒激活 API：把所有 SKILL 的描述拼成一段短文本注入 system prompt。

        与 ``format_for_system_prompt`` 输出兼容（都包含 name + description），
        但本方法不要求调用方先加载 body。

        若未传 ``summaries``，会自行调用 ``load_descriptions()``。
        """
        if summaries is None:
            summaries = self.load_descriptions()
        if not summaries:
            return ""
        lines: list[str] = ["## 可用技能（SKILL）", ""]
        for s in summaries:
            desc = s.description or "（无描述）"
            lines.append(f"- **{s.name}** — {desc}")
        lines.append("")
        lines.append(
            "若任务匹配上述技能描述，调用 `do_load_skill(name=...)` 取得完整 body 后再执行。"
        )
        return "\n".join(lines)

    def get_body(self, name: str) -> str | None:
        """按 name 重新扫描并返回 body（懒加载，避免一次性塞所有 body 到 prompt）。"""
        for root in self._search_roots():
            for fp in self._iter_skill_files(root):
                skill = parse_skill_file(fp)
                if skill is not None and skill.name == name:
                    return skill.body
        return None

    def activate_skill(self, name: str) -> str | None:
        """懒激活：返回指定 SKILL 的 body。

        ``get_body`` 的语义别名——调用方拿到 body 后自行决定怎么注入下一轮消息
        （通常作为 system 或 user 角色的额外 message）。
        """
        return self.get_body(name)


def serialize_skill(skill: Skill) -> dict[str, Any]:
    """方便日志/调试的序列化。"""
    return {
        "name": skill.name,
        "description": skill.description,
        "source_path": str(skill.source_path),
        "body_len": len(skill.body),
    }


__all__ = [
    "Skill",
    "SkillLoader",
    "SkillSummary",
    "parse_skill_file",
    "parse_skill_summary",
    "serialize_skill",
]
