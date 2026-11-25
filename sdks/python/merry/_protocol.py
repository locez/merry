"""Conversion between the native JSON protocol and typed Python models."""

from __future__ import annotations

import json
from typing import TypeVar

from pydantic import BaseModel

from ._config import (
    CompactionOutcome,
    GenerationConfig,
    ParallelToolCalls,
    ServiceTier,
)
from ._errors import output_decode_error
from ._event_parser import parse_event
from ._event_types import EventDiagnostic, QueuedInput, QueuedInputs
from ._events import Event
from ._hooks import (
    AfterModelStepContext,
    AfterToolCallContext,
    BeforeModelStepContext,
    BeforeToolCallContext,
    HookInvocation,
    HookPoint,
    HookToolStatus,
    _HookSubmitter,
)
from ._json import (
    JsonObject,
    require_json_object,
    require_json_value,
    validate_object_keys,
)
from ._models import (
    BlockedReason,
    FinalOutputRecord,
    HookStopInfo,
    JsonContent,
    RunResult,
    RunStatus,
    SessionUsage,
    TextContent,
    ToolCall,
    ToolCallBatch,
    ToolContent,
    ToolSubmission,
    _BatchSubmitter,
)

OutputT = TypeVar("OutputT", bound=BaseModel)


def parse_message(
    payload: str,
    submitter: _BatchSubmitter,
    hook_submitter: _HookSubmitter | None = None,
) -> Event | ToolCallBatch | HookInvocation:
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
        return _parse_batch(data, submitter)
    if kind == "hook_invocation":
        validate_object_keys(
            data,
            "native hook invocation message",
            required={"kind", "id", "request"},
        )
        if hook_submitter is None:
            raise TypeError(
                "hook_submitter is required to decode native hook_invocation messages"
            )
        return _parse_hook_invocation(data, hook_submitter)
    raise TypeError(f"native run message kind is unsupported: {kind}")


def _parse_batch(data: JsonObject, submitter: _BatchSubmitter) -> ToolCallBatch:
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
    return ToolCallBatch(
        id=batch_id,
        invocations=tuple(invocations),
        _submitter=submitter,
    )


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


def parse_interactive_input_item(payload: str) -> tuple[int, str]:
    data = require_json_object(
        _decode_json(payload, "native interactive input item"),
        "native interactive input item",
    )
    validate_object_keys(
        data,
        "native interactive input item",
        required={"id", "text"},
    )
    return _required_int(data, "id"), _required_string(data, "text")


def parse_interactive_snapshot(payload: str) -> QueuedInputs:
    data = require_json_object(
        _decode_json(payload, "native interactive snapshot"),
        "native interactive snapshot",
    )
    validate_object_keys(
        data,
        "native interactive snapshot",
        required={"items", "paused"},
    )
    paused = data["paused"]
    if not isinstance(paused, bool):
        raise TypeError("native interactive snapshot paused flag must be a bool")
    raw_items = data["items"]
    if not isinstance(raw_items, list):
        raise TypeError("native interactive snapshot items must be a list")
    items: list[QueuedInput] = []
    for entry in raw_items:
        item_data = require_json_object(entry, "native queued input")
        validate_object_keys(
            item_data,
            "native queued input",
            required={"id", "text", "position"},
        )
        items.append(
            QueuedInput(
                id=_required_int(item_data, "id"),
                text=_required_string(item_data, "text"),
                position=_required_int(item_data, "position"),
            )
        )
    return QueuedInputs(items=tuple(items), paused=paused)


def parse_compaction_outcome(payload: str) -> CompactionOutcome:
    data = require_json_object(
        _decode_json(payload, "native compaction outcome"),
        "native compaction outcome",
    )
    validate_object_keys(
        data,
        "native compaction outcome",
        required={
            "checkpoint_id",
            "covered_model_turn_count",
            "covered_history_item_count",
            "retained_history_item_count",
        },
    )
    return CompactionOutcome(
        checkpoint_id=_required_string(data, "checkpoint_id"),
        covered_model_turn_count=_required_int(data, "covered_model_turn_count"),
        covered_history_item_count=_required_int(data, "covered_history_item_count"),
        retained_history_item_count=_required_int(data, "retained_history_item_count"),
    )


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
    elif status_kind == "stopped_by_hook":
        validate_object_keys(
            status_data,
            "native hook-stopped run status",
            required={"kind", "hook_point", "reason"},
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
    hook_stop = _parse_hook_stop(status_data)
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
        hook_stop=hook_stop,
    )


def _parse_hook_stop(status: JsonObject) -> HookStopInfo | None:
    if status.get("kind") != "stopped_by_hook":
        return None
    return HookStopInfo(
        hook_point=_required_string(status, "hook_point"),
        reason=_required_string(status, "reason"),
    )


