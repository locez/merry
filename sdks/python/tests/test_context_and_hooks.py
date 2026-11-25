"""Deterministic tests for context window configuration, interactive settings, and lifecycle hooks."""

from __future__ import annotations

import asyncio
from typing import Annotated

import pytest
from _support import context_and_hooks_agent
from pydantic import Field

import merry
from merry import (
    AfterModelStepContext,
    AfterToolCallContext,
    AfterToolDecision,
    Anthropic,
    BeforeModelDecision,
    BeforeModelStepContext,
    BeforeToolCallContext,
    BeforeToolDecision,
    CompactionConfig,
    Event,
    HookInvocation,
    HookPoint,
    Hooks,
    InteractiveSettingsUpdate,
    OpenAICompatible,
    RunStatus,
    StructuredOutputRetryPolicy,
    TextContent,
    ToolCallBatch,
    deny_tools_hook,
    events,
    max_token_budget_hook,
    require_condition_before_stop_hook,
)


def test_provider_and_builder_config_validation() -> None:
    with pytest.raises(ValueError, match="greater than zero"):
        OpenAICompatible(
            base_url="http://127.0.0.1:8080/v1",
            api_key="k",
            model="m",
            context_window_tokens=0,
        )

    with pytest.raises(ValueError, match="greater than zero"):
        Anthropic(
            api_key="k",
            model="claude-sonnet-4-6",
            context_window_tokens=-10,
        )

    with pytest.raises(ValueError, match="greater than zero"):
        InteractiveSettingsUpdate(context_window_tokens=0)

    hooks = Hooks()

    @hooks.before_tool_call(tools=["read_file"])
    def allow_read(ctx: BeforeToolCallContext) -> BeforeToolDecision:
        return BeforeToolDecision.allow()

    agent = (
        merry.Agent.builder("cfg-valid-session")
        .provider(
            OpenAICompatible(
                base_url="http://127.0.0.1:8080/v1",
                api_key="k",
                model="m",
                context_window_tokens=200_000,
            )
        )
        .context_window_tokens(128_000)
        .compaction(CompactionConfig.disabled())
        .structured_output_retry_policy(StructuredOutputRetryPolicy(max_retries=2))
        .hooks(hooks)
        .build()
    )
    assert agent.session_id == "cfg-valid-session"


def test_context_window_resolution_and_interactive_hot_update() -> None:
    async def scenario() -> None:
        # 1. Provider capability window (128k) replaces 272k fallback
        agent_provider = context_and_hooks_agent(
            "py-cw-provider",
            [{"kind": "text", "text": "hello"}],
            provider_context_window_tokens=128_000,
        )
        result = await agent_provider.run("hi")
        assert result.session_usage is not None
        assert result.session_usage.context is not None
        assert result.session_usage.context.resolved_model_window_tokens == 128_000
        assert result.session_usage.context.source == "provider_capabilities"

        # 2. Explicit override (64k) takes precedence over provider (128k)
        agent_explicit = context_and_hooks_agent(
            "py-cw-explicit",
            [{"kind": "text", "text": "hello"}],
            provider_context_window_tokens=128_000,
            explicit_context_window_tokens=64_000,
        )
        result_explicit = await agent_explicit.run("hi")
        assert result_explicit.session_usage is not None
        assert result_explicit.session_usage.context is not None
        assert (
            result_explicit.session_usage.context.resolved_model_window_tokens == 64_000
        )
        assert result_explicit.session_usage.context.source == "explicit_config"

        # 3. Interactive session hot-updates context window and clears override back to provider
        agent_interactive = context_and_hooks_agent(
            "py-cw-interactive",
            [
                {"kind": "text", "text": "turn 1"},
                {"kind": "text", "text": "turn 2"},
            ],
            provider_context_window_tokens=128_000,
        )
        observed_windows: list[tuple[int, str]] = []
        async with agent_interactive.interactive() as session:
            msg = await session.next()
            assert isinstance(msg, Event)

            await session.update_settings(
                InteractiveSettingsUpdate(context_window_tokens=96_000)
            )
            await session.send("first")
            while True:
                item = await session.next()
                assert isinstance(item, Event)
                if (
                    isinstance(item.payload, events.UsageUpdatedPayload)
                    and item.payload.usage.context is not None
                ):
                    observed_windows.append(
                        (
                            item.payload.usage.context.resolved_model_window_tokens,
                            item.payload.usage.context.source,
                        )
                    )
                if (
                    isinstance(item.payload, events.InteractiveRunStateChangedPayload)
                    and item.payload.state
                    is events.InteractiveRunState.WAITING_FOR_INPUT
                ):
                    break

            # Clear explicit override -> falls back to provider 128k on next turn
            await session.update_settings(context_window_tokens=None)
            await session.send("second")
            while True:
                item = await session.next()
                assert isinstance(item, Event)
                if (
                    isinstance(item.payload, events.UsageUpdatedPayload)
                    and item.payload.usage.context is not None
                ):
                    observed_windows.append(
                        (
                            item.payload.usage.context.resolved_model_window_tokens,
                            item.payload.usage.context.source,
                        )
                    )
                if (
                    isinstance(item.payload, events.InteractiveRunStateChangedPayload)
                    and item.payload.state
                    is events.InteractiveRunState.WAITING_FOR_INPUT
                ):
                    break

        assert (96_000, "explicit_config") in observed_windows
        assert (128_000, "provider_capabilities") in observed_windows

    asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))


