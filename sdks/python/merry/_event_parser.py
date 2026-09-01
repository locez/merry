"""Decode native runtime event JSON into typed Python events."""

from __future__ import annotations

from collections.abc import Mapping
from enum import Enum
from typing import TypeVar

from ._event_payloads import (
    AssistantMessageDeltaPayload,
    AssistantMessagePayload,
    ClosedPayload,
    CompactionCompletedPayload,
    CompactionStartedPayload,
    EventPayloadValue,
    EvidenceReferencedPayload,
    FinalOutputRecordedPayload,
    InteractiveRunStateChangedPayload,
    ModelRetryAttemptStartedPayload,
    ModelRetryExhaustedPayload,
    ModelRetryScheduledPayload,
    PlanAttemptFinishedPayload,
    PlanAttemptProgressReportedPayload,
    PlanDirectiveUpdatedPayload,
    PlanLeaseStartedPayload,
    PlanNodeReadyPayload,
    PlanPhaseChangedPayload,
    PlanProgressReviewRequestedPayload,
    PlanProgressUpdatedPayload,
    PlanUpdatedPayload,
    QueuedInputAcceptedPayload,
    QueuedInputsChangedPayload,
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
    ToolCallBatchStartedPayload,
    ToolCallFinishedPayload,
    ToolCallStartedPayload,
    UnknownEventPayload,
    UsageUpdatedPayload,
)
from ._event_types import (
    ArtifactReference,
    EventDiagnostic,
    EventSource,
    EventType,
    EvidenceReference,
    InteractiveRunState,
    QueuedInput,
    QueuedInputLane,
    QueuedInputs,
    RawEventData,
    RuntimeToolCall,
    RuntimeToolCallBatch,
    RuntimeToolOutput,
    RuntimeToolOutputKind,
    RuntimeToolResult,
    RuntimeToolResultStatus,
    SubagentStatus,
)
from ._events import Event
from ._json import (
    JsonObject,
    JsonValue,
    require_json_object,
    validate_object_keys,
)
from ._models import SessionUsage

EnumT = TypeVar("EnumT", bound=Enum)


def _event_fields(
    *fields: str, optional: tuple[str, ...] = ()
) -> tuple[frozenset[str], frozenset[str]]:
    return frozenset(("type", *fields)), frozenset(optional)


