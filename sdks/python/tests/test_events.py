from __future__ import annotations

import pytest

import merry
from merry._event_parser import parse_event


def test_event_parser_returns_typed_payloads() -> None:
    event = parse_event(
        {
            "type": "assistant_message",
            "text": "hello",
            "artifact": {
                "id": "artifact-1",
                "kind": "assistant_message",
                "label": None,
            },
            "source": {"session_id": "session-1", "sequence": 1},
        }
    )

    assert event.type is merry.EventType.ASSISTANT_MESSAGE
    assert isinstance(event.payload, merry.AssistantMessagePayload)
    assert event.payload.text == "hello"
    assert event.payload.source.sequence == 1


def test_event_parser_preserves_line_breaks_in_assistant_text() -> None:
    delta = parse_event(
        {
            "type": "assistant_message_delta",
            "delta": "line one\r\nline two\tcontinued",
            "source": {"session_id": "session-1", "sequence": 1},
        }
    )
    message = parse_event(
        {
            "type": "assistant_message",
            "text": "line one\r\nline two\tcontinued",
            "artifact": {"id": "artifact-1", "kind": "text", "label": None},
            "source": {"session_id": "session-1", "sequence": 2},
        }
    )

    assert isinstance(delta.payload, merry.AssistantMessageDeltaPayload)
    assert delta.payload.delta == "line one\r\nline two\tcontinued"
    assert isinstance(message.payload, merry.AssistantMessagePayload)
    assert message.payload.text == "line one\r\nline two\tcontinued"


def test_event_parser_accepts_blank_assistant_text_and_delta() -> None:
    delta = parse_event(
        {
            "type": "assistant_message_delta",
            "delta": "",
            "source": {"session_id": "session-1", "sequence": 1},
        }
    )
    message = parse_event(
        {
            "type": "assistant_message",
            "text": "",
            "artifact": {"id": "artifact-1", "kind": "text", "label": None},
            "source": {"session_id": "session-1", "sequence": 2},
        }
    )

    assert isinstance(delta.payload, merry.AssistantMessageDeltaPayload)
    assert delta.payload.delta == ""
    assert isinstance(message.payload, merry.AssistantMessagePayload)
    assert message.payload.text == ""


def test_event_parser_accepts_reasoning_delta() -> None:
    event = parse_event(
        {
            "type": "reasoning_delta",
            "delta": "thinking\nmore",
            "source": {"session_id": "session-1", "sequence": 3},
        }
    )

    assert event.type is merry.EventType.REASONING_DELTA
    assert isinstance(event.payload, merry.ReasoningDeltaPayload)
    assert event.payload.delta == "thinking\nmore"
    assert event.payload.source.sequence == 3


def test_event_parser_retains_unknown_variants_without_string_dispatch() -> None:
    event = parse_event(
        {
            "type": "future_runtime_event",
            "future_field": {"value": "retained"},
        }
    )

    assert event.type is merry.EventType.UNKNOWN
    assert isinstance(event.payload, merry.UnknownEventPayload)
    assert event.payload.raw_type == "future_runtime_event"
    assert event.payload.data.value["future_field"] == {"value": "retained"}


def test_event_envelope_rejects_mismatched_discriminants() -> None:
    source = merry.EventSource("session-1", 1)
    with pytest.raises(ValueError, match="discriminants"):
        merry.Event(merry.EventType.CLOSED, merry.SessionStartedPayload(source))


def test_event_value_objects_validate_nested_types_and_json_kind() -> None:
    artifact = merry.ArtifactReference("artifact-1", "tool", None)
    diagnostic = merry.EventDiagnostic("tool.failed", "Tool failed.")

    with pytest.raises(TypeError, match="artifact reference"):
        parse_event(
            {
                "type": "tool_call_finished",
                "result": {
                    "call_id": "call-1",
                    "status": "succeeded",
                    "artifact": "artifact-1",
                    "diagnostic": None,
                },
                "output": None,
                "source": {"session_id": "session-1", "sequence": 1},
            }
        )
    with pytest.raises(TypeError, match="evidence locator"):
        parse_event(
            {
                "type": "evidence_referenced",
                "evidence": {"artifact_id": "artifact-1", "locator": "line-1"},
                "source": {"session_id": "session-1", "sequence": 1},
            }
        )
    with pytest.raises(ValueError, match="valid JSON"):
        merry.RuntimeToolOutput(merry.RuntimeToolOutputKind.JSON, "not-json")

    result = merry.RuntimeToolResult(
        call_id="call-1",
        status=merry.RuntimeToolResultStatus.FAILED,
        artifact=artifact,
        diagnostic=diagnostic,
    )
    assert result.diagnostic == diagnostic


def test_compaction_usage_requires_the_single_threshold() -> None:
    from merry._models import parse_session_usage

    usage = {
        "input_tokens": 1,
        "cached_input_tokens": None,
        "output_tokens": 1,
        "reasoning_output_tokens": None,
        "total_tokens": 2,
    }
    current = parse_session_usage(
        {
            "total": usage,
            "last": usage,
            "context": None,
            "compaction": {
                "auto_compaction_enabled": True,
                "dynamic_body_estimated_tokens": 40,
                "body_budget_tokens": 100,
                "compact_at_tokens": 85,
            },
        }
    )
    assert current.compaction is not None
    assert current.compaction.dynamic_body_estimated_tokens == 40
    assert current.compaction.compact_at_tokens == 85

    with pytest.raises(TypeError, match="missing required fields"):
        parse_session_usage(
            {
                "total": usage,
                "last": usage,
                "context": None,
                "compaction": {
                    "auto_compaction_enabled": True,
                    "body_budget_tokens": 100,
                    "compact_at_tokens": 85,
                },
            }
        )
    with pytest.raises(TypeError, match="unsupported fields"):
        parse_session_usage(
            {
                "total": usage,
                "last": usage,
                "context": None,
                "compaction": {
                    "auto_compaction_enabled": True,
                    "dynamic_body_estimated_tokens": 40,
                    "body_budget_tokens": 100,
                    "compact_at_tokens": 85,
                    "extra_tokens": 90,
                },
            }
        )
