"""Provider-neutral value types shared by the Python event contract."""

from __future__ import annotations

import json
from dataclasses import dataclass
from enum import Enum
from typing import ClassVar

from ._json import (
    JsonObject,
    _contains_control,
    require_json_object,
    require_json_value,
)


class EventType(str, Enum):
    """Stable runtime event discriminant projected from Rust."""

    SESSION_STARTED = "session_started"
    STEP_STARTED = "step_started"
    STEP_COMPLETED = "step_completed"
    COMPACTION_STARTED = "compaction_started"
    COMPACTION_COMPLETED = "compaction_completed"
    USAGE_UPDATED = "usage_updated"
    ASSISTANT_MESSAGE = "assistant_message"
    ASSISTANT_MESSAGE_DELTA = "assistant_message_delta"
    TOOL_CALL_STARTED = "tool_call_started"
    TOOL_CALL_BATCH_STARTED = "tool_call_batch_started"
    TOOL_CALL_FINISHED = "tool_call_finished"
    FINAL_OUTPUT_RECORDED = "final_output_recorded"
    MODEL_RETRY_ATTEMPT_STARTED = "model_retry_attempt_started"
    MODEL_RETRY_SCHEDULED = "model_retry_scheduled"
    MODEL_RETRY_EXHAUSTED = "model_retry_exhausted"
    EVIDENCE_REFERENCED = "evidence_referenced"
    SKILL_USED = "skill_used"
    SUBAGENT_SPAWNED = "subagent_spawned"
    SUBAGENT_STARTED = "subagent_started"
    SUBAGENT_STATUS_CHANGED = "subagent_status_changed"
    SUBAGENT_COMPLETED = "subagent_completed"
    SUBAGENT_FAILED = "subagent_failed"
    SUBAGENT_CANCELLED = "subagent_cancelled"
    PLAN_UPDATED = "plan_updated"
    PLAN_PHASE_CHANGED = "plan_phase_changed"
    PLAN_NODE_READY = "plan_node_ready"
    PLAN_LEASE_STARTED = "plan_lease_started"
    PLAN_PROGRESS_UPDATED = "plan_progress_updated"
    PLAN_PROGRESS_REVIEW_REQUESTED = "plan_progress_review_requested"
    PLAN_ATTEMPT_PROGRESS_REPORTED = "plan_attempt_progress_reported"
    PLAN_DIRECTIVE_UPDATED = "plan_directive_updated"
    PLAN_ATTEMPT_FINISHED = "plan_attempt_finished"
    RUN_FAILED = "run_failed"
    RUN_CANCELLED = "run_cancelled"
    INTERACTIVE_RUN_STATE_CHANGED = "interactive_run_state_changed"
    QUEUED_INPUT_ACCEPTED = "queued_input_accepted"
    QUEUED_INPUTS_CHANGED = "queued_inputs_changed"
    CLOSED = "closed"
    UNKNOWN = "unknown"


@dataclass(frozen=True, slots=True)
class EventSource:
    """Journal position that produced a durable runtime event."""

    session_id: str
    sequence: int

    def __post_init__(self) -> None:
        _validate_identifier("event session id", self.session_id, 256)
        _validate_nonnegative("event sequence", self.sequence)


@dataclass(frozen=True, slots=True)
class RawEventData:
    """Explicit opaque JSON for a nested Rust contract not modeled in Python."""

    value: JsonObject

    @classmethod
    def from_value(cls, value: object, label: str) -> RawEventData:
        return cls(require_json_object(value, label))

    def __post_init__(self) -> None:
        object.__setattr__(
            self, "value", require_json_object(self.value, "raw event data")
        )


@dataclass(frozen=True, slots=True)
class EventDiagnostic:
    """Small model-visible diagnostic attached to a runtime event."""

    code: str
    message: str

    def __post_init__(self) -> None:
        _validate_identifier("event diagnostic code", self.code, 128)
        _validate_text("event diagnostic message", self.message, 4096)


