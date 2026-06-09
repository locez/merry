from __future__ import annotations

import asyncio

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


async def run_named_runtime(label: str) -> None:
    runtime = merry.Runtime.from_env(session_id=f"multi-runtime-{label}")
    token = f"{label.upper()}-SESSION-TOKEN"
    print(f"[{label}] runtime_start handle_session={runtime.session_id}")

    for round_index, task_template in ROUND_TASKS:
        task = task_template.format(label=label, token=token)
        print(
            f"[{label}] round={round_index} input "
            f"handle_session={runtime.session_id} expected_token={token}"
        )

        stream = runtime.stream(task)
        async for event in stream:
            kind = event["kind"]["type"]
            event_session = event.get("session_id")
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


async def main() -> None:
    labels = ["alpha", "beta", "gamma"]

    await asyncio.gather(*(run_named_runtime(label) for label in labels))


if __name__ == "__main__":
    asyncio.run(main())
