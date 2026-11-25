"""Public lifecycle hook types and built-in strategy helpers for Merry."""

from ._hook_strategies import (
    deny_tools_hook,
    max_token_budget_hook,
    require_condition_before_stop_hook,
)
from ._hooks import (
    AfterModelCallback,
    AfterModelDecision,
    AfterModelStepContext,
    AfterToolCallback,
    AfterToolCallContext,
    AfterToolDecision,
    BeforeModelCallback,
    BeforeModelDecision,
    BeforeModelStepContext,
    BeforeToolCallback,
    BeforeToolCallContext,
    BeforeToolDecision,
    HookContext,
    HookDecision,
    HookInvocation,
    HookPoint,
    Hooks,
)

__all__ = [
    "AfterModelCallback",
    "AfterModelDecision",
    "AfterModelStepContext",
    "AfterToolCallContext",
    "AfterToolCallback",
    "AfterToolDecision",
    "BeforeModelCallback",
    "BeforeModelDecision",
    "BeforeModelStepContext",
    "BeforeToolCallContext",
    "BeforeToolCallback",
    "BeforeToolDecision",
    "HookContext",
    "HookDecision",
    "HookInvocation",
    "HookPoint",
    "Hooks",
    "deny_tools_hook",
    "max_token_budget_hook",
    "require_condition_before_stop_hook",
]
