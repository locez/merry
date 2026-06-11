from __future__ import annotations

import asyncio
import json
import os

import merry


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


def usage_text(usage: dict[str, object] | None) -> str:
    if usage is None:
        return "None"
    return json.dumps(usage, sort_keys=True)


async def main() -> None:
    runtime = runtime_from_env_config()

    stream = runtime.stream(
        "Reply in one short sentence that confirms the Merry Python SDK is connected."
    )

    print("events:")
    async for event in stream:
        print(f"- {event['type']}")

    result = await stream.result()

    print(f"status: {result.status}")
    print(f"model_turns_run: {result.model_turns_run}")
    print(f"final_output: {result.final_output}")
    print(f"result_session_usage: {usage_text(result.session_usage)}")
    print(f"runtime_usage: {usage_text(await runtime.usage())}")


if __name__ == "__main__":
    asyncio.run(main())
