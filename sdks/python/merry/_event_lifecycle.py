"""Typed session, model-attempt, usage, and terminal lifecycle payloads."""

from dataclasses import dataclass
from typing import ClassVar

from ._event_types import (
    EventDiagnostic,
    EventSource,
    EventType,
    SourcedEventPayload,
    _validate_event_source,
    _validate_identifier,
    _validate_nonnegative,
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
