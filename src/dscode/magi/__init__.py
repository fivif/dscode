"""MAGI 三脑公开 API。

三段编码范式的第二段：螺旋上升的 Scrutinize → Execute → Promote 循环。

典型用法：
    scheduler = MAGIScheduler(
        scrutinizer=Scrutinizer(llm),
        executor=Executor(forge),
        promoter=Promoter(llm),
        scribe=scribe,
        side_git_handler=snapshot_handler,  # 可选
    )
    history = await scheduler.run(prd, session_id="sess-xxx", max_rounds=10)

子模块：
- Scrutinizer    —— Casper，审视脑
- Executor       —— Balthasar，执行脑（薄包 Forge）
- Promoter       —— Melchior，提升脑（含停止判断）
- MAGIScheduler  —— 主循环 orchestrator
"""
from __future__ import annotations

from dscode.magi.execute import Executor
from dscode.magi.promote import Promoter
from dscode.magi.scheduler import MAGIScheduler
from dscode.magi.scrutinize import Scrutinizer

__all__ = [
    "Executor",
    "MAGIScheduler",
    "Promoter",
    "Scrutinizer",
]
