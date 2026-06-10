from __future__ import annotations

import asyncio
import os

import merry
from pydantic import BaseModel, ConfigDict, Field


class LookupOrderInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier to look up.")


class LookupOrderOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier that was looked up.")
    status: str = Field(description="Current fulfillment status for the order.")


def runtime_from_env_config() -> merry.Runtime:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY") or os.environ.get("OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL") or os.environ.get("OPENAI_MODEL")
    base_url = os.environ.get("MERRY_OPENAI_BASE_URL")

    if not api_key:
        raise SystemExit(
            "config.openai_api_key_missing: Set MERRY_OPENAI_API_KEY or OPENAI_API_KEY."
        )
    if not model:
        raise SystemExit(
            "config.openai_model_missing: Set MERRY_OPENAI_MODEL or OPENAI_MODEL."
        )

    return merry.Runtime(
        config=merry.RuntimeConfig(
            provider=merry.OpenAICompatibleProvider(
                api_key=api_key,
                model=model,
                base_url=base_url,
            )
        )
    )


async def main() -> None:
    runtime = runtime_from_env_config()

    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        """Look up an order by id."""
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    runtime.register_tool(lookup_order)

    stream = runtime.stream(
        "Use the lookup_order tool with order_id A123, then answer with the order status."
    )

    print("events:")
    async for event in stream:
        print(f"- {event['type']}")

    result = await stream.result()

    print(f"status: {result.status}")
    print(f"model_turns_run: {result.model_turns_run}")
    print(f"final_output: {result.final_output}")


if __name__ == "__main__":
    asyncio.run(main())
