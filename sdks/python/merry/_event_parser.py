"""Decode native runtime event JSON into typed Python events."""

from __future__ import annotations

from collections.abc import Mapping

from ._event_fields import EVENT_FIELDS
from ._event_payloads import (
    AssistantMessageDeltaPayload,
    AssistantMessagePayload,
    ClosedPayload,
    CompactionCompletedPayload,
    CompactionStartedPayload,
    EventPayloadValue,
    FinalOutputRecordedPayload,
    InteractiveRunStateChangedPayload,
    ModelOutputRateUpdatedPayload,
    ModelRetryAttemptStartedPayload,
    ModelRetryExhaustedPayload,
    ModelRetryScheduledPayload,
    PlanApprovalRequestedPayload,
    PlanModeChangedPayload,
    PlanReviewedPayload,
    PlanSavedPayload,
    QueuedInputAcceptedPayload,
    QueuedInputsChangedPayload,
    ReasoningDeltaPayload,
    RunCancelledPayload,
    RunFailedPayload,
    SessionStartedPayload,
    SkillUsedPayload,
    StepCompletedPayload,
    StepStartedPayload,
    SubagentCancelledPayload,
    SubagentCompletedPayload,
    SubagentFailedPayload,
    SubagentSpawnedPayload,
    SubagentStartedPayload,
    SubagentStatusChangedPayload,
    TodoListUpdatedPayload,
    ToolCallBatchStartedPayload,
    ToolCallFinishedPayload,
    ToolCallStartedPayload,
    UnknownEventPayload,
    UsageUpdatedPayload,
)
from ._event_types import (
    EventDiagnostic,
    EventSource,
    EventType,
    InteractiveRunState,
    PlanModeState,
    QueuedInput,
    QueuedInputs,
    RawEventData,
    RuntimeToolCall,
    RuntimeToolCallBatch,
    RuntimeToolOutput,
    RuntimeToolOutputKind,
    RuntimeToolResult,
    RuntimeToolResultStatus,
    SubagentStatus,
    TodoItem,
    TodoStatus,
)
from ._event_values import (
    _enum_value,
    _event_text,
    _optional_string,
    _required_int,
    _required_string,
    _required_strings,
)
from ._events import Event
from ._json import (
    JsonObject,
    JsonValue,
    require_json_object,
    validate_object_keys,
)
from ._models import SessionUsage, _parse_model_usage
from ._output_rate import parse_output_rate


def parse_event(value: object) -> Event:
    """Parse one native event into a typed event, retaining unknown variants."""

    data = require_json_object(value, "native runtime event")
    raw_type = _required_string(data, "type")
    try:
        event_type = EventType(raw_type)
    except ValueError:
        return Event(
            EventType.UNKNOWN, UnknownEventPayload(raw_type, RawEventData(data))
        )

    required, optional = EVENT_FIELDS[event_type]
    validate_object_keys(
        data,
        "native runtime event",
        required=required,
        optional=optional,
    )
    payload = _parse_payload(event_type, raw_type, data)
    return Event(event_type, payload)


