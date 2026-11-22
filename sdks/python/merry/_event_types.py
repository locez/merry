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
    MODEL_OUTPUT_RATE_UPDATED = "model_output_rate_updated"
    ASSISTANT_MESSAGE = "assistant_message"
    ASSISTANT_MESSAGE_DELTA = "assistant_message_delta"
    REASONING_DELTA = "reasoning_delta"
    TOOL_CALL_STARTED = "tool_call_started"
    TOOL_CALL_BATCH_STARTED = "tool_call_batch_started"
    TOOL_CALL_FINISHED = "tool_call_finished"
    FINAL_OUTPUT_RECORDED = "final_output_recorded"
    MODEL_RETRY_ATTEMPT_STARTED = "model_retry_attempt_started"
    MODEL_RETRY_SCHEDULED = "model_retry_scheduled"
    MODEL_RETRY_EXHAUSTED = "model_retry_exhausted"
    SKILL_USED = "skill_used"
    SUBAGENT_SPAWNED = "subagent_spawned"
    SUBAGENT_STARTED = "subagent_started"
    SUBAGENT_STATUS_CHANGED = "subagent_status_changed"
    SUBAGENT_COMPLETED = "subagent_completed"
    SUBAGENT_FAILED = "subagent_failed"
    SUBAGENT_CANCELLED = "subagent_cancelled"
    PLAN_MODE_CHANGED = "plan_mode_changed"
    PLAN_SAVED = "plan_saved"
    PLAN_APPROVAL_REQUESTED = "plan_approval_requested"
    PLAN_REVIEWED = "plan_reviewed"
    TODO_LIST_UPDATED = "todo_list_updated"
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
    """Content kind for a recorded tool result."""

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
        _validate_text(
            "runtime tool output",
            self.value,
            1_048_576,
            allow_blank=True,
            allow_newline_tab=True,
        )
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
    """Typed tool result metadata view."""

    call_id: str
    status: RuntimeToolResultStatus
    diagnostic: EventDiagnostic | None

    def __post_init__(self) -> None:
        _validate_identifier("runtime tool result call id", self.call_id, 256)
        if not isinstance(self.status, RuntimeToolResultStatus):
            raise TypeError(
                "runtime tool result status must be a RuntimeToolResultStatus"
            )
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


class SubagentStatus(str, Enum):
    """Lifecycle status of a runtime-owned subagent."""

    QUEUED = "queued"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    BLOCKED = "blocked"


class PlanModeState(str, Enum):
    """Session Plan Mode lifecycle state."""

    INACTIVE = "inactive"
    PENDING = "pending"
    ACTIVE = "active"
    EXIT_PENDING = "exit_pending"


class TodoStatus(str, Enum):
    """Status of one lightweight session todo item."""

    PENDING = "pending"
    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
    CANCELLED = "cancelled"


@dataclass(frozen=True, slots=True)
class TodoItem:
    """Typed view of one session checklist item."""

    id: str
    content: str
    status: TodoStatus

    def __post_init__(self) -> None:
        _validate_identifier("todo item id", self.id, 128)
        _validate_text("todo item content", self.content, 512)
        if not isinstance(self.status, TodoStatus):
            raise TypeError("todo item status must be a TodoStatus")


class InteractiveRunState(str, Enum):
    """Interactive driver state projected through the runtime event stream."""

    WAITING_FOR_INPUT = "waiting_for_input"
    RUNNING_MODEL = "running_model"
    RUNNING_TOOL = "running_tool"
    INTERRUPTING = "interrupting"
    CLOSED = "closed"


@dataclass(frozen=True, slots=True)
class QueuedInput:
    """Typed view of one pending interactive input."""

    id: int
    text: str
    position: int

    def __post_init__(self) -> None:
        _validate_nonnegative("queued input id", self.id)
        _validate_text(
            "queued input text", self.text, 1_048_576, allow_newline_tab=True
        )
        _validate_nonnegative("queued input position", self.position)


@dataclass(frozen=True, slots=True)
class QueuedInputs:
    """Typed view of the pending interactive input queue."""

    items: tuple[QueuedInput, ...]
    paused: bool

    def __post_init__(self) -> None:
        if not isinstance(self.items, tuple) or any(
            not isinstance(item, QueuedInput) for item in self.items
        ):
            raise TypeError("queued inputs must be a tuple of QueuedInput values")
        if not isinstance(self.paused, bool):
            raise TypeError("queued inputs paused flag must be a bool")


class EventPayload:
    """Base class for one typed runtime event payload."""

    event_type: ClassVar[EventType]


class SourcedEventPayload(EventPayload):
    """Base for event payloads anchored to a durable journal position.

    Subclasses declare ``source`` as a frozen dataclass field. The property
    keeps that field read-only in the type checker; a bare annotation here
    would be writable and reject the frozen override.
    """

    @property
    def source(self) -> EventSource:
        raise NotImplementedError

    def __post_init__(self) -> None:
        _validate_event_source(self.source)


def _validate_identifier(name: str, value: str, maximum: int) -> None:
    _validate_text(name, value, maximum)
    if value != value.strip():
        raise ValueError(f"{name} must be trimmed")


def _validate_text(
    name: str,
    value: str,
    maximum: int,
    *,
    allow_blank: bool = False,
    allow_newline_tab: bool = False,
) -> None:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be a string")
    if not allow_blank and not value.strip():
        raise ValueError(f"{name} must not be blank")
    if len(value) > maximum:
        raise ValueError(f"{name} is too long")
    if _contains_control(value, allow_newline_tab=allow_newline_tab):
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
