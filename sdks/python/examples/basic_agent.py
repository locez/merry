from __future__ import annotations

import asyncio
import logging
import os

import merry

logger = logging.getLogger(__name__)


def provider_from_env() -> merry.OpenAICompatible:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL")
    if api_key is None:
        raise SystemExit("Set MERRY_OPENAI_API_KEY before running this example.")
    if model is None:
        raise SystemExit("Set MERRY_OPENAI_MODEL before running this example.")
    return merry.OpenAICompatible(
        api_key=api_key,
        model=model,
        base_url=os.environ.get("MERRY_OPENAI_BASE_URL"),
    )


def build_agent() -> merry.Agent:
    return (
        merry.AgentBuilder("python-basic-example").provider(provider_from_env()).build()
    )


def handle_event(event: merry.Event) -> None:
    if isinstance(event.payload, merry.AssistantMessagePayload):
        logger.info("assistant_message=%s", event.payload.text)
        return
    if event.type in {
        merry.EventType.TOOL_CALL_STARTED,
        merry.EventType.TOOL_CALL_FINISHED,
    }:
        logger.info("tool_event=%s", event.type.value)
        return
    logger.info("runtime_event=%s", event.type.value)


async def main() -> None:
    run = build_agent().stream(
        "Reply in one short sentence confirming the Merry Python SDK is connected."
    )
    try:
        async for message in run:
            if isinstance(message, merry.Event):
                handle_event(message)
            else:
                await run.cancel()
                raise TypeError(
                    "The basic example received a tool call without a registered host tool."
                )
        result = await run.result()
    finally:
        await run.close()
    logger.info(
        "status=%s turns=%d output=%s",
        result.status.value,
        result.model_turns_run,
        result.final_output,
    )


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
