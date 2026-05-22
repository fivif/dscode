"""SpecLoader —— 加载项目规范 (.dscode/spec/) 并格式化为 prompt 注入文本。

设计要点：
- 首次使用时若 `.dscode/spec/` 不存在，会从内置 `dscode.templates`
  目录把默认模板（architecture.md / conventions.md / safety.md）整套复制过去。
- `load_all()` 返回 `{filename: content}`，便于上层精细引用单个文件。
- `format_for_prompt()` 拼成一段适合钉死在 cache prefix 的稳定文本，
  使 DeepSeek prompt caching 可以稳定命中。
"""
from __future__ import annotations

import shutil
from pathlib import Path
from typing import Final

import dscode.templates as _templates_pkg

SPEC_DIRNAME: Final[str] = ".dscode/spec"
_TEMPLATE_ROOT: Final[Path] = Path(_templates_pkg.__file__).resolve().parent


class SpecLoader:
    """加载 `.dscode/spec/` 下所有 markdown 文件作为项目规范。"""

    def __init__(self, project_root: Path | str) -> None:
        self.project_root = Path(project_root)
        self.spec_dir = self.project_root / SPEC_DIRNAME
        self._cache: dict[str, str] | None = None

    # ------------------------------------------------------------
    # 公共 API
    # ------------------------------------------------------------

    def load_all(self) -> dict[str, str]:
        """读取所有 spec markdown 文件。

        若目录不存在，自动从 `src/dscode/templates/` 复制默认模板（architecture /
        conventions / safety），保证 Plan 流程在新项目里也能跑。

        Returns:
            `{filename: content}` —— 文件名带后缀，按字典序排序遍历。
        """
        if not self.spec_dir.exists():
            self._bootstrap_from_templates()

        files: dict[str, str] = {}
        for md_path in sorted(self.spec_dir.glob("*.md")):
            try:
                files[md_path.name] = md_path.read_text(encoding="utf-8")
            except OSError:
                # 读不到的单个文件不能影响整体加载
                continue
        self._cache = files
        return files

    def format_for_prompt(self) -> str:
        """把全部 spec 文件拼成一段带文件头分隔的文本，注入 system prompt。

        每个文件以 `## spec: <filename>` 开头，便于模型识别来源。
        若没有 spec 文件，返回空字符串（调用方应跳过对应 cache block）。
        """
        files = self._cache if self._cache is not None else self.load_all()
        if not files:
            return ""
        parts: list[str] = ["# 项目规范（来自 .dscode/spec/）"]
        for name, body in files.items():
            parts.append(f"\n## spec: {name}\n")
            parts.append(body.rstrip())
        return "\n".join(parts).rstrip() + "\n"

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    def _bootstrap_from_templates(self) -> None:
        """从内置模板目录复制一份默认 spec 到项目根。"""
        self.spec_dir.mkdir(parents=True, exist_ok=True)
        for tpl in sorted(_TEMPLATE_ROOT.glob("*.md")):
            target = self.spec_dir / tpl.name
            if target.exists():
                continue
            try:
                shutil.copyfile(tpl, target)
            except OSError:
                continue


__all__ = ["SPEC_DIRNAME", "SpecLoader"]
