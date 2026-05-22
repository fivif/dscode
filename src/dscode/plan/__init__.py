"""Plan 阶段公开 API。

三段编码范式的第一段：把模糊需求 → 结构化 PRD + 上下文清单。

典型用法：
    runner = PlanRunner(llm=client, project_root=Path("."))
    task_dir = await runner.run("重构 user_service.py")
    # → .dscode/tasks/<task_id>/{prd.md, prd.json, implement.jsonl, check.jsonl}

子模块：
- SpecLoader     —— 项目规范加载（.dscode/spec/）
- GrillMe        —— grill-me 风格深度访谈
- PRDGenerator   —— PRD + ContextManifest 生成
- PlanRunner     —— 一站式 orchestrator
- load_prd       —— 从落盘的 task 目录或 prd.json/prd.md 还原 PRDDocument
"""
from __future__ import annotations

import json
from pathlib import Path

from dscode.core.types import ContextManifest, LLMProviderProtocol, PRDDocument
from dscode.plan.grill_me import AskUserFn, GrillMe
from dscode.plan.prd_generator import PRDGenerator
from dscode.plan.spec_loader import SPEC_DIRNAME, SpecLoader


def load_prd(path: Path | str) -> PRDDocument:
    """从落盘 PRD 还原 PRDDocument。

    支持三种 path：
    - 直接指向 prd.json
    - 直接指向 prd.md（自动改读同目录 prd.json）
    - 指向 task 目录（自动找 prd.json）

    PRDGenerator.write_to_disk 总是同时写 prd.md + prd.json，
    机器读取优先用 prd.json。
    """
    p = Path(path)
    if p.is_dir():
        json_path = p / "prd.json"
    elif p.suffix == ".json":
        json_path = p
    elif p.suffix == ".md":
        json_path = p.with_name("prd.json")
    else:
        raise ValueError(f"无法识别的 PRD 路径: {p!r}")
    if not json_path.exists():
        raise FileNotFoundError(
            f"找不到 prd.json: {json_path}（请先跑 dscode plan 生成 PRD）"
        )
    data = json.loads(json_path.read_text(encoding="utf-8"))
    return PRDDocument.model_validate(data)


class PlanRunner:
    """SpecLoader + GrillMe + PRDGenerator 一站式 orchestrator。

    用法：
        runner = PlanRunner(llm=DeepSeekClient(), project_root=Path("."))
        task_dir = await runner.run("重构 user_service.py", ask_user=cb)
    """

    def __init__(
        self,
        llm: LLMProviderProtocol,
        project_root: Path | str,
        *,
        grill_model: str = "deepseek-v4-flash",
        prd_model: str = "deepseek-v4-flash",
        max_grill_rounds: int = 10,
    ) -> None:
        self.llm = llm
        self.project_root = Path(project_root)
        self.spec_loader = SpecLoader(self.project_root)
        self.grill = GrillMe(llm=llm, model=grill_model)
        self.prd_gen = PRDGenerator(llm=llm, model=prd_model)
        self.max_grill_rounds = max_grill_rounds

    async def run(
        self,
        task_description: str,
        ask_user: AskUserFn | None = None,
        related_files: list[str] | tuple[str, ...] = (),
    ) -> Path:
        """跑完整 Plan 流程，返回 task 目录路径。"""
        prd, impl, chk = await self.plan(
            task_description=task_description,
            ask_user=ask_user,
            related_files=related_files,
        )
        return self.prd_gen.write_to_disk(prd, impl, chk, self.project_root)

    async def plan(
        self,
        task_description: str,
        ask_user: AskUserFn | None = None,
        related_files: list[str] | tuple[str, ...] = (),
    ) -> tuple[PRDDocument, ContextManifest, ContextManifest]:
        """同 `run` 但不落盘，返回内存对象。"""
        # 1) 加载并格式化项目规范
        self.spec_loader.load_all()
        spec_text = self.spec_loader.format_for_prompt()

        # 2) grill-me 访谈
        qa = await self.grill.interview(
            task_description=task_description,
            spec_text=spec_text,
            max_rounds=self.max_grill_rounds,
            ask_user=ask_user,
        )

        # 3) 生成 PRD + 两份 manifest
        prd, impl, chk = await self.prd_gen.generate(
            task_description=task_description,
            interview_qa=qa,
            spec_text=spec_text,
            related_files=related_files,
        )
        return prd, impl, chk


__all__ = [
    "SPEC_DIRNAME",
    "AskUserFn",
    "GrillMe",
    "PRDGenerator",
    "PlanRunner",
    "SpecLoader",
    "load_prd",
]