def test_unregistered_and_filtered_hooks_emit_zero_unwanted_invocations() -> None:
    @merry.tool
    async def alpha() -> str:
        """Alpha test tool."""
        return "a"

    @merry.tool
    async def beta() -> str:
        """Beta test tool."""
        return "b"

    async def scenario() -> None:
        # 1. No hooks registered -> zero HookInvocation in manual mode
        agent_no_hooks = context_and_hooks_agent(
            "py-hooks-none",
            [
                {
                    "kind": "tool_calls",
                    "calls": [{"id": "c1", "name": "alpha", "arguments": {}}],
                },
                {"kind": "text", "text": "done"},
            ],
            tools=(alpha,),
            hooks=None,
        )
        hook_messages: list[HookInvocation] = []
        async with agent_no_hooks.stream("go", auto_hooks=False) as run:
            async for msg in run:
                if isinstance(msg, HookInvocation):
                    hook_messages.append(msg)
                elif isinstance(msg, ToolCallBatch):
                    await agent_no_hooks.execute_tool_batch(msg)
            result = await run.result()
        assert hook_messages == []
        assert result.status == RunStatus.COMPLETED

        # 2. Filtered hook ONLY on "alpha" -> "beta" never emits HookInvocation!
        hooks = Hooks()
        seen_in_manual: list[str] = []

        @hooks.before_tool_call(tools=["alpha"])
        def only_alpha(ctx: BeforeToolCallContext) -> BeforeToolDecision:
            return BeforeToolDecision.allow()

        agent_filtered = context_and_hooks_agent(
            "py-hooks-filtered",
            [
                {
                    "kind": "tool_calls",
                    "calls": [
                        {"id": "c1", "name": "beta", "arguments": {}},
                        {"id": "c2", "name": "alpha", "arguments": {}},
                    ],
                },
                {"kind": "text", "text": "done"},
            ],
            tools=(alpha, beta),
            hooks=hooks,
        )
        async with agent_filtered.stream(
            "go", auto_hooks=False, auto_tools=True
        ) as run:
            async for msg in run:
                if isinstance(msg, HookInvocation):
                    assert isinstance(msg.context, BeforeToolCallContext)
                    seen_in_manual.append(msg.context.tool_name)
                    await msg.submit(BeforeToolDecision.allow())
            result = await run.result()
        assert seen_in_manual == ["alpha"]
        assert result.status == RunStatus.COMPLETED

    asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))


