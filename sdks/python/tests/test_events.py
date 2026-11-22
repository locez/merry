from __future__ import annotations

import pytest

import merry
from merry._event_parser import parse_event


def test_event_parser_returns_typed_payloads() -> None:
    event = parse_event(
        {
            "type": "assistant_message",
            "text": "hello",
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
    diagnostic = merry.EventDiagnostic("tool.failed", "Tool failed.")

    with pytest.raises(TypeError, match="event diagnostic"):
        parse_event(
            {
                "type": "tool_call_finished",
                "result": {
                    "call_id": "call-1",
                    "status": "failed",
                    "diagnostic": "not-an-object",
                },
                "output": None,
                "source": {"session_id": "session-1", "sequence": 1},
            }
        )
    with pytest.raises(ValueError, match="valid JSON"):
        merry.RuntimeToolOutput(merry.RuntimeToolOutputKind.JSON, "not-json")

    result = merry.RuntimeToolResult(
        call_id="call-1",
        status=merry.RuntimeToolResultStatus.FAILED,
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


def test_event_parser_parses_single_pending_queue_events() -> None:
    changed = parse_event(
        {
            "type": "queued_inputs_changed",
            "inputs": {
                "items": [
                    {"id": 1, "text": "first pending", "position": 0},
                    {"id": 2, "text": "second pending", "position": 1},
                ],
                "paused": True,
            },
        }
    )
    assert changed.type is merry.EventType.QUEUED_INPUTS_CHANGED
    assert isinstance(changed.payload, merry.QueuedInputsChangedPayload)
    assert changed.payload.inputs.paused is True
    assert changed.payload.inputs.items == (
        merry.QueuedInput(id=1, text="first pending", position=0),
        merry.QueuedInput(id=2, text="second pending", position=1),
    )

    accepted = parse_event(
        {
            "type": "queued_input_accepted",
            "inputs": [
                {"id": 1, "text": "first pending", "position": 0},
            ],
        }
    )
    assert accepted.type is merry.EventType.QUEUED_INPUT_ACCEPTED
    assert isinstance(accepted.payload, merry.QueuedInputAcceptedPayload)
    assert accepted.payload.inputs == (
        merry.QueuedInput(id=1, text="first pending", position=0),
    )

    with pytest.raises(TypeError):
        parse_event(
            {
                "type": "queued_input_accepted",
                "lane": "next",
                "inputs": [{"id": 1, "text": "legacy", "position": 0}],
            }
        )


def test_event_parser_supports_enriched_subagent_events_and_usage_rollup() -> None:
    from merry._models import parse_session_usage

    spawned = parse_event(
        {
            "type": "subagent_spawned",
            "agent_id": "subagent-1",
            "task_id": "task-1",
            "task_anchor": "Audit auth flow",
            "display_name": "auditor",
            "role": "explore",
            "source": {"session_id": "session-1", "sequence": 3},
        }
    )
    assert spawned.type is merry.EventType.SUBAGENT_SPAWNED
    assert isinstance(spawned.payload, merry.SubagentSpawnedPayload)
    assert spawned.payload.display_name == "auditor"
    assert spawned.payload.role == "explore"

    blocked = parse_event(
        {
            "type": "subagent_status_changed",
            "agent_id": "subagent-1",
            "task_id": "task-1",
            "status": "blocked",
            "source": {"session_id": "session-1", "sequence": 4},
        }
    )
    assert isinstance(blocked.payload, merry.SubagentStatusChangedPayload)
    assert blocked.payload.status is merry.SubagentStatus.BLOCKED

    completed = parse_event(
        {
            "type": "subagent_completed",
            "agent_id": "subagent-1",
            "task_id": "task-1",
            "summary": "Audit complete",
            "output_paths": [],
            "changed_paths": ["src/auth.rs"],
            "usage": {
                "input_tokens": 120,
                "cached_input_tokens": 40,
                "output_tokens": 30,
                "reasoning_output_tokens": None,
                "total_tokens": 150,
            },
            "source": {"session_id": "session-1", "sequence": 5},
        }
    )
    assert isinstance(completed.payload, merry.SubagentCompletedPayload)
    assert completed.payload.usage is not None
    assert completed.payload.usage.total_tokens == 150

    usage = parse_session_usage(
        {
            "total": {
                "input_tokens": 620,
                "cached_input_tokens": None,
                "output_tokens": 80,
                "reasoning_output_tokens": None,
                "total_tokens": 700,
            },
            "last": {
                "input_tokens": 500,
                "cached_input_tokens": None,
                "output_tokens": 50,
                "reasoning_output_tokens": None,
                "total_tokens": 550,
            },
            "context": None,
            "compaction": None,
            "subagent_total": {
                "input_tokens": 120,
                "cached_input_tokens": 40,
                "output_tokens": 30,
                "reasoning_output_tokens": None,
                "total_tokens": 150,
            },
        }
    )
    assert usage.subagent_total is not None
    assert usage.subagent_total.input_tokens == 120
    assert usage.subagent_total.output_tokens == 30


def test_event_parser_supports_plan_mode_and_todo_list_events() -> None:
    mode_changed = parse_event(
        {
            "type": "plan_mode_changed",
            "mode": "active",
            "source": {"session_id": "session-1", "sequence": 1},
        }
    )
    assert mode_changed.type is merry.EventType.PLAN_MODE_CHANGED
    assert isinstance(mode_changed.payload, merry.PlanModeChangedPayload)
    assert mode_changed.payload.mode is merry.PlanModeState.ACTIVE

    plan_doc = {
        "id": "plan-001",
        "slug": "auth-refactor",
        "title": "Auth Refactor",
        "status": "draft",
        "content": "# Auth Refactor\n\n- Step 1",
    }
    saved = parse_event(
        {
            "type": "plan_saved",
            "plan": plan_doc,
            "source": {"session_id": "session-1", "sequence": 2},
        }
    )
    assert saved.type is merry.EventType.PLAN_SAVED
    assert isinstance(saved.payload, merry.PlanSavedPayload)
    assert saved.payload.plan.value["id"] == "plan-001"

    approval = parse_event(
        {
            "type": "plan_approval_requested",
            "plan": plan_doc,
            "source": {"session_id": "session-1", "sequence": 3},
        }
    )
    assert approval.type is merry.EventType.PLAN_APPROVAL_REQUESTED
    assert isinstance(approval.payload, merry.PlanApprovalRequestedPayload)
    assert approval.payload.plan.value["title"] == "Auth Refactor"

    reviewed = parse_event(
        {
            "type": "plan_reviewed",
            "plan_id": "plan-001",
            "decision": {"kind": "approve", "comments": "Looks good"},
            "source": {"session_id": "session-1", "sequence": 4},
        }
    )
    assert reviewed.type is merry.EventType.PLAN_REVIEWED
    assert isinstance(reviewed.payload, merry.PlanReviewedPayload)
    assert reviewed.payload.plan_id == "plan-001"
    assert reviewed.payload.decision.value["kind"] == "approve"

    todos = parse_event(
        {
            "type": "todo_list_updated",
            "items": [
                {"id": "1", "content": "Inspect runtime", "status": "completed"},
                {"id": "2", "content": "Update tests", "status": "in_progress"},
            ],
            "source": {"session_id": "session-1", "sequence": 5},
        }
    )
    assert todos.type is merry.EventType.TODO_LIST_UPDATED
    assert isinstance(todos.payload, merry.TodoListUpdatedPayload)
    assert todos.payload.items == (
        merry.TodoItem(
            id="1",
            content="Inspect runtime",
            status=merry.TodoStatus.COMPLETED,
        ),
        merry.TodoItem(
            id="2",
            content="Update tests",
            status=merry.TodoStatus.IN_PROGRESS,
        ),
    )

    with pytest.raises(TypeError, match="todo status"):
        parse_event(
            {
                "type": "todo_list_updated",
                "items": [{"id": "1", "content": "Bad status", "status": "unknown"}],
                "source": {"session_id": "session-1", "sequence": 6},
            }
        )
