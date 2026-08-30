from __future__ import annotations

import asyncio
import logging
import os

from pydantic import BaseModel, ConfigDict, Field

import merry

logger = logging.getLogger(__name__)


class LookupOrderInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier to look up.")


class LookupOrderOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: str = Field(description="Current fulfillment status for the order.")


def build_agent() -> merry.Agent:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL")
    if api_key is None or model is None:
        raise SystemExit(
            "Set MERRY_OPENAI_API_KEY and MERRY_OPENAI_MODEL before running."
        )

    builder = merry.AgentBuilder("python-tool-example").provider(
        merry.OpenAICompatible(
            api_key=api_key,
            model=model,
            base_url=os.environ.get("MERRY_OPENAI_BASE_URL"),
        )
    )

    @builder.tool
    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        """Look up an order by id."""
        return LookupOrderOutput(status="shipped")

    return builder.build()


async def main() -> None:
    result = await build_agent().run(
        "Use lookup_order for order A123, then report its fulfillment status."
    )
    logger.info("status=%s output=%s", result.status.value, result.final_output)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
