import asyncio
import time

import pytest

import merry
from _support import runtime_with_fake_response


async def _assert_runtime_run_returns_final_output_and_events():
    runtime = runtime_with_fake_response("done")

    result = await runtime.run("Say done.")

    assert result.final_output == "done"
    assert result.status == "completed"
    assert result.steps_run == 1
    assert isinstance(result.status, str)
    assert isinstance(result.steps_run, int)
    assert all(isinstance(event, dict) for event in result.events)
    assert [event["kind"]["type"] for event in result.events] == [
        "session_started",
        "step_started",
        "artifact_recorded",
        "step_completed",
    ]


def test_runtime_run_returns_final_output_and_events():
    asyncio.run(_assert_runtime_run_returns_final_output_and_events())


async def _assert_runtime_run_stream_yields_event_dicts_in_order():
    runtime = runtime_with_fake_response("streamed")

    event_types = []
    events = []
    async for event in runtime.run_stream("Say streamed."):
        events.append(event)
        event_types.append(event["kind"]["type"])

    assert all(isinstance(event, dict) for event in events)
    assert event_types == [
        "session_started",
        "step_started",
        "artifact_recorded",
        "step_completed",
    ]


def test_runtime_run_stream_yields_event_dicts_in_order():
    asyncio.run(_assert_runtime_run_stream_yields_event_dicts_in_order())


async def _assert_runtime_stream_returns_result_after_events():
    runtime = runtime_with_fake_response("streamed result")
    stream = runtime.stream("Say streamed.")
    event_types = []

    async for event in stream:
        event_types.append(event["kind"]["type"])

    result = await stream.result()

    assert result.status == "completed"
    assert result.steps_run == 1
    assert result.final_output == "streamed result"
    assert [event["kind"]["type"] for event in result.events] == event_types


def test_runtime_stream_returns_result_after_events():
    asyncio.run(_assert_runtime_stream_returns_result_after_events())


async def _assert_runtime_run_stream_yields_before_stream_finishes():
    runtime = merry.Runtime.__new__(merry.Runtime)

    class SlowNativeStream:
        def __init__(self):
            self._events = [
                {"kind": {"type": "session_started"}},
                {"kind": {"type": "step_started"}},
                None,
            ]

        def next_blocking(self):
            time.sleep(0.05)
            return self._events.pop(0)

    class SlowStreamingNativeRuntime:
        def run_stream_blocking(self, _task):
            return SlowNativeStream()

    runtime._native = SlowStreamingNativeRuntime()
    runtime._tools = {}

    started = time.monotonic()
    stream = runtime.run_stream("Say streamed.")
    first_event = await anext(stream)

    assert first_event["kind"]["type"] == "session_started"
    assert time.monotonic() - started < 0.09


def test_runtime_run_stream_yields_before_stream_finishes():
    asyncio.run(_assert_runtime_run_stream_yields_before_stream_finishes())


async def _assert_runtime_run_does_not_block_event_loop():
    runtime = merry.Runtime.__new__(merry.Runtime)

    class SlowNativeRuntime:
        def run_blocking(self, _task):
            time.sleep(0.05)
            return {
                "status": "completed",
                "steps_run": 1,
                "final_output": "done",
                "events": [],
            }

    runtime._native = SlowNativeRuntime()
    ticks = []

    async def ticker():
        await asyncio.sleep(0.01)
        ticks.append(time.monotonic())

    started = time.monotonic()
    result, _ = await asyncio.gather(runtime.run("Say done."), ticker())

    assert result.final_output == "done"
    assert ticks
    assert ticks[0] - started < 0.04


def test_runtime_run_does_not_block_event_loop():
    asyncio.run(_assert_runtime_run_does_not_block_event_loop())


def test_runtime_default_constructor_returns_runtime_object():
    runtime = merry.Runtime()

    assert isinstance(runtime, merry.Runtime)


def test_runtime_invalid_session_id_maps_without_leaking_rejected_value():
    rejected_value = " secret-token "

    with pytest.raises(merry.MerryRuntimeError) as raised:
        merry.Runtime(session_id=rejected_value)

    assert raised.value.code == "runtime.invalid_session_id"
    assert raised.value.domain == "runtime"
    assert "secret-token" not in raised.value.info.message
    assert "secret-token" not in str(raised.value)
    assert "secret-token" not in (raised.value.info.hint or "")
    assert "secret-token" not in repr(raised.value.info.context)