@dataclass(frozen=True, slots=True)
class ArtifactReference:
    """Provider-neutral reference to a persisted runtime artifact."""

    id: str
    kind: str
    label: str | None

    def __post_init__(self) -> None:
        _validate_identifier("artifact id", self.id, 256)
        _validate_identifier("artifact kind", self.kind, 128)
        if self.label is not None:
            _validate_text("artifact label", self.label, 512)


@dataclass(frozen=True, slots=True)
class RuntimeToolCall:
    """Typed runtime view of a pending tool call."""

    id: str
    name: str
    arguments: JsonObject

    def __post_init__(self) -> None:
        _validate_identifier("runtime tool call id", self.id, 256)
        _validate_tool_name(self.name)
        object.__setattr__(
            self,
            "arguments",
            require_json_object(self.arguments, "runtime tool call arguments"),
        )


@dataclass(frozen=True, slots=True)
class RuntimeToolCallBatch:
    """Typed runtime view of an ordered tool call batch."""

    id: str
    calls: tuple[RuntimeToolCall, ...]

    def __post_init__(self) -> None:
        _validate_identifier("runtime tool call batch id", self.id, 256)
        if not isinstance(self.calls, tuple) or any(
            not isinstance(call, RuntimeToolCall) for call in self.calls
        ):
            raise TypeError(
                "runtime tool call batch calls must be a tuple of RuntimeToolCall values"
            )
        if not self.calls:
            raise ValueError("runtime tool call batch must contain calls")
        call_ids = [call.id for call in self.calls]
        if len(call_ids) != len(set(call_ids)):
            raise ValueError("runtime tool call batch ids must be unique")


class RuntimeToolResultStatus(str, Enum):
    """Status of a tool result recorded by Rust."""

    SUCCEEDED = "succeeded"
    FAILED = "failed"


class RuntimeToolOutputKind(str, Enum):
    """Content kind for an artifact-backed tool result."""

    TEXT = "text"
    JSON = "json"


@dataclass(frozen=True, slots=True)
class RuntimeToolOutput:
    """Typed text or JSON tool output view."""

    kind: RuntimeToolOutputKind
    value: str

    def __post_init__(self) -> None:
        if not isinstance(self.kind, RuntimeToolOutputKind):
            raise TypeError("runtime tool output kind must be a RuntimeToolOutputKind")
        _validate_text("runtime tool output", self.value, 1_048_576, allow_blank=True)
        if self.kind is RuntimeToolOutputKind.JSON:
            try:
                decoded: object = json.loads(self.value)
            except json.JSONDecodeError as error:
                raise ValueError(
                    "runtime JSON tool output must be valid JSON"
                ) from error
            require_json_value(decoded, "runtime JSON tool output")


@dataclass(frozen=True, slots=True)
class RuntimeToolResult:
    """Typed artifact-backed tool result view."""

    call_id: str
    status: RuntimeToolResultStatus
    artifact: ArtifactReference
    diagnostic: EventDiagnostic | None

    def __post_init__(self) -> None:
        _validate_identifier("runtime tool result call id", self.call_id, 256)
        if not isinstance(self.status, RuntimeToolResultStatus):
            raise TypeError(
                "runtime tool result status must be a RuntimeToolResultStatus"
            )
        if not isinstance(self.artifact, ArtifactReference):
            raise TypeError("runtime tool result artifact must be an ArtifactReference")
        if self.diagnostic is not None and not isinstance(
            self.diagnostic, EventDiagnostic
        ):
            raise TypeError(
                "runtime tool result diagnostic must be an EventDiagnostic or None"
            )
        if (
            self.status is RuntimeToolResultStatus.SUCCEEDED
            and self.diagnostic is not None
        ):
            raise ValueError(
                "successful runtime tool results must not have a diagnostic"
            )
        if self.status is RuntimeToolResultStatus.FAILED and self.diagnostic is None:
            raise ValueError("failed runtime tool results must have a diagnostic")