_EVENT_FIELDS: dict[EventType, tuple[frozenset[str], frozenset[str]]] = {
    EventType.SESSION_STARTED: _event_fields("source"),
    EventType.STEP_STARTED: _event_fields("source"),
    EventType.STEP_COMPLETED: _event_fields("source"),
    EventType.COMPACTION_STARTED: _event_fields("source"),
    EventType.COMPACTION_COMPLETED: _event_fields(
        "checkpoint_id", "covered_history_item_count", "source"
    ),
    EventType.USAGE_UPDATED: _event_fields("usage", "source"),
    EventType.ASSISTANT_MESSAGE: _event_fields("text", "artifact", "source"),
    EventType.ASSISTANT_MESSAGE_DELTA: _event_fields("delta", "source"),
    EventType.TOOL_CALL_STARTED: _event_fields("call", "source"),
    EventType.TOOL_CALL_BATCH_STARTED: _event_fields("batch", "source"),
    EventType.TOOL_CALL_FINISHED: _event_fields("result", "output", "source"),
    EventType.FINAL_OUTPUT_RECORDED: _event_fields("call_id", "artifact", "source"),
    EventType.MODEL_RETRY_ATTEMPT_STARTED: _event_fields(
        "attempt", "max_attempts", "source"
    ),
    EventType.MODEL_RETRY_SCHEDULED: _event_fields(
        "attempt", "next_attempt", "max_attempts", "delay_ms", "error_kind", "source"
    ),
    EventType.MODEL_RETRY_EXHAUSTED: _event_fields(
        "attempts_run", "max_attempts", "error_kind", "source"
    ),
    EventType.EVIDENCE_REFERENCED: _event_fields("evidence", "source"),
    EventType.SKILL_USED: _event_fields(
        "skill_name", "skill_md_path", "tool_call_id", "artifact", "source"
    ),
    EventType.SUBAGENT_SPAWNED: _event_fields(
        "agent_id", "task_id", "task_anchor", "source"
    ),
    EventType.SUBAGENT_STARTED: _event_fields("agent_id", "task_id", "source"),
    EventType.SUBAGENT_STATUS_CHANGED: _event_fields(
        "agent_id", "task_id", "status", "source"
    ),
    EventType.SUBAGENT_COMPLETED: _event_fields(
        "agent_id", "task_id", "summary", "output_paths", "changed_paths", "source"
    ),
    EventType.SUBAGENT_FAILED: _event_fields(
        "agent_id", "task_id", "diagnostic", "source"
    ),
    EventType.SUBAGENT_CANCELLED: _event_fields(
        "agent_id", "task_id", "diagnostic", "source"
    ),
    EventType.PLAN_UPDATED: _event_fields("snapshot", "summary", "source"),
    EventType.PLAN_PHASE_CHANGED: _event_fields("plan_id", "phase", "source"),
    EventType.PLAN_NODE_READY: _event_fields(
        "plan_id", "node_id", "node_revision", "source"
    ),
    EventType.PLAN_LEASE_STARTED: _event_fields("lease", "source"),
    EventType.PLAN_PROGRESS_UPDATED: _event_fields("progress", "source"),
    EventType.PLAN_PROGRESS_REVIEW_REQUESTED: _event_fields(
        "plan_id", "attempt_id", "reason", "source"
    ),
    EventType.PLAN_ATTEMPT_PROGRESS_REPORTED: _event_fields("progress", "source"),
    EventType.PLAN_DIRECTIVE_UPDATED: _event_fields("directive", "source"),
    EventType.PLAN_ATTEMPT_FINISHED: _event_fields("attempt", "source"),
    EventType.RUN_FAILED: _event_fields("diagnostic", "source"),
    EventType.RUN_CANCELLED: _event_fields("diagnostic", "source"),
    EventType.INTERACTIVE_RUN_STATE_CHANGED: _event_fields("state"),
    EventType.QUEUED_INPUT_ACCEPTED: _event_fields("lane", "inputs"),
    EventType.QUEUED_INPUTS_CHANGED: _event_fields("inputs"),
    EventType.CLOSED: _event_fields(),
}


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

    required, optional = _EVENT_FIELDS[event_type]
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
        case EventType.ASSISTANT_MESSAGE:
            return AssistantMessagePayload(
                _event_text(data, "text"),
                _artifact(data["artifact"]),
                _source(data),
            )
        case EventType.ASSISTANT_MESSAGE_DELTA:
            return AssistantMessageDeltaPayload(
                _event_text(data, "delta"), _source(data)
            )
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
                _artifact(data["artifact"]),
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
        case EventType.EVIDENCE_REFERENCED:
            return EvidenceReferencedPayload(_evidence(data["evidence"]), _source(data))
        case EventType.SKILL_USED:
            return SkillUsedPayload(
                _required_string(data, "skill_name"),
                _required_string(data, "skill_md_path"),
                _required_string(data, "tool_call_id"),
                _artifact(data["artifact"]),
                _source(data),
            )
        case EventType.SUBAGENT_SPAWNED:
            return SubagentSpawnedPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _required_string(data, "task_anchor"),
                _source(data),
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
            return SubagentCompletedPayload(
                _required_string(data, "agent_id"),
                _required_string(data, "task_id"),
                _required_string(data, "summary"),
                _required_strings(data["output_paths"], "output paths"),
                _required_strings(data["changed_paths"], "changed paths"),
                _source(data),
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
        case EventType.PLAN_UPDATED:
            return PlanUpdatedPayload(
                RawEventData.from_value(data["snapshot"], "plan snapshot"),
                RawEventData.from_value(data["summary"], "plan revision summary"),
                _source(data),
            )
        case EventType.PLAN_PHASE_CHANGED:
            return PlanPhaseChangedPayload(
                _required_string(data, "plan_id"),
                _required_string(data, "phase"),
                _source(data),
            )
        case EventType.PLAN_NODE_READY:
            return PlanNodeReadyPayload(
                _required_string(data, "plan_id"),
                _required_string(data, "node_id"),
                _required_int(data, "node_revision"),
                _source(data),
            )
        case EventType.PLAN_LEASE_STARTED:
            return PlanLeaseStartedPayload(
                RawEventData.from_value(data["lease"], "plan lease"), _source(data)
            )
        case EventType.PLAN_PROGRESS_UPDATED:
            return PlanProgressUpdatedPayload(
                RawEventData.from_value(data["progress"], "plan progress"),
                _source(data),
            )
        case EventType.PLAN_PROGRESS_REVIEW_REQUESTED:
            return PlanProgressReviewRequestedPayload(
                _required_string(data, "plan_id"),
                _required_string(data, "attempt_id"),
                _required_string(data, "reason"),
                _source(data),
            )
        case EventType.PLAN_ATTEMPT_PROGRESS_REPORTED:
            return PlanAttemptProgressReportedPayload(
                RawEventData.from_value(data["progress"], "plan attempt progress"),
                _source(data),
            )
        case EventType.PLAN_DIRECTIVE_UPDATED:
            return PlanDirectiveUpdatedPayload(
                RawEventData.from_value(data["directive"], "plan directive"),
                _source(data),
            )
        case EventType.PLAN_ATTEMPT_FINISHED:
            return PlanAttemptFinishedPayload(
                RawEventData.from_value(data["attempt"], "plan attempt"),
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
            return QueuedInputAcceptedPayload(
                _enum_value(QueuedInputLane, data["lane"], "queued input lane"),
                _queued_inputs(data["inputs"]),
            )
        case EventType.QUEUED_INPUTS_CHANGED:
            return QueuedInputsChangedPayload(_queued_input_groups(data["inputs"]))
        case EventType.CLOSED:
            return ClosedPayload()
        case EventType.UNKNOWN:
            return UnknownEventPayload(raw_type, RawEventData(data))


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


def _artifact(value: object) -> ArtifactReference:
    data = require_json_object(value, "artifact reference")
    validate_object_keys(
        data,
        "artifact reference",
        required={"id", "kind", "label"},
    )
    return ArtifactReference(
        id=_required_string(data, "id"),
        kind=_required_string(data, "kind"),
        label=_optional_string(data.get("label")),
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
        required={"call_id", "status", "artifact", "diagnostic"},
    )
    return RuntimeToolResult(
        call_id=_required_string(data, "call_id"),
        status=_enum_value(
            RuntimeToolResultStatus, data["status"], "tool result status"
        ),
        artifact=_artifact(data["artifact"]),
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


def _evidence(value: object) -> EvidenceReference:
    data = require_json_object(value, "evidence reference")
    validate_object_keys(
        data,
        "evidence reference",
        required={"artifact_id", "locator"},
    )
    return EvidenceReference(
        artifact_id=_required_string(data, "artifact_id"),
        locator=RawEventData.from_value(data["locator"], "evidence locator"),
    )


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
    validate_object_keys(data, "queued input", required={"text", "lane", "position"})
    return QueuedInput(
        text=_required_string(data, "text"),
        lane=_enum_value(QueuedInputLane, data["lane"], "queued input lane"),
        position=_required_int(data, "position"),
    )


def _queued_input_groups(value: object) -> QueuedInputs:
    data = require_json_object(value, "queued input groups")
    validate_object_keys(
        data,
        "queued input groups",
        required={"next", "suspended", "backlog"},
    )
    return QueuedInputs(
        next=_queued_inputs(data["next"], "queued next inputs"),
        suspended=_queued_inputs(data["suspended"], "queued suspended inputs"),
        backlog=_queued_inputs(data["backlog"], "queued backlog inputs"),
    )


def _parse_usage(value: object) -> SessionUsage:
    from ._models import parse_session_usage

    return parse_session_usage(value)


def _enum_value(enum_type: type[EnumT], value: object, label: str) -> EnumT:
    if not isinstance(value, str):
        raise TypeError(f"{label} must be a string")
    try:
        return enum_type(value)
    except ValueError as error:
        raise TypeError(f"{label} is unsupported") from error


def _event_text(data: Mapping[str, JsonValue], key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"event field {key!r} must be a string")
    return value


def _required_string(data: Mapping[str, JsonValue], key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"event field {key!r} must be a string")
    if not value.strip():
        raise ValueError(f"event field {key!r} must not be blank")
    return value


def _optional_string(value: object) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise TypeError("optional event field must be a string or null")
    return value


def _required_int(data: Mapping[str, JsonValue], key: str) -> int:
    value = data[key]
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"event field {key!r} must be an integer")
    return value


def _required_strings(value: object, label: str) -> tuple[str, ...]:
    if not isinstance(value, list):
        raise TypeError(f"{label} must be a list")
    values: list[str] = []
    for item in value:
        if not isinstance(item, str):
            raise TypeError(f"{label} must contain only strings")
        values.append(item)
    return tuple(values)
