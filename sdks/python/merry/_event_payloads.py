"""Typed payloads for the provider-neutral runtime event stream."""

from __future__ import annotations

from dataclasses import dataclass
from typing import ClassVar, TypeAlias

from ._event_types import (
    ArtifactReference,
    EventDiagnostic,
    EventPayload,
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
    RuntimeToolResult,
    SourcedEventPayload,
    SubagentStatus,
    _validate_event_source,
    _validate_identifier,
    _validate_nonnegative,
    _validate_text,
)
from ._models import SessionUsage


@dataclass(frozen=True, slots=True)
class SessionStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SESSION_STARTED
    source: EventSource


@dataclass(frozen=True, slots=True)
class StepStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.STEP_STARTED
    source: EventSource


@dataclass(frozen=True, slots=True)
class StepCompletedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.STEP_COMPLETED
    source: EventSource


@dataclass(frozen=True, slots=True)
class CompactionStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.COMPACTION_STARTED
    source: EventSource


@dataclass(frozen=True, slots=True)
class CompactionCompletedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.COMPACTION_COMPLETED
    checkpoint_id: str
    covered_history_item_count: int
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("checkpoint id", self.checkpoint_id, 256)
        _validate_nonnegative(
            "covered history item count", self.covered_history_item_count
        )


@dataclass(frozen=True, slots=True)
class UsageUpdatedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.USAGE_UPDATED
    usage: SessionUsage
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.usage, SessionUsage):
            raise TypeError("usage must be a SessionUsage")


@dataclass(frozen=True, slots=True)
class AssistantMessagePayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.ASSISTANT_MESSAGE
    text: str
    artifact: ArtifactReference
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
        if not isinstance(self.artifact, ArtifactReference):
            raise TypeError("assistant message artifact must be an ArtifactReference")


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
    artifact: ArtifactReference
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("final output call id", self.call_id, 256)
        if not isinstance(self.artifact, ArtifactReference):
            raise TypeError("final output artifact must be an ArtifactReference")


@dataclass(frozen=True, slots=True)
class ModelRetryAttemptStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.MODEL_RETRY_ATTEMPT_STARTED
    attempt: int
    max_attempts: int
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_nonnegative("retry attempt", self.attempt)
        _validate_nonnegative("maximum retry attempts", self.max_attempts)


@dataclass(frozen=True, slots=True)
class ModelRetryScheduledPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.MODEL_RETRY_SCHEDULED
    attempt: int
    next_attempt: int
    max_attempts: int
    delay_ms: int
    error_kind: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_nonnegative("retry attempt", self.attempt)
        _validate_nonnegative("next retry attempt", self.next_attempt)
        _validate_nonnegative("maximum retry attempts", self.max_attempts)
        _validate_nonnegative("retry delay", self.delay_ms)
        _validate_identifier("retry error kind", self.error_kind, 256)


@dataclass(frozen=True, slots=True)
class ModelRetryExhaustedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.MODEL_RETRY_EXHAUSTED
    attempts_run: int
    max_attempts: int
    error_kind: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_nonnegative("retry attempts run", self.attempts_run)
        _validate_nonnegative("maximum retry attempts", self.max_attempts)
        _validate_identifier("retry error kind", self.error_kind, 256)


@dataclass(frozen=True, slots=True)
class EvidenceReferencedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.EVIDENCE_REFERENCED
    evidence: EvidenceReference
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.evidence, EvidenceReference):
            raise TypeError("evidence must be an EvidenceReference")


@dataclass(frozen=True, slots=True)
class SkillUsedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SKILL_USED
    skill_name: str
    skill_md_path: str
    tool_call_id: str
    artifact: ArtifactReference
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("skill name", self.skill_name, 256)
        _validate_identifier("skill SKILL.md path", self.skill_md_path, 4096)
        _validate_identifier("skill tool call id", self.tool_call_id, 256)
        if not isinstance(self.artifact, ArtifactReference):
            raise TypeError("skill artifact must be an ArtifactReference")


@dataclass(frozen=True, slots=True)
class SubagentSpawnedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.SUBAGENT_SPAWNED
    agent_id: str
    task_id: str
    task_anchor: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)
        _validate_text("subagent task anchor", self.task_anchor, 4096)


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

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("subagent id", self.agent_id, 256)
        _validate_identifier("subagent task id", self.task_id, 256)
        _validate_text("subagent summary", self.summary, 4096)
        _validate_strings("subagent output paths", self.output_paths, 4096)
        _validate_strings("subagent changed paths", self.changed_paths, 4096)


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
class PlanUpdatedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_UPDATED
    snapshot: RawEventData
    summary: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.snapshot, RawEventData):
            raise TypeError("plan snapshot must be RawEventData")
        if not isinstance(self.summary, RawEventData):
            raise TypeError("plan revision summary must be RawEventData")


