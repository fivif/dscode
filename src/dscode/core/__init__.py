"""DS Code 核心引擎。

- types: 核心数据类和接口契约（**所有模块的共享基础**）
- scribe: 三级记忆引擎（L1 raw / L2 facts / L3 patterns）
- forge: ReAct 流式执行引擎
- anvil: 异步反思引擎（v1 占位）
"""

from dscode.core.anvil import Anvil, CompressionReport, Contradiction, ReflectionReport
from dscode.core.forge import Forge
from dscode.core.scribe import Scribe
from dscode.core.types import (
    ContextPacket,
    ExecutionResult,
    Fact,
    FactWriteResult,
    ForgeProtocol,
    LLMProviderProtocol,
    LLMResponse,
    MAGIPhase,
    MAGIRoundLog,
    Message,
    Pattern,
    PatternWriteResult,
    PRDDocument,
    PromoteOutput,
    RawEvent,
    SafetyDecision,
    ScribeProtocol,
    ScrutinizeOutput,
    StreamEvent,
    StreamEventType,
    ToolCallSpec,
    ToolFunctionSpec,
    ToolHandler,
    ToolRegistryProtocol,
    ToolResult,
    ToolSpec,
    ToolStatus,
    Usage,
)

__all__ = [
    "Anvil",
    "CompressionReport",
    "ContextPacket",
    "Contradiction",
    "ExecutionResult",
    "Fact",
    "FactWriteResult",
    "Forge",
    "ForgeProtocol",
    "LLMProviderProtocol",
    "LLMResponse",
    "MAGIPhase",
    "MAGIRoundLog",
    "Message",
    "PRDDocument",
    "Pattern",
    "PatternWriteResult",
    "PromoteOutput",
    "RawEvent",
    "ReflectionReport",
    "SafetyDecision",
    "Scribe",
    "ScribeProtocol",
    "ScrutinizeOutput",
    "StreamEvent",
    "StreamEventType",
    "ToolCallSpec",
    "ToolFunctionSpec",
    "ToolHandler",
    "ToolRegistryProtocol",
    "ToolResult",
    "ToolSpec",
    "ToolStatus",
    "Usage",
]
