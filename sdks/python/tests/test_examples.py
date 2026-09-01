from __future__ import annotations

import merry
from examples import (
    basic_agent,
    multi_runtime_orchestration,
    structured_output,
    tool_decorator,
)


def test_examples_import_and_expose_async_entrypoints() -> None:
    assert callable(basic_agent.main)
    assert callable(multi_runtime_orchestration.main)
    assert callable(structured_output.main)
    assert callable(tool_decorator.main)


def test_examples_cover_typed_events_multiple_runtimes_and_two_field_output() -> None:
    event = merry.Event(
        merry.EventType.ASSISTANT_MESSAGE,
        merry.AssistantMessagePayload(
            "hello",
            merry.ArtifactReference("artifact-1", "assistant_message", None),
            merry.EventSource("example-session", 1),
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
    lookup = tool_decorator.LookupOrderInput(order_id="A123")

    assert report.session_id == "example-testing"
    assert answer.next_step == "continue"
    assert lookup.order_id == "A123"
