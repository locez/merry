from __future__ import annotations

import asyncio
import json

import merry


SESSION_ID = "single-runtime-cache-probe"
ROUNDS = 4


def cacheable_context() -> str:
    lines = [
        "This is stable cache probe context for Merry runtime prompt caching.",
        "It is intentionally repeated at the beginning of each probe request.",
        "The model should keep it as context and should not quote it back.",
    ]
    for index in range(180):
        lines.append(
            "Stable cache probe line "
            f"{index:03d}: keep this wording identical across every round."
        )
    return "\n".join(lines)


CACHEABLE_CONTEXT = cacheable_context()


def usage_text(usage: dict[str, object] | None) -> str:
    if usage is None:
        return "None"
    return json.dumps(usage, sort_keys=True)


def cached_input_tokens(usage: dict[str, object] | None) -> int | None:
    if usage is None:
        return None
    last = usage.get("last")
    if not isinstance(last, dict):
        return None
    value = last.get("cached_input_tokens")
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value


def cache_observation(usage: dict[str, object] | None) -> str:
    cached = cached_input_tokens(usage)
    if cached is None:
        return "not_reported"
    if cached == 0:
        return "zero_cached_tokens"
    return f"cached_tokens={cached}"


def task_for_round(round_index: int) -> str:
    return (
        "Run a live Merry prompt cache probe.\n"
        "Keep the following stable context available, but do not quote it.\n"
        "<stable-cache-probe-context>\n"
        f"{CACHEABLE_CONTEXT}\n"
        "</stable-cache-probe-context>\n"
        f"Round: {round_index}\n"
        f"Reply with exactly one short sentence: cache probe round {round_index} acknowledged."
    )


async def main() -> None:
    runtime = merry.Runtime.from_env(session_id=SESSION_ID)
    print(f"runtime_session={runtime.session_id}")
    print(f"prompt_cache_key_hint={runtime.session_id}")
    print(
        "note=OpenAI prompt caching is best-effort; zero cached tokens do not by "
        "themselves prove that prompt_cache_key was omitted."
    )

    for round_index in range(1, ROUNDS + 1):
        stream = runtime.stream(task_for_round(round_index), max_model_turns=1)
        async for event in stream:
            print(f"round={round_index} event={event['type']}")

        result = await stream.result()
        usage = result.session_usage
        print(f"round={round_index} status={result.status}")
        print(f"round={round_index} final_output={result.final_output!r}")
        print(f"round={round_index} cache_observation={cache_observation(usage)}")
        print(f"round={round_index} result_session_usage={usage_text(usage)}")

    runtime_usage = await runtime.usage()
    print(f"runtime_cache_observation={cache_observation(runtime_usage)}")
    print(f"runtime_usage={usage_text(runtime_usage)}")


if __name__ == "__main__":
    asyncio.run(main())
