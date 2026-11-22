"""Async-first Python SDK for the Rust-owned Merry agent facade."""

from . import _merry as _merry
from . import config as config
from . import errors as errors
from . import events as events
from . import tools as tools
from ._agent import Agent
from ._builder import AgentBuilder
from ._config import (
    Anthropic,
    GenerationConfig,
    OpenAICompatible,
    ParallelToolCalls,
    PatchConfig,
    PromptBlock,
    PromptProfile,
    Provider,
    ServiceTier,
    WorkspaceConfig,
    WorkspaceLimits,
)
from ._errors import (
    MerryCompactionError,
    MerryConfigError,
    MerryContextError,
    MerryError,
    MerryErrorInfo,
    MerryInternalError,
    MerryOutputError,
    MerryPolicyError,
    MerryProviderError,
    MerryRuntimeError,
    MerryToolError,
    ToolDomainError,
)
from ._events import Event, EventType
from ._interactive import InputQueue, InteractiveInputItem, InteractiveSession
from ._models import (
    BlockedReason,
    CompactionUsageWindow,
    FinalOutputRecord,
    JsonContent,
    ModelUsage,
    RunResult,
    RunStatus,
    SessionUsage,
    TextContent,
    ToolCall,
    ToolCallBatch,
    ToolDiagnostic,
    ToolResult,
    ToolSubmission,
    UsageContextWindow,
)
from ._run import AgentRun
from ._tools import Tool, ToolHandler, ToolRegistry, tool

__version__ = _merry.version()

__all__ = [
    "Agent",
    "AgentBuilder",
    "AgentRun",
    "Anthropic",
    "BlockedReason",
    "CompactionUsageWindow",
    "Event",
    "EventType",
    "FinalOutputRecord",
    "GenerationConfig",
    "InputQueue",
    "InteractiveInputItem",
    "InteractiveSession",
    "JsonContent",
    "MerryCompactionError",
    "MerryConfigError",
    "MerryContextError",
    "MerryError",
    "MerryErrorInfo",
    "MerryInternalError",
    "MerryOutputError",
    "MerryPolicyError",
    "MerryProviderError",
    "MerryRuntimeError",
    "MerryToolError",
    "ModelUsage",
    "OpenAICompatible",
    "ParallelToolCalls",
    "PatchConfig",
    "PromptBlock",
    "PromptProfile",
    "Provider",
    "RunResult",
    "RunStatus",
    "ServiceTier",
    "SessionUsage",
    "TextContent",
    "Tool",
    "ToolCall",
    "ToolCallBatch",
    "ToolDiagnostic",
    "ToolDomainError",
    "ToolHandler",
    "ToolRegistry",
    "ToolResult",
    "ToolSubmission",
    "UsageContextWindow",
    "WorkspaceConfig",
    "WorkspaceLimits",
    "__version__",
    "config",
    "errors",
    "events",
    "tool",
    "tools",
]
