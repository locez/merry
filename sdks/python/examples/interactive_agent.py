from __future__ import annotations

import asyncio
import os
from collections.abc import AsyncIterator

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


async def render_events(
    label: str,
    stream: AsyncIterator[dict[str, object]],
    *,
    completed_turns: int,
    completed: asyncio.Event,
) -> None:
    seen_completed_turns = 0
    async for event in stream:
        event_type = event.get("type")

        if event_type == "runtime":
            runtime_event = event["event"]
            if not isinstance(runtime_event, dict):
                raise TypeError("interactive runtime event must be a dict")
            kind = runtime_event["kind"]
            if not isinstance(kind, dict):
                raise TypeError("interactive runtime event kind must be a dict")
            kind_type = kind["type"]
            event_session = runtime_event.get("session_id")
            print(f"[{label}] runtime_event kind={kind_type} event_session={event_session}")
            if kind_type == "step_completed":
                seen_completed_turns += 1
                if seen_completed_turns >= completed_turns:
                    completed.set()
        elif event_type == "state_changed":
            print(f"[{label}] state={event['state']}")
        elif event_type == "input_accepted":
            print(f"[{label}] input_accepted queue={event['queue']} ids={event['ids']}")
        elif event_type == "queue_changed":
            snapshot = event["snapshot"]
            if not isinstance(snapshot, dict):
                raise TypeError("interactive queue snapshot must be a dict")
            print(
                f"[{label}] queue "
                f"next={_texts(snapshot, 'next')} "
                f"suspended={_texts(snapshot, 'suspended')} "
                f"backlog={_texts(snapshot, 'backlog')}"
            )
        else:
            print(f"[{label}] event type={event_type}")


def _texts(snapshot: dict[str, object], queue: str) -> list[str]:
    items = snapshot.get(queue, [])
    if not isinstance(items, list):
        raise TypeError(f"interactive queue {queue} must be a list")
    texts: list[str] = []
    for item in items:
        if not isinstance(item, dict):
            raise TypeError(f"interactive queue {queue} items must be dicts")
        text = item.get("text")
        if not isinstance(text, str):
            raise TypeError(f"interactive queue {queue} item text must be a str")
        texts.append(text)
    return texts


async def main() -> None:
    label = "interactive"
    runtime = runtime_from_env_config()
    run = runtime.start_interactive(max_model_turns=8)
    completed = asyncio.Event()

    print(f"[{label}] runtime_start handle_session={runtime.session_id}")
    render_task = asyncio.create_task(
        render_events(label, run.stream, completed_turns=2, completed=completed)
    )

    first = await run.input.submit_next(
        "Reply in one short sentence that says this is the first interactive turn."
    )
    print(f"[{label}] submitted_next id={first['id']} handle_session={runtime.session_id}")

    second = await run.input.enqueue(
        "Reply in one short sentence that says this is the queued backlog turn."
    )
    print(f"[{label}] enqueued_backlog id={second['id']} handle_session={runtime.session_id}")

    await run.input.update(
        second["id"],
        "Reply in one short sentence that says this is the edited backlog turn.",
    )
    print(f"[{label}] updated_backlog id={second['id']} handle_session={runtime.session_id}")

    await run.control.resume_backlog()
    print(f"[{label}] resumed_backlog handle_session={runtime.session_id}")

    await asyncio.wait_for(completed.wait(), timeout=180)
    await run.control.close()
    await render_task
    print(f"[{label}] closed handle_session={runtime.session_id}")


if __name__ == "__main__":
    asyncio.run(main())
