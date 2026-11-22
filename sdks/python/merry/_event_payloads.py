"""Typed payloads for the provider-neutral runtime event stream."""

from __future__ import annotations

from dataclasses import dataclass
from typing import ClassVar, TypeAlias

from ._event_lifecycle import (
    CompactionCompletedPayload,
    CompactionStartedPayload,
    ModelRetryAttemptStartedPayload,
    ModelRetryExhaustedPayload,
    ModelRetryScheduledPayload,
    RunCancelledPayload,
    RunFailedPayload,
    SessionStartedPayload,
    StepCompletedPayload,
    StepStartedPayload,
    UsageUpdatedPayload,
)
from ._event_types import (
    EventDiagnostic,
    EventPayload,
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
    RuntimeToolResult,
    SourcedEventPayload,
    SubagentStatus,
    TodoItem,
    _validate_event_source,
    _validate_identifier,
    _validate_text,
)
from ._models import ModelUsage
from ._output_rate import ModelOutputRateUpdatedPayload


@dataclass(frozen=True, slots=True)
class AssistantMessagePayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.ASSISTANT_MESSAGE
    text: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_text(
            "assistant message",
            self.text,
            1_048_576,
            allow_blank=True,
            allow_newline_tab=True,
        )


@dataclass(frozen=True, slots=True)
class AssistantMessageDeltaPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.ASSISTANT_MESSAGE_DELTA
    delta: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_text(
            "assistant message delta",
            self.delta,
            1_048_576,
            allow_blank=True,
            allow_newline_tab=True,
        )


@dataclass(frozen=True, slots=True)
class ReasoningDeltaPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.REASONING_DELTA
    delta: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_text(
            "reasoning delta",
            self.delta,
            1_048_576,
            allow_blank=True,
            allow_newline_tab=True,
        )


@dataclass(frozen=True, slots=True)
class ToolCallStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.TOOL_CALL_STARTED
    call: RuntimeToolCall
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.call, RuntimeToolCall):
            raise TypeError("tool call must be a RuntimeToolCall")


@dataclass(frozen=True, slots=True)
class ToolCallBatchStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.TOOL_CALL_BATCH_STARTED
    batch: RuntimeToolCallBatch
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.batch, RuntimeToolCallBatch):
            raise TypeError("tool call batch must be a RuntimeToolCallBatch")


@dataclass(frozen=True, slots=True)
class ToolCallFinishedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.TOOL_CALL_FINISHED
    result: RuntimeToolResult
    output: RuntimeToolOutput | None
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.result, RuntimeToolResult):
            raise TypeError("tool result must be a RuntimeToolResult")
        if self.output is not None and not isinstance(self.output, RuntimeToolOutput):
            raise TypeError("tool output must be a RuntimeToolOutput or None")


@dataclass(frozen=True, slots=True)
class FinalOutputRecordedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.FINAL_OUTPUT_RECORDED
    call_id: str
    json: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("final output call id", self.call_id, 256)
        _validate_text(
            "final output JSON",
            self.json,
            1_048_576,
            allow_blank=True,
            allow_newline_tab=True,
        )


@dataclass(frozen=True, slots=True)
class SkillUsedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SKILL_USED
    skill_name: str
    skill_md_path: str
    tool_call_id: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("skill name", self.skill_name, 256)
        _validate_identifier("skill SKILL.md path", self.skill_md_path, 4096)
        _validate_identifier("skill tool call id", self.tool_call_id, 256)


@dataclass(frozen=True, slots=True)
class SubagentSpawnedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SUBAGENT_SPAWNED
    agent_id: str
    task_id: str
    task_anchor: str
    source: EventSource
    display_name: str | None = None
    role: str | None = None

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)
        _validate_text("subagent task anchor", self.task_anchor, 4096)
        if self.display_name is not None:
            _validate_text("subagent display name", self.display_name, 256)
        if self.role is not None:
            _validate_identifier("subagent role", self.role, 128)


@dataclass(frozen=True, slots=True)
class SubagentStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SUBAGENT_STARTED
    agent_id: str
    task_id: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)


@dataclass(frozen=True, slots=True)
class SubagentStatusChangedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SUBAGENT_STATUS_CHANGED
    agent_id: str
    task_id: str
    status: SubagentStatus
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)
        if not isinstance(self.status, SubagentStatus):
            raise TypeError("subagent status must be a SubagentStatus")


@dataclass(frozen=True, slots=True)
class SubagentCompletedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SUBAGENT_COMPLETED
    agent_id: str
    task_id: str
    summary: str
    output_paths: tuple[str, ...]
    changed_paths: tuple[str, ...]
    source: EventSource
    usage: ModelUsage | None = None

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)
        _validate_text("subagent summary", self.summary, 4096)
        _validate_strings("subagent output paths", self.output_paths, 4096)
        _validate_strings("subagent changed paths", self.changed_paths, 4096)
        if self.usage is not None and not isinstance(self.usage, ModelUsage):
            raise TypeError("subagent usage must be ModelUsage or None")


