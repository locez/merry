"""Conversion between the native JSON protocol and typed Python models."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, TypeVar

from pydantic import BaseModel

from ._errors import output_decode_error
from ._event_parser import parse_event
from ._event_types import ArtifactReference, EventDiagnostic
from ._events import Event
from ._json import (
    JsonObject,
    require_json_object,
    require_json_value,
    validate_object_keys,
)
from ._models import (
    BlockedReason,
    FinalOutputRecord,
    RunResult,
    RunStatus,
    SessionUsage,
    ToolCall,
    ToolCallBatch,
    ToolSubmission,
)

if TYPE_CHECKING:
    from ._run import AgentRun

OutputT = TypeVar("OutputT", bound=BaseModel)


def parse_message(
    payload: str, run: AgentRun[OutputT]
) -> Event | ToolCallBatch[OutputT]:
    decoded: object = _decode_json(payload, "native run message")
    data = require_json_object(decoded, "native run message")
    kind = _required_string(data, "kind")
    if kind == "event":
        validate_object_keys(data, "native event message", required={"kind", "event"})
        event_value = data["event"]
        return parse_event(event_value)
    if kind == "tool_invocations":
        validate_object_keys(
            data,
            "native tool invocation message",
            required={"kind", "id", "invocations"},
        )
        return _parse_batch(data, run)
    raise TypeError(f"native run message kind is unsupported: {kind}")


def _parse_batch(data: JsonObject, run: AgentRun[OutputT]) -> ToolCallBatch[OutputT]:
    validate_object_keys(
        data,
        "native tool invocation batch",
        required={"kind", "id", "invocations"},
    )
    batch_id = _required_string(data, "id")
    raw_invocations = data["invocations"]
    if not isinstance(raw_invocations, list) or not raw_invocations:
        raise TypeError("native tool invocation batch must contain invocations")
    invocations: list[ToolCall] = []
    for raw_invocation in raw_invocations:
        invocation = require_json_object(raw_invocation, "native tool invocation")
        validate_object_keys(
            invocation,
            "native tool invocation",
            required={"id", "name", "arguments"},
        )
        arguments = require_json_object(
            invocation["arguments"], "tool invocation arguments"
        )
        invocations.append(
            ToolCall(
                id=_required_string(invocation, "id"),
                name=_required_string(invocation, "name"),
                arguments=arguments,
            )
        )
    return ToolCallBatch(id=batch_id, invocations=tuple(invocations), _run=run)


def parse_submission(payload: str) -> ToolSubmission:
    data = require_json_object(
        _decode_json(payload, "native tool submission"), "native tool submission"
    )
    validate_object_keys(data, "native tool submission", required={"status"})
    status = _required_string(data, "status")
    try:
        return ToolSubmission(status)
    except ValueError as error:
        raise TypeError(
            f"native tool submission status is unsupported: {status}"
        ) from error


def parse_run_result(
    payload: str,
    output_model: type[OutputT] | None,
) -> RunResult[OutputT]:
    data = require_json_object(
        _decode_json(payload, "native run result"), "native run result"
    )
    validate_object_keys(
        data,
        "native run result",
        required={
            "status",
            "events",
            "model_turns_run",
            "final_output",
            "final_output_json",
            "session_usage",
        },
    )
    status_data = require_json_object(data["status"], "native run status")
    status_kind = _required_string(status_data, "kind")
    if status_kind in {"failed", "cancelled"}:
        validate_object_keys(
            status_data,
            "native failed run status",
            required={"kind", "diagnostic"},
        )
    elif status_kind == "blocked":
        validate_object_keys(
            status_data,
            "native blocked run status",
            required={"kind", "reason"},
        )
    else:
        validate_object_keys(
            status_data, "native completed run status", required={"kind"}
        )
    try:
        status = RunStatus(status_kind)
    except ValueError as error:
        raise TypeError(f"native run status is unsupported: {status_kind}") from error

    raw_events = data["events"]
    if not isinstance(raw_events, list):
        raise TypeError("native run result events must be a list")
    events = tuple(parse_event(item) for item in raw_events)
    final_output_value: object = data["final_output"]
    final_output = _optional_string(final_output_value, "final_output")
    final_output_record = _parse_final_output_json(data["final_output_json"])
    final_output_json = (
        None if final_output_record is None else final_output_record.value
    )
    structured_output: OutputT | None = None
    if output_model is not None and final_output_json is not None:
        if not isinstance(final_output_json, dict):
            raise TypeError("native final_output_json must be an object")
        try:
            structured_output = output_model.model_validate(final_output_json)
        except ValueError:
            raise output_decode_error() from None

    usage_value: object = data["session_usage"]
    session_usage = None if usage_value is None else _parse_usage(usage_value)
    diagnostic = _parse_diagnostic(status_data)
    blocked_reason = _parse_blocked_reason(status_data)
    return RunResult(
        status=status,
        events=events,
        model_turns_run=_required_int(data, "model_turns_run"),
        final_output=final_output,
        final_output_json=final_output_json,
        structured_output=structured_output,
        session_usage=session_usage,
        diagnostic=diagnostic,
        blocked_reason=blocked_reason,
        final_output_record=final_output_record,
    )


def _parse_final_output_json(value: object) -> FinalOutputRecord | None:
    if value is None:
        return None
    data = require_json_object(value, "native final output")
    validate_object_keys(
        data,
        "native final output",
        required={"call_id", "artifact", "json"},
    )
    artifact_data = require_json_object(data["artifact"], "final output artifact")
    validate_object_keys(
        artifact_data,
        "final output artifact",
        required={"id", "kind", "label"},
    )
    artifact = ArtifactReference(
        id=_required_string(artifact_data, "id"),
        kind=_required_string(artifact_data, "kind"),
        label=_optional_string(artifact_data["label"], "artifact label"),
    )
    json_value: object = data["json"]
    if not isinstance(json_value, str):
        raise TypeError("native final output JSON field must be a string")
    try:
        decoded: object = json.loads(json_value)
    except json.JSONDecodeError as error:
        raise TypeError("native final output contains invalid JSON") from error
    return FinalOutputRecord(
        call_id=_required_string(data, "call_id"),
        artifact=artifact,
        value=require_json_value(decoded, "native final output JSON"),
    )


def _parse_diagnostic(status: JsonObject) -> EventDiagnostic | None:
    if "diagnostic" not in status:
        return None
    value: object = status["diagnostic"]
    data = require_json_object(value, "native diagnostic")
    validate_object_keys(data, "native diagnostic", required={"code", "message"})
    return EventDiagnostic(
        code=_required_string(data, "code"),
        message=_required_string(data, "message"),
    )


def _parse_blocked_reason(status: JsonObject) -> BlockedReason | None:
    if "reason" not in status:
        return None
    reason = require_json_object(status["reason"], "native blocked reason")
    kind = _required_string(reason, "kind")
    fields: dict[str, frozenset[str]] = {
        "max_model_turns_reached": frozenset({"kind", "max_model_turns"}),
        "multiple_pending_tool_calls": frozenset({"kind", "pending_count"}),
        "step_completed_with_pending_tool_call": frozenset({"kind", "pending_count"}),
        "step_ended_without_terminal_event": frozenset({"kind"}),
        "final_output_tool_not_called": frozenset({"kind"}),
        "bridge_tool_call_requested": frozenset({"kind", "call_id", "tool_name"}),
    }
    try:
        required = fields[kind]
    except KeyError as error:
        raise TypeError(f"native blocked reason is unsupported: {kind}") from error
    validate_object_keys(reason, "native blocked reason", required=required)
    if "pending_count" in reason:
        _required_int(reason, "pending_count")
    if "max_model_turns" in reason:
        _required_int(reason, "max_model_turns")
    if "call_id" in reason:
        _required_string(reason, "call_id")
        _required_string(reason, "tool_name")
    data: JsonObject = {key: value for key, value in reason.items() if key != "kind"}
    return BlockedReason(kind=kind, data=data)


def _parse_usage(value: object) -> SessionUsage:
    from ._models import parse_session_usage

    return parse_session_usage(value)


def _decode_json(payload: str, label: str) -> object:
    try:
        return json.loads(payload)
    except json.JSONDecodeError as error:
        raise TypeError(f"{label} is not valid JSON") from error


def _required_string(data: JsonObject, key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"native protocol field {key!r} must be a string")
    return value


def _optional_string(value: object, key: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise TypeError(f"native protocol field {key!r} must be a string or null")
    return value


def _required_int(data: JsonObject, key: str) -> int:
    value = data[key]
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"native protocol field {key!r} must be an integer")
    return value
