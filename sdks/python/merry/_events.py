"""Public typed runtime event envelope and event contract exports."""

from __future__ import annotations

from dataclasses import dataclass

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
    RuntimeToolOutputKind,
    RuntimeToolResult,
    RuntimeToolResultStatus,
    SubagentStatus,
)


@dataclass(frozen=True, slots=True)
class Event:
    """Typed runtime event with a discriminated provider-neutral payload."""

    type: EventType
    payload: EventPayloadValue

    def __post_init__(self) -> None:
        if not isinstance(self.type, EventType):
            raise TypeError("event type must be an EventType")
        if not isinstance(self.payload, EventPayload):
            raise TypeError("event payload must be a typed EventPayload")
        if self.type is not self.payload.event_type:
            raise ValueError("event type and payload discriminants must match")


__all__ = [
    "ArtifactReference",
    "AssistantMessageDeltaPayload",
    "AssistantMessagePayload",
    "ClosedPayload",
    "CompactionCompletedPayload",
    "CompactionStartedPayload",
    "Event",
    "EventDiagnostic",
    "EventPayload",
    "EventPayloadValue",
    "EventSource",
    "EventType",
    "EvidenceReference",
    "EvidenceReferencedPayload",
    "FinalOutputRecordedPayload",
    "InteractiveRunState",
    "InteractiveRunStateChangedPayload",
    "ModelRetryAttemptStartedPayload",
    "ModelRetryExhaustedPayload",
    "ModelRetryScheduledPayload",
    "PlanAttemptFinishedPayload",
    "PlanAttemptProgressReportedPayload",
    "PlanDirectiveUpdatedPayload",
    "PlanLeaseStartedPayload",
    "PlanNodeReadyPayload",
    "PlanPhaseChangedPayload",
    "PlanProgressReviewRequestedPayload",
    "PlanProgressUpdatedPayload",
    "PlanUpdatedPayload",
    "QueuedInput",
    "QueuedInputAcceptedPayload",
    "QueuedInputLane",
    "QueuedInputs",
    "QueuedInputsChangedPayload",
    "RawEventData",
    "RunCancelledPayload",
    "RunFailedPayload",
    "RuntimeToolCall",
    "RuntimeToolCallBatch",
    "RuntimeToolOutput",
    "RuntimeToolOutputKind",
    "RuntimeToolResult",
    "RuntimeToolResultStatus",
    "SessionStartedPayload",
    "SkillUsedPayload",
    "StepCompletedPayload",
    "StepStartedPayload",
    "SubagentCancelledPayload",
    "SubagentCompletedPayload",
    "SubagentFailedPayload",
    "SubagentSpawnedPayload",
    "SubagentStartedPayload",
    "SubagentStatus",
    "SubagentStatusChangedPayload",
    "ToolCallBatchStartedPayload",
    "ToolCallFinishedPayload",
    "ToolCallStartedPayload",
    "UnknownEventPayload",
    "UsageUpdatedPayload",
]
