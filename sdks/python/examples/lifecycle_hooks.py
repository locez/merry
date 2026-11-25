"""Example demonstrating context window configuration and lifecycle hooks."""

from __future__ import annotations

import asyncio
import logging
import os
from typing import Annotated

from pydantic import Field

import merry
from merry import (
    AfterModelDecision,
    AfterModelStepContext,
    AfterToolCallContext,
    AfterToolDecision,
    BeforeToolCallContext,
    BeforeToolDecision,
    CompactionConfig,
    Event,
    Hooks,
    RunStatus,
    TextContent,
    events,
    max_token_budget_hook,
)

logger = logging.getLogger(__name__)


@merry.tool(parallel_safe=True)
async def lookup_doc(
    doc_id: Annotated[str, Field(description="Document identifier to fetch.")],
) -> str:
    """Fetch a document by identifier."""
    return f"doc({doc_id}): status=ok token=SECRET_VALUE"


def build_hooks() -> Hooks:
    hooks = Hooks()

    # Stop cleanly before the next model turn once cumulative tokens reach 100k.
    hooks.before_model_step(max_token_budget_hook(100_000, reason="token budget hit"))

    # Intercept only `lookup_doc`; other tools incur zero hook bridge traffic.
    @hooks.before_tool_call(tools=["lookup_doc"])
    async def guard_lookup(ctx: BeforeToolCallContext) -> BeforeToolDecision:
        doc_id = ctx.arguments.get("doc_id")
        if doc_id == "cached":
            return BeforeToolDecision.short_circuit(TextContent("doc(cached): ok"))
        if doc_id == "restricted":
            return BeforeToolDecision.deny("Access to restricted documents is denied.")
        return BeforeToolDecision.allow()

    # Redact sensitive values before the tool result is committed to the journal.
    @hooks.after_tool_call(tools=["lookup_doc"])
    def redact_doc(ctx: AfterToolCallContext) -> AfterToolDecision:
        if isinstance(ctx.output, TextContent) and "SECRET_VALUE" in ctx.output.text:
            return AfterToolDecision.modify_output(
                ctx.output.text.replace("SECRET_VALUE", "[REDACTED]")
            )
        return AfterToolDecision.continue_normally()

    # Require a concise final answer marker before allowing turn completion.
    @hooks.after_model_step
    def check_final(ctx: AfterModelStepContext) -> AfterModelDecision:
        if ctx.is_final_turn and ctx.model_turn == 1 and not ctx.assistant_text:
            return AfterModelDecision.continue_with("Provide a brief summary.")
        return AfterModelDecision.accept()

    return hooks


def build_agent() -> merry.Agent:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL")
    if api_key is None or model is None:
        raise SystemExit(
            "Set MERRY_OPENAI_API_KEY and MERRY_OPENAI_MODEL before running."
        )

    return merry.Agent(
        session_id="python-hooks-example",
        provider=merry.OpenAICompatible(
            api_key=api_key,
            model=model,
            base_url=os.environ.get("MERRY_OPENAI_BASE_URL"),
            context_window_tokens=128_000,
        ),
        compaction=CompactionConfig(),
        tools=[lookup_doc],
        hooks=build_hooks(),
    )


async def main() -> None:
    agent = build_agent()
    async with agent.stream(
        "Use lookup_doc for doc_id='alpha' and summarize the result.",
        auto_tools=True,
    ) as run:
        async for message in run:
            if isinstance(message, Event) and isinstance(
                message.payload, events.AssistantMessageDeltaPayload
            ):
                print(message.payload.delta, end="", flush=True)
        result = await run.result()

    print()
    if result.status is RunStatus.STOPPED_BY_HOOK and result.hook_stop is not None:
        logger.info(
            "stopped_by_hook point=%s reason=%s",
            result.hook_stop.hook_point,
            result.hook_stop.reason,
        )
    else:
        logger.info("status=%s turns=%d", result.status.value, result.model_turns_run)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
