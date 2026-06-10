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

        if event_type == "interactive_run_state_changed":
            print(f"[{label}] state={event['state']}")
        elif event_type == "queued_input_accepted":
            print(
                f"[{label}] input_accepted "
                f"lane={event['lane']} texts={_event_texts(event)}"
            )
        elif event_type == "queued_inputs_changed":
            inputs = event["inputs"]
            if not isinstance(inputs, dict):
                raise TypeError("interactive queued inputs must be a dict")
            print(
                f"[{label}] queue "
                f"next={_texts(inputs, 'next')} "
                f"suspended={_texts(inputs, 'suspended')} "
                f"backlog={_texts(inputs, 'backlog')}"
            )
        elif event_type == "assistant_message":
            print(f"[{label}] assistant={event['text']}")
        elif event_type == "tool_call_started":
            call = event["call"]
            if not isinstance(call, dict):
                raise TypeError("tool call must be a dict")
            print(f"[{label}] tool_call_started name={call.get('name')} id={call.get('id')}")
        elif event_type == "tool_call_finished":
            result = event["result"]
            if not isinstance(result, dict):
                raise TypeError("tool result must be a dict")
            print(f"[{label}] tool_call_finished status={result.get('status')}")
        elif event_type == "step_completed":
            source = event.get("source")
            print(f"[{label}] runtime_event type=step_completed source={source}")
            seen_completed_turns += 1
            if seen_completed_turns >= completed_turns:
                completed.set()
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


def _event_texts(event: dict[str, object]) -> list[str]:
    items = event.get("inputs", [])
    if not isinstance(items, list):
        raise TypeError("queued input accepted event inputs must be a list")
    texts: list[str] = []
    for item in items:
        if not isinstance(item, dict):
            raise TypeError("queued input accepted event items must be dicts")
        text = item.get("text")
        if not isinstance(text, str):
            raise TypeError("queued input accepted event item text must be a str")
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
    print(
        f"[{label}] submitted_next lane={first.lane} text={first.text!r} "
        f"handle_session={runtime.session_id}"
    )

    second = await run.input.enqueue(
        "Reply in one short sentence that says this backlog turn will be edited."
    )
    print(
        f"[{label}] enqueued_backlog lane={second.lane} text={second.text!r} "
        f"handle_session={runtime.session_id}"
    )
    try:
        await second.update(
            second.text.replace(
                "will be edited",
                "is the edited automatic backlog turn",
            )
        )
        print(
            f"[{label}] updated_backlog text={second.text!r} "
            f"handle_session={runtime.session_id}"
        )
    except merry.MerryError:
        print(
            f"[{label}] backlog_already_accepted text={second.text!r} "
            f"handle_session={runtime.session_id}"
        )

    await asyncio.wait_for(completed.wait(), timeout=180)
    await run.control.close()
    await render_task
    print(f"[{label}] closed handle_session={runtime.session_id}")


if __name__ == "__main__":
    asyncio.run(main())