def _parse_hook_invocation(
    data: JsonObject,
    submitter: _HookSubmitter,
) -> HookInvocation:
    invocation_id = _required_string(data, "id")
    request = require_json_object(data["request"], "native hook request")
    point_str = _required_string(request, "point")
    try:
        point = HookPoint(point_str)
    except ValueError as error:
        raise TypeError(f"native hook point is unsupported: {point_str}") from error

    if point == HookPoint.BEFORE_MODEL_STEP:
        usage_val = request.get("session_usage")
        session_usage = None if usage_val is None else _parse_usage(usage_val)
        generation = _parse_generation_config(request["generation_config"])
        return HookInvocation(
            id=invocation_id,
            point=point,
            context=BeforeModelStepContext(
                session_id=_required_string(request, "session_id"),
                model_turn=_required_int(request, "model_turn"),
                max_model_turns=_required_int(request, "max_model_turns"),
                session_usage=session_usage,
                generation=generation,
            ),
            _submitter=submitter,
        )

    if point == HookPoint.AFTER_MODEL_STEP:
        usage_val = request.get("session_usage")
        session_usage = None if usage_val is None else _parse_usage(usage_val)
        raw_tools = request.get("pending_tool_names")
        if not isinstance(raw_tools, list):
            raise TypeError("native hook pending_tool_names must be a list of strings")
        pending_tool_names: list[str] = []
        for item in raw_tools:
            if not isinstance(item, str):
                raise TypeError(
                    "native hook pending_tool_names must be a list of strings"
                )
            pending_tool_names.append(item)
        is_final = request.get("is_final_turn")
        if type(is_final) is not bool:
            raise TypeError("native hook is_final_turn must be a boolean")
        return HookInvocation(
            id=invocation_id,
            point=point,
            context=AfterModelStepContext(
                session_id=_required_string(request, "session_id"),
                model_turn=_required_int(request, "model_turn"),
                max_model_turns=_required_int(request, "max_model_turns"),
                assistant_text=_optional_string(
                    request.get("assistant_text"), "assistant_text"
                ),
                pending_tool_names=tuple(pending_tool_names),
                is_final_turn=is_final,
                session_usage=session_usage,
            ),
            _submitter=submitter,
        )

    if point == HookPoint.BEFORE_TOOL_CALL:
        is_bridge = request.get("is_bridge_tool")
        if type(is_bridge) is not bool:
            raise TypeError("native hook is_bridge_tool must be a boolean")
        return HookInvocation(
            id=invocation_id,
            point=point,
            context=BeforeToolCallContext(
                session_id=_required_string(request, "session_id"),
                model_turn=_required_int(request, "model_turn"),
                call_id=_required_string(request, "call_id"),
                tool_name=_required_string(request, "tool_name"),
                arguments=require_json_object(
                    request["arguments"], "hook tool arguments"
                ),
                is_bridge_tool=is_bridge,
            ),
            _submitter=submitter,
        )

    is_bridge = request.get("is_bridge_tool")
    if type(is_bridge) is not bool:
        raise TypeError("native hook is_bridge_tool must be a boolean")
    status_raw = _required_string(request, "status")
    status: HookToolStatus
    if status_raw == "succeeded":
        status = "succeeded"
    elif status_raw == "failed":
        status = "failed"
    else:
        raise TypeError(f"unsupported hook tool status: {status_raw}")
    diag_raw = request.get("diagnostic")
    diagnostic: EventDiagnostic | None = None
    if diag_raw is not None:
        diag_obj = require_json_object(diag_raw, "hook tool diagnostic")
        diagnostic = EventDiagnostic(
            code=_required_string(diag_obj, "code"),
            message=_required_string(diag_obj, "message"),
        )
    return HookInvocation(
        id=invocation_id,
        point=point,
        context=AfterToolCallContext(
            session_id=_required_string(request, "session_id"),
            model_turn=_required_int(request, "model_turn"),
            call_id=_required_string(request, "call_id"),
            tool_name=_required_string(request, "tool_name"),
            arguments=require_json_object(request["arguments"], "hook tool arguments"),
            status=status,
            output=_parse_tool_output(request["output"]),
            diagnostic=diagnostic,
            is_bridge_tool=is_bridge,
        ),
        _submitter=submitter,
    )


def _parse_tool_output(value: object) -> ToolContent:
    data = require_json_object(value, "hook tool output")
    kind = _required_string(data, "kind")
    if kind == "text":
        return TextContent(_required_string(data, "text"))
    if kind == "json":
        return JsonContent(_required_string(data, "json"))
    raise TypeError(f"unsupported tool output kind: {kind}")


def _parse_generation_config(value: object) -> GenerationConfig:
    data = require_json_object(value, "hook generation config")
    max_output_raw = data.get("max_output_tokens")
    max_output: int | None
    if max_output_raw is None:
        max_output = None
    elif type(max_output_raw) is int and max_output_raw >= 1:
        max_output = max_output_raw
    else:
        raise TypeError("generation max_output_tokens must be a positive int or None")
    parallel_raw = data.get("parallel_tool_calls")
    parallel = (
        ParallelToolCalls(parallel_raw)
        if isinstance(parallel_raw, str)
        else ParallelToolCalls.AUTO
    )
    reasoning_raw = data.get("reasoning_effort")
    reasoning = _optional_string(reasoning_raw, "reasoning_effort")
    tier_raw = data.get("service_tier")
    tier = ServiceTier(tier_raw) if isinstance(tier_raw, str) else None
    return GenerationConfig(
        max_output_tokens=max_output,
        parallel_tool_calls=parallel,
        reasoning_effort=reasoning,
        service_tier=tier,
    )


def _parse_final_output_json(value: object) -> FinalOutputRecord | None:
    if value is None:
        return None
    data = require_json_object(value, "native final output")
    validate_object_keys(
        data,
        "native final output",
        required={"call_id", "json"},
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
    if status.get("kind") != "blocked" or "reason" not in status:
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
