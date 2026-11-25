"""Built-in reusable hook strategy factories for the Python SDK."""

from __future__ import annotations

import inspect
from collections.abc import Awaitable, Callable, Sequence

from ._hooks import (
    AfterModelCallback,
    AfterModelDecision,
    AfterModelStepContext,
    BeforeModelCallback,
    BeforeModelDecision,
    BeforeModelStepContext,
    BeforeToolCallback,
    BeforeToolCallContext,
    BeforeToolDecision,
    _normalize_tool_filter,
    _require_nonblank,
)


def max_token_budget_hook(
    max_total_tokens: int,
    *,
    reason: str = "token budget exceeded",
) -> BeforeModelCallback:
    """Built-in strategy hook that stops the turn before the next step once total tokens hit `max_total_tokens`."""

    if type(max_total_tokens) is not int or max_total_tokens < 1:
        raise ValueError("max_total_tokens must be a positive integer")
    _require_nonblank("reason", reason)

    def _guard(ctx: BeforeModelStepContext) -> BeforeModelDecision:
        if (
            ctx.session_usage is not None
            and ctx.session_usage.total.total_tokens >= max_total_tokens
        ):
            return BeforeModelDecision.stop_turn(reason)
        return BeforeModelDecision.continue_normally()

    return _guard


def deny_tools_hook(
    tools: Sequence[str],
    *,
    message: str | None = None,
) -> BeforeToolCallback:
    """Built-in strategy hook that denies calls to any tool in `tools`."""

    denied = _normalize_tool_filter(tools)
    if denied is None or not denied:
        raise ValueError("deny_tools_hook requires at least one tool name")

    def _guard(ctx: BeforeToolCallContext) -> BeforeToolDecision:
        if ctx.tool_name in denied:
            return BeforeToolDecision.deny(
                message or f"Tool {ctx.tool_name!r} is denied by policy."
            )
        return BeforeToolDecision.allow()

    return _guard


def require_condition_before_stop_hook(
    check: Callable[[AfterModelStepContext], str | None | Awaitable[str | None]],
) -> AfterModelCallback:
    """Built-in strategy hook that forces continuation when a final turn fails `check`.

    `check(ctx)` returns `None` to allow the turn to complete, or a non-blank
    feedback string to continue the turn with that feedback.
    """

    async def _gate(ctx: AfterModelStepContext) -> AfterModelDecision:
        if not ctx.is_final_turn:
            return AfterModelDecision.accept()
        raw = check(ctx)
        feedback = await raw if inspect.isawaitable(raw) else raw
        if feedback is None:
            return AfterModelDecision.accept()
        return AfterModelDecision.continue_with(feedback)

    return _gate
