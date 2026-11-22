from __future__ import annotations

import asyncio
import logging
import os

import merry
from merry import events

logger = logging.getLogger(__name__)


def build_agent() -> merry.Agent:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL")
    if api_key is None or model is None:
        raise SystemExit(
            "Set MERRY_OPENAI_API_KEY and MERRY_OPENAI_MODEL before running."
        )
    return merry.Agent(
        session_id="python-interactive-example",
        provider=merry.OpenAICompatible(
            api_key=api_key,
            model=model,
            base_url=os.environ.get("MERRY_OPENAI_BASE_URL"),
        ),
        system_prompt="You are a concise conversational assistant.",
    )


async def main() -> None:
    agent = build_agent()
    async with agent.interactive() as session:
        await session.send("Say hello in one sentence.")
        async for message in session:
            if isinstance(message, merry.ToolCallBatch):
                await agent.execute_tool_batch(message)
            elif isinstance(message.payload, events.AssistantMessagePayload):
                logger.info("assistant=%s", message.payload.text)
                break


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
