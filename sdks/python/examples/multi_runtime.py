from __future__ import annotations

import asyncio
import json

import merry


ROUND_TASKS = [
    (
        1,
        "Start a short three-turn check. Reply with exactly this token first: {token}. "
        "Then say you are runtime {label}.",
    ),
    (
        2,
        "Continue the same check. Reply with the token from round 1, then say this is "
        "round 2 for runtime {label}. Do not mention any other runtime label.",
    ),
    (
        3,
        "Finish the same check. Reply with the token from round 1, then say this is "
        "round 3 for runtime {label}. Do not mention any other runtime label.",
    ),
]


def usage_text(usage: dict[str, object] | None) -> str:
    if usage is None:
        return "None"
    return json.dumps(usage, sort_keys=True)


async def run_named_runtime(label: str) -> None:
    runtime = merry.Runtime.from_env(session_id=f"multi-runtime-{label}")
    token = f"{label.upper()}-SESSION-TOKEN"
    print(
        f"[{label}] runtime_start handle_session={runtime.session_id} "
        f"prompt_cache_key_hint={runtime.session_id}"
    )

    for round_index, task_template in ROUND_TASKS:
        task = task_template.format(label=label, token=token)
        print(
            f"[{label}] round={round_index} input "
            f"handle_session={runtime.session_id} expected_token={token}"
        )

        stream = runtime.stream(task)
        async for event in stream:
            kind = event["type"]
            source = event.get("source")
            event_session = source.get("session_id") if isinstance(source, dict) else None
            print(
                f"[{label}] round={round_index} event kind={kind} "
                f"handle_session={runtime.session_id} event_session={event_session}"
            )

        result = await stream.result()
        print(
            f"[{label}] round={round_index} result status={result.status} "
            f"handle_session={runtime.session_id} "
            f"model_turns_run={result.model_turns_run}"
        )
        print(f"[{label}] round={round_index} final_output={result.final_output!r}")
        print(
            f"[{label}] round={round_index} "
            f"result_session_usage={usage_text(result.session_usage)}"
        )
        print(
            f"[{label}] round={round_index} "
            f"runtime_usage={usage_text(await runtime.usage())}"
        )


async def main() -> None:
    labels = ["alpha", "beta", "gamma"]

    print(
        "note=This example checks concurrent runtime isolation. "
        "Provider cached_input_tokens are best-effort diagnostics, not a "
        "per-runtime pass/fail assertion."
    )
    await asyncio.gather(*(run_named_runtime(label) for label in labels))


if __name__ == "__main__":
    asyncio.run(main())