def _parse_payload(
    event_type: EventType, raw_type: str, data: JsonObject
) -> EventPayloadValue:
    match event_type:
        case EventType.SESSION_STARTED:
            return SessionStartedPayload(_source(data))
        case EventType.STEP_STARTED:
            return StepStartedPayload(_source(data))
        case EventType.STEP_COMPLETED:
            return StepCompletedPayload(_source(data))
        case EventType.COMPACTION_STARTED:
            return CompactionStartedPayload(_source(data))
        case EventType.COMPACTION_COMPLETED:
            return CompactionCompletedPayload(
                _required_string(data, "checkpoint_id"),
                _required_int(data, "covered_history_item_count"),
                _source(data),
            )
        case EventType.USAGE_UPDATED:
            return UsageUpdatedPayload(_parse_usage(data["usage"]), _source(data))
        case EventType.MODEL_OUTPUT_RATE_UPDATED:
            return ModelOutputRateUpdatedPayload(
                parse_output_rate(data["rate"]), _source(data)
            )
        case EventType.ASSISTANT_MESSAGE:
            return AssistantMessagePayload(
                _event_text(data, "text"),
                _source(data),
            )
        case EventType.ASSISTANT_MESSAGE_DELTA:
            return AssistantMessageDeltaPayload(
                _event_text(data, "delta"), _source(data)
            )
        case EventType.REASONING_DELTA:
            return ReasoningDeltaPayload(_event_text(data, "delta"), _source(data))
        case EventType.TOOL_CALL_STARTED:
            return ToolCallStartedPayload(_tool_call(data["call"]), _source(data))
        case EventType.TOOL_CALL_BATCH_STARTED:
            return ToolCallBatchStartedPayload(
                _tool_batch(data["batch"]), _source(data)
            )
        case EventType.TOOL_CALL_FINISHED:
            return ToolCallFinishedPayload(
                _tool_result(data["result"]),
                _optional_tool_output(data.get("output")),
                _source(data),
            )
        case EventType.FINAL_OUTPUT_RECORDED:
            return FinalOutputRecordedPayload(
                _required_string(data, "call_id"),
                _event_text(data, "json"),
                _source(data),
            )
        case EventType.MODEL_RETRY_ATTEMPT_STARTED:
            return ModelRetryAttemptStartedPayload(
                _required_int(data, "attempt"),
                _required_int(data, "max_attempts"),
                _source(data),
            )
        case EventType.MODEL_RETRY_SCHEDULED:
            return ModelRetryScheduledPayload(
                _required_int(data, "attempt"),
                _required_int(data, "next_attempt"),
                _required_int(data, "max_attempts"),
                _required_int(data, "delay_ms"),
                _required_string(data, "error_kind"),
                _source(data),
            )
        case EventType.MODEL_RETRY_EXHAUSTED:
            return ModelRetryExhaustedPayload(
                _required_int(data, "attempts_run"),
                _required_int(data, "max_attempts"),
                _required_string(data, "error_kind"),
                _source(data),
            )
        case EventType.SKILL_USED:
            return SkillUsedPayload(
                _required_string(data, "skill_name"),
                _required_string(data, "skill_md_path"),
                _required_string(data, "tool_call_id"),
                _source(data),
            )
        case EventType.SUBAGENT_SPAWNED:
            return SubagentSpawnedPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _required_string(data, "task_anchor"),
                _source(data),
                display_name=_optional_string(data.get("display_name")),
                role=_optional_string(data.get("role")),
            )
        case EventType.SUBAGENT_STARTED:
            return SubagentStartedPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _source(data),
            )
        case EventType.SUBAGENT_STATUS_CHANGED:
            return SubagentStatusChangedPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _enum_value(SubagentStatus, data["status"], "subagent status"),
                _source(data),
            )
        case EventType.SUBAGENT_COMPLETED:
            usage_value = data.get("usage", None)
            usage = None if usage_value is None else _parse_model_usage(usage_value)
            return SubagentCompletedPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _required_string(data, "summary"),
                _required_strings(data["output_paths"], "output paths"),
                _required_strings(data["changed_paths"], "changed paths"),
                _source(data),
                usage=usage,
            )
        case EventType.SUBAGENT_FAILED:
            return SubagentFailedPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _diagnostic(data["diagnostic"]),
                _source(data),
            )
        case EventType.SUBAGENT_CANCELLED:
            return SubagentCancelledPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _diagnostic(data["diagnostic"]),
                _source(data),
            )
        case EventType.PLAN_MODE_CHANGED:
            return PlanModeChangedPayload(
                _enum_value(PlanModeState, data["mode"], "plan mode"),
                _source(data),
            )
        case EventType.PLAN_SAVED:
            return PlanSavedPayload(
                RawEventData.from_value(data["plan"], "plan document"),
                _source(data),
            )
        case EventType.PLAN_APPROVAL_REQUESTED:
            return PlanApprovalRequestedPayload(
                RawEventData.from_value(data["plan"], "plan document"),
                _source(data),
            )
        case EventType.PLAN_REVIEWED:
            return PlanReviewedPayload(
                _required_string(data, "plan_id"),
                RawEventData.from_value(data["decision"], "plan review decision"),
                _source(data),
            )
        case EventType.TODO_LIST_UPDATED:
            return TodoListUpdatedPayload(
                _todo_items(data["items"]),
                _source(data),
            )
        case EventType.RUN_FAILED:
            return RunFailedPayload(_diagnostic(data["diagnostic"]), _source(data))
        case EventType.RUN_CANCELLED:
            return RunCancelledPayload(_diagnostic(data["diagnostic"]), _source(data))
        case EventType.INTERACTIVE_RUN_STATE_CHANGED:
            return InteractiveRunStateChangedPayload(
                _enum_value(InteractiveRunState, data["state"], "interactive run state")
            )
        case EventType.QUEUED_INPUT_ACCEPTED:
            return QueuedInputAcceptedPayload(_queued_inputs(data["inputs"]))
        case EventType.QUEUED_INPUTS_CHANGED:
            return QueuedInputsChangedPayload(_queued_input_groups(data["inputs"]))
        case EventType.CLOSED:
            return ClosedPayload()
        case EventType.UNKNOWN:
            return UnknownEventPayload(raw_type, RawEventData(data))


