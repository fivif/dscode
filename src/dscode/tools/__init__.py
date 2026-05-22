"""Tools 公开 API + 默认注册函数。"""
from __future__ import annotations

from dscode.tools.registry import ToolRegistry


def build_default_registry() -> ToolRegistry:
    """构建包含所有内置工具的默认注册中心。"""
    reg = ToolRegistry()

    from dscode.tools.bash import SPEC as BASH_SPEC
    from dscode.tools.bash import handler as bash_handler
    from dscode.tools.file_ops import (
        PATCH_SPEC,
        READ_SPEC,
        WRITE_SPEC,
        patch_handler,
        read_handler,
        write_handler,
    )
    from dscode.tools.git_ops import SPEC as GIT_SPEC
    from dscode.tools.git_ops import handler as git_handler
    from dscode.tools.grep import SPEC as GREP_SPEC
    from dscode.tools.grep import handler as grep_handler
    from dscode.tools.lsp_query import SPEC as LSP_SPEC
    from dscode.tools.lsp_query import handler as lsp_handler
    from dscode.tools.side_git import (
        ROLLBACK_SPEC,
        SNAPSHOT_SPEC,
        rollback_handler,
        snapshot_handler,
    )
    from dscode.tools.test_runner import SPEC as TEST_SPEC
    from dscode.tools.test_runner import handler as test_handler

    reg.register(GREP_SPEC, grep_handler)
    reg.register(READ_SPEC, read_handler)
    reg.register(WRITE_SPEC, write_handler)
    reg.register(PATCH_SPEC, patch_handler)
    reg.register(BASH_SPEC, bash_handler)
    reg.register(TEST_SPEC, test_handler)
    reg.register(GIT_SPEC, git_handler)
    reg.register(SNAPSHOT_SPEC, snapshot_handler)
    reg.register(ROLLBACK_SPEC, rollback_handler)
    reg.register(LSP_SPEC, lsp_handler)

    return reg


__all__ = ["ToolRegistry", "build_default_registry"]
