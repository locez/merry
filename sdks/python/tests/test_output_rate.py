from __future__ import annotations

import pytest

import merry
from merry._event_parser import parse_event
from merry._json import JsonObject, JsonValue


def rate_event() -> JsonObject:
    return {
        "type": "model_output_rate_updated",
        "rate": {
            "output_tokens": 2400,
            "elapsed_nanos": 8_000_000_000,
            "token_source": "provider_usage",
            "timing_quality": "receive_window",
        },
        "source": {"session_id": "session-1", "sequence": 3},
    }


def test_output_rate_event_exposes_typed_provider_receive_timing() -> None:
    event = parse_event(rate_event())
    assert event.type is merry.EventType.MODEL_OUTPUT_RATE_UPDATED
    assert isinstance(event.payload, merry.ModelOutputRateUpdatedPayload)
    assert event.payload.rate == merry.ModelOutputRate(
        2400,
        8_000_000_000,
        merry.OutputTokenSource.PROVIDER_USAGE,
        merry.OutputTimingQuality.RECEIVE_WINDOW,
    )
    assert event.payload.source.sequence == 3


def test_output_rate_reset_does_not_conflate_missing_with_zero_usage() -> None:
    data = rate_event()
    data["rate"] = None
    event = parse_event(data)
    assert isinstance(event.payload, merry.ModelOutputRateUpdatedPayload)
    assert event.payload.rate is None


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("output_tokens", -1),
        ("elapsed_nanos", True),
        ("token_source", False),
        ("timing_quality", "unsupported"),
        ("extra", 1),
    ],
)
def test_output_rate_rejects_invalid_or_unknown_fields(
    field: str, value: JsonValue
) -> None:
    data = rate_event()
    rate: JsonObject = {
        "output_tokens": 2400,
        "elapsed_nanos": 8_000_000_000,
        "token_source": "provider_usage",
        "timing_quality": "receive_window",
    }
    rate[field] = value
    data["rate"] = rate
    with pytest.raises((TypeError, ValueError)):
        parse_event(data)
