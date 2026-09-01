from __future__ import annotations

import asyncio
import json
import logging
import os

from pydantic import BaseModel, ConfigDict, Field

import merry

logger = logging.getLogger(__name__)


class Answer(BaseModel):
    model_config = ConfigDict(extra="forbid")

    summary: str = Field(description="Concise answer summary.")
    next_step: str = Field(description="One practical next step for the reader.")


def build_agent() -> merry.Agent:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL")
    if api_key is None or model is None:
        raise SystemExit(
            "Set MERRY_OPENAI_API_KEY and MERRY_OPENAI_MODEL before running."
        )
    return (
        merry.AgentBuilder("python-structured-example")
        .provider(
            merry.OpenAICompatible(
                api_key=api_key,
                model=model,
                base_url=os.environ.get("MERRY_OPENAI_BASE_URL"),
            )
        )
        .build()
    )


async def main() -> None:
    result = await build_agent().run(
        "Return JSON with exactly two fields: summary and next_step. "
        "Summarize the purpose of Merry and give one practical next step.",
        final_output_model=Answer,
    )
    output = result.structured_output
    if output is None:
        raise RuntimeError("The model did not return the requested structured output.")
    logger.info(
        "status=%s summary=%s next_step=%s final_output_json=%s",
        result.status.value,
        output.summary,
        output.next_step,
        json.dumps(output.model_dump(mode="json"), ensure_ascii=False, sort_keys=True),
    )


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