@dataclass(frozen=True, slots=True)
class SubagentFailedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SUBAGENT_FAILED
    agent_id: str
    task_id: str
    diagnostic: EventDiagnostic
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)
        if not isinstance(self.diagnostic, EventDiagnostic):
            raise TypeError("subagent diagnostic must be an EventDiagnostic")


@dataclass(frozen=True, slots=True)
class SubagentCancelledPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SUBAGENT_CANCELLED
    agent_id: str
    task_id: str
    diagnostic: EventDiagnostic
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)
        if not isinstance(self.diagnostic, EventDiagnostic):
            raise TypeError("subagent diagnostic must be an EventDiagnostic")


@dataclass(frozen=True, slots=True)
class PlanModeChangedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_MODE_CHANGED
    mode: PlanModeState
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.mode, PlanModeState):
            raise TypeError("plan mode must be a PlanModeState")


@dataclass(frozen=True, slots=True)
class PlanSavedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_SAVED
    plan: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.plan, RawEventData):
            raise TypeError("plan document must be RawEventData")


@dataclass(frozen=True, slots=True)
class PlanApprovalRequestedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_APPROVAL_REQUESTED
    plan: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.plan, RawEventData):
            raise TypeError("plan document must be RawEventData")


@dataclass(frozen=True, slots=True)
class PlanReviewedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_REVIEWED
    plan_id: str
    decision: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("plan id", self.plan_id, 256)
        if not isinstance(self.decision, RawEventData):
            raise TypeError("plan review decision must be RawEventData")


@dataclass(frozen=True, slots=True)
class TodoListUpdatedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.TODO_LIST_UPDATED
    items: tuple[TodoItem, ...]
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.items, tuple) or any(
            not isinstance(item, TodoItem) for item in self.items
        ):
            raise TypeError("todo items must be a tuple of TodoItem values")


@dataclass(frozen=True, slots=True)
class InteractiveRunStateChangedPayload(EventPayload):
    event_type: ClassVar[EventType] = EventType.INTERACTIVE_RUN_STATE_CHANGED
    state: InteractiveRunState

    def __post_init__(self) -> None:
        if not isinstance(self.state, InteractiveRunState):
            raise TypeError("interactive run state must be an InteractiveRunState")


@dataclass(frozen=True, slots=True)
class QueuedInputAcceptedPayload(EventPayload):
    event_type: ClassVar[EventType] = EventType.QUEUED_INPUT_ACCEPTED
    inputs: tuple[QueuedInput, ...]

    def __post_init__(self) -> None:
        if not isinstance(self.inputs, tuple) or any(
            not isinstance(item, QueuedInput) for item in self.inputs
        ):
            raise TypeError("queued inputs must be a tuple of QueuedInput values")


@dataclass(frozen=True, slots=True)
class QueuedInputsChangedPayload(EventPayload):
    event_type: ClassVar[EventType] = EventType.QUEUED_INPUTS_CHANGED
    inputs: QueuedInputs

    def __post_init__(self) -> None:
        if not isinstance(self.inputs, QueuedInputs):
            raise TypeError("queued inputs must be a QueuedInputs value")


@dataclass(frozen=True, slots=True)
class ClosedPayload(EventPayload):
    event_type: ClassVar[EventType] = EventType.CLOSED


@dataclass(frozen=True, slots=True)
class UnknownEventPayload(EventPayload):
    """Forward-compatible payload for a Rust event added after this SDK."""

    event_type: ClassVar[EventType] = EventType.UNKNOWN
    raw_type: str
    data: RawEventData

    def __post_init__(self) -> None:
        _validate_identifier("unknown event type", self.raw_type, 128)
        if not isinstance(self.data, RawEventData):
            raise TypeError("unknown event data must be RawEventData")


def _validate_strings(name: str, values: tuple[str, ...], maximum: int) -> None:
    if not isinstance(values, tuple):
        raise TypeError(f"{name} must be a tuple of strings")
    for value in values:
        _validate_text(name, value, maximum)


EventPayloadValue: TypeAlias = (
    SessionStartedPayload
    | StepStartedPayload
    | StepCompletedPayload
    | CompactionStartedPayload
    | CompactionCompletedPayload
    | UsageUpdatedPayload
    | ModelOutputRateUpdatedPayload
    | AssistantMessagePayload
    | AssistantMessageDeltaPayload
    | ReasoningDeltaPayload
    | ToolCallStartedPayload
    | ToolCallBatchStartedPayload
    | ToolCallFinishedPayload
    | FinalOutputRecordedPayload
    | ModelRetryAttemptStartedPayload
    | ModelRetryScheduledPayload
    | ModelRetryExhaustedPayload
    | SkillUsedPayload
    | SubagentSpawnedPayload
    | SubagentStartedPayload
    | SubagentStatusChangedPayload
    | SubagentCompletedPayload
    | SubagentFailedPayload
    | SubagentCancelledPayload
    | PlanModeChangedPayload
    | PlanSavedPayload
    | PlanApprovalRequestedPayload
    | PlanReviewedPayload
    | TodoListUpdatedPayload
    | RunFailedPayload
    | RunCancelledPayload
    | InteractiveRunStateChangedPayload
    | QueuedInputAcceptedPayload
    | QueuedInputsChangedPayload
    | ClosedPayload
    | UnknownEventPayload
)