def test_python_hooks_modify_deny_short_circuit_continue_and_stop_turn() -> None:
    executed: list[tuple[str, str]] = []

    @merry.tool
    async def work(item: Annotated[str, Field(description="Work item.")]) -> str:
        """Perform work on an item."""
        executed.append(("work", item))
        return f"raw:{item}"

    @merry.tool
    async def forbidden(
        item: Annotated[str, Field(description="Forbidden item.")],
    ) -> str:
        """Forbidden tool that must be denied by hook."""
        executed.append(("forbidden", item))
        return "should-not-run"

    @merry.tool
    async def cached(item: Annotated[str, Field(description="Cached item.")]) -> str:
        """Cached tool that will be short-circuited by hook."""
        executed.append(("cached", item))
        return "should-not-run"

    hooks = Hooks()
    after_seen_outputs: list[str] = []
    continue_triggered = False

    @hooks.before_tool_call
    async def intercept_tools(ctx: BeforeToolCallContext) -> BeforeToolDecision:
        if ctx.tool_name == "work":
            return BeforeToolDecision.modify_input({"item": "rewritten"})
        if ctx.tool_name == "cached":
            return BeforeToolDecision.short_circuit(TextContent("cached-hit"))
        return BeforeToolDecision.allow()

    hooks.before_tool_call(tools=["forbidden"])(deny_tools_hook(["forbidden"]))

    @hooks.after_tool_call(tools=["work"])
    async def redact_work(ctx: AfterToolCallContext) -> AfterToolDecision:
        assert isinstance(ctx.output, TextContent)
        after_seen_outputs.append(ctx.output.text)
        return AfterToolDecision.modify_output("sanitized:rewritten")

    def verify_gate(ctx: AfterModelStepContext) -> str | None:
        nonlocal continue_triggered
        if not continue_triggered:
            continue_triggered = True
            return "Please double check"
        return None

    hooks.after_model_step(require_condition_before_stop_hook(verify_gate))

    async def scenario() -> None:
        agent = context_and_hooks_agent(
            "py-hooks-full",
            [
                {
                    "kind": "tool_calls",
                    "calls": [
                        {"id": "c1", "name": "work", "arguments": {"item": "orig"}},
                        {"id": "c2", "name": "forbidden", "arguments": {"item": "x"}},
                        {"id": "c3", "name": "cached", "arguments": {"item": "y"}},
                    ],
                },
                {"kind": "text", "text": "first draft"},
                {"kind": "text", "text": "verified final"},
            ],
            tools=(work, forbidden, cached),
            hooks=hooks,
        )
        result = await agent.run("do everything")
        assert result.status == RunStatus.COMPLETED
        assert result.final_output == "verified final"
        assert result.model_turns_run == 3
        assert executed == [("work", "rewritten")]
        assert after_seen_outputs == ["raw:rewritten"]

    asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))