@dataclass(frozen=True, slots=True)
class PlanPhaseChangedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_PHASE_CHANGED
    plan_id: str
    phase: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("plan id", self.plan_id, 256)
        _validate_identifier("plan phase", self.phase, 128)


@dataclass(frozen=True, slots=True)
class PlanNodeReadyPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_NODE_READY
    plan_id: str
    node_id: str
    node_revision: int
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("plan id", self.plan_id, 256)
        _validate_identifier("plan node id", self.node_id, 256)
        _validate_nonnegative("plan node revision", self.node_revision)


@dataclass(frozen=True, slots=True)
class PlanLeaseStartedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_LEASE_STARTED
    lease: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.lease, RawEventData):
            raise TypeError("plan lease must be RawEventData")


@dataclass(frozen=True, slots=True)
class PlanProgressUpdatedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_PROGRESS_UPDATED
    progress: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.progress, RawEventData):
            raise TypeError("plan progress must be RawEventData")


@dataclass(frozen=True, slots=True)
class PlanProgressReviewRequestedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_PROGRESS_REVIEW_REQUESTED
    plan_id: str
    attempt_id: str
    reason: str
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        _validate_identifier("plan id", self.plan_id, 256)
        _validate_identifier("plan attempt id", self.attempt_id, 256)
        _validate_text("plan review reason", self.reason, 4096)


@dataclass(frozen=True, slots=True)
class PlanAttemptProgressReportedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_ATTEMPT_PROGRESS_REPORTED
    progress: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.progress, RawEventData):
            raise TypeError("plan attempt progress must be RawEventData")


@dataclass(frozen=True, slots=True)
class PlanDirectiveUpdatedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_DIRECTIVE_UPDATED
    directive: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.directive, RawEventData):
            raise TypeError("plan directive must be RawEventData")


@dataclass(frozen=True, slots=True)
class PlanAttemptFinishedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.PLAN_ATTEMPT_FINISHED
    attempt: RawEventData
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.attempt, RawEventData):
            raise TypeError("plan attempt must be RawEventData")


@dataclass(frozen=True, slots=True)
class RunFailedPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.RUN_FAILED
    diagnostic: EventDiagnostic
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.diagnostic, EventDiagnostic):
            raise TypeError("run diagnostic must be an EventDiagnostic")


@dataclass(frozen=True, slots=True)
class RunCancelledPayload(SourcedEventPayload):
    event_type: ClassVar[EventType] = EventType.RUN_CANCELLED
    diagnostic: EventDiagnostic
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if not isinstance(self.diagnostic, EventDiagnostic):
            raise TypeError("run diagnostic must be an EventDiagnostic")


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
    lane: QueuedInputLane
    inputs: tuple[QueuedInput, ...]

    def __post_init__(self) -> None:
        if not isinstance(self.lane, QueuedInputLane):
            raise TypeError("queued input lane must be a QueuedInputLane")
        if not isinstance(self.inputs, tuple) or any(
            not isinstance(item, QueuedInput) for item in self.inputs
        ):
            raise TypeError("queued inputs must be a tuple of QueuedInput values")
        if any(item.lane is not self.lane for item in self.inputs):
            raise ValueError("queued input lane does not match its accepted group")


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
    | AssistantMessagePayload
    | AssistantMessageDeltaPayload
    | ToolCallStartedPayload
    | ToolCallBatchStartedPayload
    | ToolCallFinishedPayload
    | FinalOutputRecordedPayload
    | ModelRetryAttemptStartedPayload
    | ModelRetryScheduledPayload
    | ModelRetryExhaustedPayload
    | EvidenceReferencedPayload
    | SkillUsedPayload
    | SubagentSpawnedPayload
    | SubagentStartedPayload
    | SubagentStatusChangedPayload
    | SubagentCompletedPayload
    | SubagentFailedPayload
    | SubagentCancelledPayload
    | PlanUpdatedPayload
    | PlanPhaseChangedPayload
    | PlanNodeReadyPayload
    | PlanLeaseStartedPayload
    | PlanProgressUpdatedPayload
    | PlanProgressReviewRequestedPayload
    | PlanAttemptProgressReportedPayload
    | PlanDirectiveUpdatedPayload
    | PlanAttemptFinishedPayload
    | RunFailedPayload
    | RunCancelledPayload
    | InteractiveRunStateChangedPayload
    | QueuedInputAcceptedPayload
    | QueuedInputsChangedPayload
    | ClosedPayload
    | UnknownEventPayload
)
