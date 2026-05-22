"""全局配置加载。

来源（优先级从高到低）：
1. 环境变量
2. 项目级 `.dscode/config.toml`
3. 全局 `~/.dscode/config.toml`
4. 内置默认值

设计要点：
- 用 Pydantic v2 BaseModel 兜底校验。
- TOML 解析用 Python 3.11+ 标准库 `tomllib`。
- 字段命名与环境变量映射：`deepseek_api_key` ↔ `DEEPSEEK_API_KEY`，
  其它字段统一前缀 `DSCODE_`（例如 `DSCODE_DEFAULT_MODEL`）。
- TOML schema 用嵌套表（[model] [magi] [safety] ...），见 README/init 模板。
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import Any

try:
    import tomllib  # type: ignore[import-not-found]
except ImportError:  # pragma: no cover
    import tomli as tomllib  # type: ignore[no-redef]

from pydantic import BaseModel, ConfigDict, Field


def _coerce_bool(val: Any) -> bool:
    """字符串/数字/布尔统一转 bool。"""
    if isinstance(val, bool):
        return val
    if isinstance(val, (int, float)):
        return bool(val)
    if isinstance(val, str):
        return val.strip().lower() in {"1", "true", "yes", "on", "y"}
    return False


def _coerce_int(val: Any, default: int) -> int:
    try:
        return int(val)
    except (TypeError, ValueError):
        return default


def _coerce_float(val: Any, default: float) -> float:
    try:
        return float(val)
    except (TypeError, ValueError):
        return default


def _read_toml(path: Path) -> dict[str, Any]:
    """读取 TOML 文件，失败/不存在时返回空字典。"""
    if not path.exists():
        return {}
    try:
        with path.open("rb") as f:
            return tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return {}


def _flatten_toml(data: dict[str, Any]) -> dict[str, Any]:
    """把 [model] [magi] [safety] [deepseek] 等嵌套表展平到字段名。

    映射：
      [model] default → default_model
      [model] router → default_router_model
      [model] executor → default_executor_model
      [magi] max_rounds → max_magi_rounds
      [magi] max_steps → max_steps
      [safety] unsafe → safety_unsafe_mode
      [deepseek] api_key → deepseek_api_key
      [deepseek] base_url → deepseek_base_url
      [telemetry] cache_enabled → cache_telemetry_enabled
      [paths] db_path → db_path
    顶层键直接保留（向后兼容）。
    """
    out: dict[str, Any] = {}

    model = data.get("model") or {}
    if isinstance(model, dict):
        if "default" in model:
            out["default_model"] = model["default"]
        if "router" in model:
            out["default_router_model"] = model["router"]
        if "executor" in model:
            out["default_executor_model"] = model["executor"]

    magi = data.get("magi") or {}
    if isinstance(magi, dict):
        if "max_rounds" in magi:
            out["max_magi_rounds"] = magi["max_rounds"]
        if "max_steps" in magi:
            out["max_steps"] = magi["max_steps"]

    safety = data.get("safety") or {}
    if isinstance(safety, dict):
        if "unsafe" in safety:
            out["safety_unsafe_mode"] = safety["unsafe"]

    ds = data.get("deepseek") or {}
    if isinstance(ds, dict):
        if "api_key" in ds:
            out["deepseek_api_key"] = ds["api_key"]
        if "base_url" in ds:
            out["deepseek_base_url"] = ds["base_url"]

    tel = data.get("telemetry") or {}
    if isinstance(tel, dict):
        if "cache_enabled" in tel:
            out["cache_telemetry_enabled"] = tel["cache_enabled"]

    paths = data.get("paths") or {}
    if isinstance(paths, dict):
        if "db_path" in paths:
            out["db_path"] = paths["db_path"]

    # 顶层键透传（覆盖嵌套）
    for k, v in data.items():
        if not isinstance(v, dict):
            out[k] = v

    return out


class Config(BaseModel):
    """全局配置。来源：env vars + ~/.dscode/config.toml + .dscode/config.toml（项目级）。"""

    model_config = ConfigDict(arbitrary_types_allowed=True)

    deepseek_api_key: str | None = None
    deepseek_base_url: str = "https://api.deepseek.com"
    default_model: str = "deepseek-v4-flash"
    default_router_model: str = "deepseek-v4-flash"
    default_executor_model: str = "deepseek-v4-pro"
    max_steps: int = 40
    max_magi_rounds: int = 20
    cache_telemetry_enabled: bool = True
    project_root: Path = Field(default_factory=Path.cwd)
    db_path: Path | None = None
    safety_unsafe_mode: bool = False

    # ------------------------------------------------------------
    # 工厂
    # ------------------------------------------------------------

    @classmethod
    def load(cls, project_root: Path | None = None) -> Config:
        """优先级：env > project .dscode/config.toml > global ~/.dscode/config.toml > 默认。"""
        root = (project_root or Path.cwd()).resolve()

        global_cfg = _flatten_toml(_read_toml(Path.home() / ".dscode" / "config.toml"))
        project_cfg = _flatten_toml(_read_toml(root / ".dscode" / "config.toml"))

        merged: dict[str, Any] = {}
        merged.update(global_cfg)
        merged.update(project_cfg)

        # 环境变量覆盖（最高优先级）
        env_map = {
            "DEEPSEEK_API_KEY": "deepseek_api_key",
            "DEEPSEEK_BASE_URL": "deepseek_base_url",
            "DSCODE_DEFAULT_MODEL": "default_model",
            "DSCODE_ROUTER_MODEL": "default_router_model",
            "DSCODE_EXECUTOR_MODEL": "default_executor_model",
            "DSCODE_MAX_STEPS": "max_steps",
            "DSCODE_MAX_MAGI_ROUNDS": "max_magi_rounds",
            "DSCODE_CACHE_TELEMETRY": "cache_telemetry_enabled",
            "DSCODE_DB_PATH": "db_path",
            "DSCODE_UNSAFE": "safety_unsafe_mode",
        }
        for env_name, field_name in env_map.items():
            val = os.getenv(env_name)
            if val is not None and val != "":
                merged[field_name] = val

        # 类型强制
        if "max_steps" in merged:
            merged["max_steps"] = _coerce_int(merged["max_steps"], 40)
        if "max_magi_rounds" in merged:
            merged["max_magi_rounds"] = _coerce_int(merged["max_magi_rounds"], 20)
        if "cache_telemetry_enabled" in merged:
            merged["cache_telemetry_enabled"] = _coerce_bool(merged["cache_telemetry_enabled"])
        if "safety_unsafe_mode" in merged:
            merged["safety_unsafe_mode"] = _coerce_bool(merged["safety_unsafe_mode"])
        if merged.get("db_path"):
            merged["db_path"] = Path(merged["db_path"]).expanduser()

        merged["project_root"] = root

        # 过滤掉未知字段，避免 Pydantic 报错
        allowed = set(cls.model_fields.keys())
        merged = {k: v for k, v in merged.items() if k in allowed}

        return cls(**merged)

    # ------------------------------------------------------------
    # 派生路径
    # ------------------------------------------------------------

    @property
    def dscode_dir(self) -> Path:
        """`.dscode/` 项目元数据目录。"""
        return self.project_root / ".dscode"

    @property
    def effective_db_path(self) -> Path:
        """实际使用的 SQLite 路径。"""
        if self.db_path is not None:
            return self.db_path
        return self.dscode_dir / "memory" / "state.db"

    @property
    def telemetry_path(self) -> Path:
        """缓存遥测持久化路径。"""
        return self.dscode_dir / "telemetry.json"

    @property
    def tasks_dir(self) -> Path:
        return self.dscode_dir / "tasks"

    @property
    def spec_dir(self) -> Path:
        return self.dscode_dir / "spec"

    # 估算时长保留接口（v1 简化）
    def estimate_default_hours(self) -> float:
        """默认任务时长估算（小时）。v1 简化为常量。"""
        return _coerce_float(self.max_magi_rounds * 0.1, 2.0)


__all__ = ["Config"]
