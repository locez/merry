from __future__ import annotations

import asyncio
import logging
import os
from typing import Annotated

from pydantic import BaseModel, Field

import merry

logger = logging.getLogger(__name__)


class LookupOrderOutput(BaseModel):
    order_id: str
    status: str


@merry.tool(parallel_safe=True)
async def lookup_order(
    order_id: Annotated[str, Field(description="Stable order identifier to look up.")],
) -> LookupOrderOutput:
    """Look up an order by id."""
    return LookupOrderOutput(order_id=order_id, status="shipped")


def build_agent() -> merry.Agent:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL")
    if api_key is None or model is None:
        raise SystemExit(
            "Set MERRY_OPENAI_API_KEY and MERRY_OPENAI_MODEL before running."
        )

    return merry.Agent(
        session_id="python-tool-example",
        provider=merry.OpenAICompatible(
            api_key=api_key,
            model=model,
            base_url=os.environ.get("MERRY_OPENAI_BASE_URL"),
        ),
        tools=[lookup_order],
    )


async def main() -> None:
    result = await build_agent().run(
        "Use lookup_order for order A123, then report its fulfillment status."
    )
    logger.info("status=%s output=%s", result.status.value, result.final_output)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
