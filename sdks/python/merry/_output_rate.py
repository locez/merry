"""Typed receive-side throughput observations owned by the Rust runtime."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import ClassVar

from ._event_types import (
    EventSource,
    EventType,
    SourcedEventPayload,
    _validate_event_source,
    _validate_nonnegative,
)
from ._event_values import _enum_value, _required_int
from ._json import JsonValue, require_json_object, validate_object_keys


class OutputTokenSource(str, Enum):
    """Source of the output-token numerator."""

    ESTIMATED = "estimated"
    PROVIDER_USAGE = "provider_usage"


class OutputTimingQuality(str, Enum):
    """Limitations of the client receive interval, not a server decode clock."""

    RECEIVE_WINDOW = "receive_window"
    PARTIAL_OUTPUT = "partial_output"
    CONSUMER_LIMITED = "consumer_limited"


@dataclass(frozen=True, slots=True)
class ModelOutputRate:
    """Output tokens over the provider's first-to-last receive interval.

    Zero duration cannot yield throughput. Limited timing retains an approximate
    receive rate; consumer-limited intervals can include consumer stalls.
    Provider usage does not imply that hidden reasoning timing was observed.
    """

    output_tokens: int
    elapsed_nanos: int
    token_source: OutputTokenSource
    timing_quality: OutputTimingQuality

    def __post_init__(self) -> None:
        _validate_nonnegative("output tokens", self.output_tokens)
        _validate_nonnegative("output elapsed nanoseconds", self.elapsed_nanos)
        if not isinstance(self.token_source, OutputTokenSource):
            raise TypeError("token_source must be an OutputTokenSource")
        if not isinstance(self.timing_quality, OutputTimingQuality):
            raise TypeError("timing_quality must be an OutputTimingQuality")


@dataclass(frozen=True, slots=True)
class ModelOutputRateUpdatedPayload(SourcedEventPayload):
    """Receive-side sample, or None when the runtime clears the active sample."""

    event_type: ClassVar[EventType] = EventType.MODEL_OUTPUT_RATE_UPDATED
    rate: ModelOutputRate | None
    source: EventSource

    def __post_init__(self) -> None:
        _validate_event_source(self.source)
        if self.rate is not None and not isinstance(self.rate, ModelOutputRate):
            raise TypeError("rate must be a ModelOutputRate or None")


def parse_output_rate(value: JsonValue) -> ModelOutputRate | None:
    """Decode a strict runtime observation without recalculating Rust-owned metrics."""
    if value is None:
        return None
    rate = require_json_object(value, "model output rate")
    validate_object_keys(
        rate,
        "model output rate",
        required={"output_tokens", "elapsed_nanos", "token_source", "timing_quality"},
    )
    return ModelOutputRate(
        _required_int(rate, "output_tokens"),
        _required_int(rate, "elapsed_nanos"),
        _enum_value(OutputTokenSource, rate["token_source"], "output token source"),
        _enum_value(
            OutputTimingQuality, rate["timing_quality"], "output timing quality"
        ),
    )