def _todo_items(value: object) -> tuple[TodoItem, ...]:
    if not isinstance(value, list):
        raise TypeError("todo items must be a list")
    items: list[TodoItem] = []
    for entry in value:
        raw = require_json_object(entry, "todo item")
        validate_object_keys(
            raw,
            "todo item",
            required={"id", "content", "status"},
        )
        items.append(
            TodoItem(
                _required_string(raw, "id"),
                _required_string(raw, "content"),
                _enum_value(TodoStatus, raw["status"], "todo status"),
            )
        )
    return tuple(items)


def _source(data: Mapping[str, JsonValue]) -> EventSource:
    source = require_json_object(data["source"], "event source")
    validate_object_keys(
        source,
        "event source",
        required={"session_id", "sequence"},
    )
    return EventSource(
        session_id=_required_string(source, "session_id"),
        sequence=_required_int(source, "sequence"),
    )


def _tool_call(value: object) -> RuntimeToolCall:
    data = require_json_object(value, "runtime tool call")
    validate_object_keys(
        data,
        "runtime tool call",
        required={"id", "name", "arguments"},
    )
    return RuntimeToolCall(
        id=_required_string(data, "id"),
        name=_required_string(data, "name"),
        arguments=require_json_object(data["arguments"], "tool call arguments"),
    )


def _tool_batch(value: object) -> RuntimeToolCallBatch:
    data = require_json_object(value, "runtime tool call batch")
    validate_object_keys(data, "runtime tool call batch", required={"id", "calls"})
    raw_calls = data["calls"]
    if not isinstance(raw_calls, list) or not raw_calls:
        raise TypeError("runtime tool call batch must contain calls")
    return RuntimeToolCallBatch(
        id=_required_string(data, "id"),
        calls=tuple(_tool_call(call) for call in raw_calls),
    )


def _tool_result(value: object) -> RuntimeToolResult:
    data = require_json_object(value, "runtime tool result")
    validate_object_keys(
        data,
        "runtime tool result",
        required={"call_id", "status", "diagnostic"},
    )
    return RuntimeToolResult(
        call_id=_required_string(data, "call_id"),
        status=_enum_value(
            RuntimeToolResultStatus, data["status"], "tool result status"
        ),
        diagnostic=None
        if data.get("diagnostic") is None
        else _diagnostic(data["diagnostic"]),
    )


def _optional_tool_output(value: object) -> RuntimeToolOutput | None:
    if value is None:
        return None
    data = require_json_object(value, "runtime tool output")
    kind = _enum_value(RuntimeToolOutputKind, data["kind"], "tool output kind")
    key = "text" if kind is RuntimeToolOutputKind.TEXT else "json"
    validate_object_keys(data, "runtime tool output", required={"kind", key})
    return RuntimeToolOutput(kind=kind, value=_required_string(data, key))


def _diagnostic(value: object) -> EventDiagnostic:
    data = require_json_object(value, "event diagnostic")
    validate_object_keys(data, "event diagnostic", required={"code", "message"})
    return EventDiagnostic(
        code=_required_string(data, "code"),
        message=_required_string(data, "message"),
    )


def _queued_inputs(
    value: object, label: str = "queued inputs"
) -> tuple[QueuedInput, ...]:
    if not isinstance(value, list):
        raise TypeError(f"{label} must be a list")
    return tuple(_queued_input(item) for item in value)


def _queued_input(value: object) -> QueuedInput:
    data = require_json_object(value, "queued input")
    validate_object_keys(data, "queued input", required={"id", "text", "position"})
    return QueuedInput(
        id=_required_int(data, "id"),
        text=_required_string(data, "text"),
        position=_required_int(data, "position"),
    )


def _queued_input_groups(value: object) -> QueuedInputs:
    data = require_json_object(value, "queued input groups")
    validate_object_keys(
        data,
        "queued input groups",
        required={"items", "paused"},
    )
    paused = data["paused"]
    if not isinstance(paused, bool):
        raise TypeError("queued inputs paused flag must be a bool")
    return QueuedInputs(
        items=_queued_inputs(data["items"], "queued pending inputs"),
        paused=paused,
    )


def _parse_usage(value: object) -> SessionUsage:
    from ._models import parse_session_usage

    return parse_session_usage(value)