def test_hook_stop_turn_and_fail_closed_and_budget_guard() -> None:
    executed_count = 0

    @merry.tool
    async def act() -> str:
        """Perform an action."""
        nonlocal executed_count
        executed_count += 1
        return "ran"

    async def scenario() -> None:
        # 1. before_tool_call StopTurn stops cleanly without executing tools
        stop_hooks = Hooks()

        @stop_hooks.before_tool_call
        def stop_on_act(ctx: BeforeToolCallContext) -> BeforeToolDecision:
            return BeforeToolDecision.stop_turn(
                "manual review required", final_output="waiting on human"
            )

        agent_stop = context_and_hooks_agent(
            "py-hook-stop-tool",
            [
                {
                    "kind": "tool_calls",
                    "calls": [
                        {"id": "c1", "name": "act", "arguments": {}},
                        {"id": "c2", "name": "act", "arguments": {}},
                    ],
                }
            ],
            tools=(act,),
            hooks=stop_hooks,
        )
        res = await agent_stop.run("go")
        assert res.status == RunStatus.STOPPED_BY_HOOK
        assert res.hook_stop is not None
        assert res.hook_stop.hook_point == HookPoint.BEFORE_TOOL_CALL.value
        assert res.hook_stop.reason == "manual review required"
        assert res.final_output == "waiting on human"
        assert executed_count == 0

        # 2. Built-in max_token_budget_hook stops before turn 2 (turn 1 uses 100+25 = 125 tokens)
        budget_hooks = Hooks()
        budget_hooks.before_model_step(
            max_token_budget_hook(100, reason="budget reached")
        )
        agent_budget = context_and_hooks_agent(
            "py-hook-budget",
            [
                {
                    "kind": "tool_calls",
                    "calls": [{"id": "c1", "name": "act", "arguments": {}}],
                },
                {"kind": "text", "text": "never reached"},
            ],
            tools=(act,),
            hooks=budget_hooks,
        )
        budget_res = await agent_budget.run("spend tokens")
        assert budget_res.status == RunStatus.STOPPED_BY_HOOK
        assert budget_res.hook_stop is not None
        assert budget_res.hook_stop.hook_point == HookPoint.BEFORE_MODEL_STEP.value
        assert budget_res.hook_stop.reason == "budget reached"
        assert budget_res.model_turns_run == 1

        # 3. Fail-closed: Python exception in hook propagates cleanly and never silently allows
        boom_hooks = Hooks()

        @boom_hooks.before_model_step
        async def explode(ctx: BeforeModelStepContext) -> BeforeModelDecision:
            raise RuntimeError("hook policy service unreachable")

        agent_boom = context_and_hooks_agent(
            "py-hook-boom",
            [{"kind": "text", "text": "should not run"}],
            hooks=boom_hooks,
        )
        with pytest.raises(RuntimeError, match="hook policy service unreachable"):
            await agent_boom.run("hi")

    asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))


def test_interactive_stop_turn_returns_cleanly_to_idle_for_next_send() -> None:
    calls_seen = 0

    @merry.tool
    async def escalate() -> str:
        """Escalate a request to an operator."""
        return "escalated"

    hooks = Hooks()

    @hooks.before_tool_call(tools=["escalate"])
    def stop_first(ctx: BeforeToolCallContext) -> BeforeToolDecision:
        nonlocal calls_seen
        calls_seen += 1
        return BeforeToolDecision.stop_turn("escalated to operator")

    async def scenario() -> None:
        agent = context_and_hooks_agent(
            "py-interactive-stop-turn",
            [
                {
                    "kind": "tool_calls",
                    "calls": [{"id": "c1", "name": "escalate", "arguments": {}}],
                },
                {"kind": "text", "text": "recovered on next turn"},
            ],
            tools=(escalate,),
            hooks=hooks,
        )
        async with agent.interactive(auto_hooks=True, auto_tools=True) as session:
            first_state = await session.next()
            assert isinstance(first_state, Event)

            # Turn 1: model calls escalate -> hook StopTurn -> session returns to waiting_for_input
            await session.send("please escalate")
            while True:
                item = await session.next()
                assert isinstance(item, Event)
                if (
                    isinstance(item.payload, events.InteractiveRunStateChangedPayload)
                    and item.payload.state
                    is events.InteractiveRunState.WAITING_FOR_INPUT
                ):
                    break
            assert calls_seen == 1

            # Turn 2: session is still healthy and handles next user input cleanly
            await session.send("continue normally")
            seen_completed = False
            while True:
                item = await session.next()
                assert isinstance(item, Event)
                if isinstance(item.payload, events.StepCompletedPayload):
                    seen_completed = True
                if (
                    isinstance(item.payload, events.InteractiveRunStateChangedPayload)
                    and item.payload.state
                    is events.InteractiveRunState.WAITING_FOR_INPUT
                ):
                    break
            assert seen_completed

    asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))
