from __future__ import annotations

import merry
from examples import (
    basic_agent,
    interactive_session,
    multi_runtime_orchestration,
    structured_output,
    tool_decorator,
)
from merry import events


def test_examples_import_and_expose_async_entrypoints() -> None:
    assert callable(basic_agent.main)
    assert callable(interactive_session.main)
    assert callable(multi_runtime_orchestration.main)
    assert callable(structured_output.main)
    assert callable(tool_decorator.main)


def test_examples_cover_typed_events_multiple_runtimes_and_two_field_output() -> None:
    event = merry.Event(
        merry.EventType.ASSISTANT_MESSAGE,
        events.AssistantMessagePayload(
            "hello",
            events.EventSource("example-session", 1),
        ),
    )
    basic_agent.handle_event(event)

    report = multi_runtime_orchestration.RuntimeReport(
        label="testing",
        session_id="example-testing",
        status=merry.RunStatus.COMPLETED,
        event_types=(merry.EventType.ASSISTANT_MESSAGE.value,),
        final_output="done",
    )
    answer = structured_output.Answer(summary="done", next_step="continue")

    assert report.session_id == "example-testing"
    assert answer.next_step == "continue"
    assert tool_decorator.lookup_order.name == "lookup_order"
    assert tool_decorator.lookup_order.parallel_safe is True