@dataclass(frozen=True, slots=True)
class EvidenceReference:
    """Typed outer shape of exact evidence, with an opaque locator payload."""

    artifact_id: str
    locator: RawEventData

    def __post_init__(self) -> None:
        _validate_identifier("evidence artifact id", self.artifact_id, 256)
        if not isinstance(self.locator, RawEventData):
            raise TypeError("evidence locator must be RawEventData")


class SubagentStatus(str, Enum):
    """Lifecycle status of a runtime-owned subagent."""

    QUEUED = "queued"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class InteractiveRunState(str, Enum):
    """Interactive driver state projected through the runtime event stream."""

    WAITING_FOR_INPUT = "waiting_for_input"
    RUNNING_MODEL = "running_model"
    RUNNING_TOOL = "running_tool"
    INTERRUPTING = "interrupting"
    CLOSED = "closed"


class QueuedInputLane(str, Enum):
    """Queue lane for an interactive input item."""

    NEXT = "next"
    SUSPENDED = "suspended"
    BACKLOG = "backlog"


@dataclass(frozen=True, slots=True)
class QueuedInput:
    """Typed view of one queued interactive input."""

    text: str
    lane: QueuedInputLane
    position: int

    def __post_init__(self) -> None:
        _validate_text("queued input text", self.text, 1_048_576)
        if not isinstance(self.lane, QueuedInputLane):
            raise TypeError("queued input lane must be a QueuedInputLane")
        _validate_nonnegative("queued input position", self.position)


@dataclass(frozen=True, slots=True)
class QueuedInputs:
    """Typed view of all queued interactive inputs."""

    next: tuple[QueuedInput, ...]
    suspended: tuple[QueuedInput, ...]
    backlog: tuple[QueuedInput, ...]

    def __post_init__(self) -> None:
        for inputs in (self.next, self.suspended, self.backlog):
            if not isinstance(inputs, tuple) or any(
                not isinstance(item, QueuedInput) for item in inputs
            ):
                raise TypeError(
                    "queued input groups must be tuples of QueuedInput values"
                )
        for lane, inputs in (
            (QueuedInputLane.NEXT, self.next),
            (QueuedInputLane.SUSPENDED, self.suspended),
            (QueuedInputLane.BACKLOG, self.backlog),
        ):
            if any(item.lane is not lane for item in inputs):
                raise ValueError("queued input lane does not match its group")


class EventPayload:
    """Base class for one typed runtime event payload."""

    event_type: ClassVar[EventType]


class SourcedEventPayload(EventPayload):
    """Base for event payloads anchored to a durable journal position."""

    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)


def _validate_identifier(name: str, value: str, maximum: int) -> None:
    _validate_text(name, value, maximum)
    if value != value.strip():
        raise ValueError(f"{name} must be trimmed")


def _validate_text(
    name: str, value: str, maximum: int, *, allow_blank: bool = False
) -> None:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be a string")
    if not allow_blank and not value.strip():
        raise ValueError(f"{name} must not be blank")
    if len(value) > maximum:
        raise ValueError(f"{name} is too long")
    if _contains_control(value):
        raise ValueError(f"{name} must not contain control characters")


def _validate_tool_name(value: str) -> None:
    _validate_identifier("runtime tool name", value, 64)
    first = value[0]
    if not (first.isascii() and (first.isalpha() or first == "_")):
        raise ValueError(
            "runtime tool name must start with an ASCII letter or underscore"
        )
    if not all(
        character.isascii() and (character.isalnum() or character in {"_", "-"})
        for character in value
    ):
        raise ValueError(
            "runtime tool name must use ASCII letters, digits, underscore, or hyphen"
        )


def _validate_nonnegative(name: str, value: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{name} must be an integer")
    if value < 0 or value > (1 << 64) - 1:
        raise ValueError(f"{name} must fit in an unsigned 64-bit integer")


def _validate_event_source(value: object) -> None:
    if not isinstance(value, EventSource):
        raise TypeError("event source must be an EventSource")
