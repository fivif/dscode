"""文件写入安全卫士。

拒绝写入敏感系统路径以及 escape 工作目录的相对路径。
"""
from __future__ import annotations

import os
from pathlib import Path

from dscode.core.types import SafetyDecision

# 绝对禁写前缀（无论 unsafe 与否，按需扩展）
# 注意：macOS 上 /etc → /private/etc 等符号链接，两侧都列出
_FORBIDDEN_ABS_PREFIXES = (
    "/etc",
    "/private/etc",
    "/usr",
    "/System",
    "/bin",
    "/sbin",
    "/private/var/root",
    "/Library/System",
)

# 用户敏感目录（相对于 home）
_FORBIDDEN_HOME_PATHS = (
    ".ssh",
    ".aws",
    ".gnupg",
)


def _matches_forbidden_prefix(path_str: str) -> str | None:
    for prefix in _FORBIDDEN_ABS_PREFIXES:
        if path_str == prefix or path_str.startswith(prefix + "/"):
            return prefix
    return None


class FileGuard:
    """文件操作安全卫士。"""

    def __init__(self, cwd: str | None = None, unsafe: bool = False) -> None:
        self.cwd = Path(cwd or os.getcwd()).resolve()
        self.unsafe = unsafe

    def check_write(self, path: str) -> SafetyDecision:
        """校验是否允许写入 path。"""
        try:
            p = Path(path)
            # 拒绝 escape cwd 的相对路径
            if not p.is_absolute():
                resolved = (self.cwd / p).resolve()
                try:
                    resolved.relative_to(self.cwd)
                except ValueError:
                    return SafetyDecision(
                        allowed=False,
                        denied=True,
                        reason=f"path escapes working directory: {path}",
                    )
                abs_path = resolved
                # 原始 raw 绝对路径（解析前后都要查）
                raw_abs_str = str(abs_path)
            else:
                raw_abs_str = str(p)  # resolve 前
                abs_path = p.resolve()
        except (OSError, RuntimeError) as e:
            return SafetyDecision(
                allowed=False,
                denied=True,
                reason=f"invalid path: {e}",
            )

        abs_str = str(abs_path)

        # 系统目录：同时检查 raw 与 resolved，覆盖 symlink 情形
        for candidate in (raw_abs_str, abs_str):
            hit = _matches_forbidden_prefix(candidate)
            if hit and not self.unsafe:
                return SafetyDecision(
                    allowed=False,
                    denied=True,
                    reason=f"writes to system directory denied: {hit}",
                )

        # 用户敏感目录
        home = Path.home().resolve()
        for sub in _FORBIDDEN_HOME_PATHS:
            sensitive = home / sub
            try:
                abs_path.relative_to(sensitive)
                if not self.unsafe:
                    return SafetyDecision(
                        allowed=False,
                        denied=True,
                        reason=f"writes to sensitive home directory denied: ~/{sub}",
                    )
            except ValueError:
                continue

        # .dscode/spec/ 受保护
        try:
            rel = abs_path.relative_to(self.cwd)
            parts = rel.parts
            if (
                len(parts) >= 2
                and parts[0] == ".dscode"
                and parts[1] == "spec"
                and not self.unsafe
            ):
                return SafetyDecision(
                    allowed=False,
                    denied=True,
                    reason="writes to .dscode/spec/ are protected (use --unsafe to override)",
                )
        except ValueError:
            pass

        return SafetyDecision(allowed=True)
