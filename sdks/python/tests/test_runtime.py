import asyncio
import time
from uuid import UUID

import pytest

import merry
from _support import runtime_with_fake_response


async def _assert_runtime_run_returns_final_output_and_events():
    runtime = runtime_with_fake_response("done")

    result = await runtime.run("Say done.")

    assert result.final_output == "done"
    assert result.status == "completed"
    assert result.model_turns_run == 1
    assert isinstance(result.status, str)
    assert isinstance(result.model_turns_run, int)
    assert all(isinstance(event, dict) for event in result.events)
    assert [event["type"] for event in result.events] == [
        "session_started",
        "step_started",
        "assistant_message_delta",
        "assistant_message",
        "step_completed",
    ]
    assert result.events[2]["delta"] == "done"


def test_runtime_run_returns_final_output_and_events():
    asyncio.run(_assert_runtime_run_returns_final_output_and_events())


def test_runtime_usage_is_none_before_model_usage() -> None:
    runtime = runtime_with_fake_response("done")

    assert asyncio.run(runtime.usage()) is None


def test_run_result_exposes_session_usage_snapshot() -> None:
    runtime = runtime_with_fake_response("done")

    result = asyncio.run(runtime.run("hello"))

    assert result.session_usage is None


async def _assert_runtime_run_stream_yields_event_dicts_in_order():
    runtime = runtime_with_fake_response("streamed")

    event_types = []
    events = []
    async for event in runtime.run_stream("Say streamed."):
        events.append(event)
        event_types.append(event["type"])

    assert all(isinstance(event, dict) for event in events)
    assert event_types == [
        "session_started",
        "step_started",
        "assistant_message_delta",
        "assistant_message",
        "step_completed",
    ]
    assert events[2]["delta"] == "streamed"


def test_runtime_run_stream_yields_event_dicts_in_order():
    asyncio.run(_assert_runtime_run_stream_yields_event_dicts_in_order())


async def _assert_runtime_stream_returns_result_after_events():
    runtime = runtime_with_fake_response("streamed result")
    stream = runtime.stream("Say streamed.")
    event_types = []

    async for event in stream:
        event_types.append(event["type"])

    result = await stream.result()

    assert result.status == "completed"
    assert result.model_turns_run == 1
    assert result.final_output == "streamed result"
    assert [event["type"] for event in result.events] == event_types


def test_runtime_stream_returns_result_after_events():
    asyncio.run(_assert_runtime_stream_returns_result_after_events())


async def _assert_runtime_run_stream_yields_before_stream_finishes():
    runtime = merry.Runtime.__new__(merry.Runtime)

    class SlowNativeStream:
        def __init__(self):
            self._events = [
                {"type": "session_started"},
                {"type": "step_started"},
                None,
            ]

        def next_blocking(self):
            time.sleep(0.05)
            return self._events.pop(0)

    class SlowStreamingNativeRuntime:
        def run_stream_blocking(self, _task, _final_output_schema_json=None, _max_model_turns=None):
            return SlowNativeStream()

    runtime._native = SlowStreamingNativeRuntime()
    runtime._tools = {}

    started = time.monotonic()
    stream = runtime.run_stream("Say streamed.")
    first_event = await anext(stream)

    assert first_event["type"] == "session_started"
    assert time.monotonic() - started < 0.09


def test_runtime_run_stream_yields_before_stream_finishes():
    asyncio.run(_assert_runtime_run_stream_yields_before_stream_finishes())


async def _assert_runtime_run_does_not_block_event_loop():
    runtime = merry.Runtime.__new__(merry.Runtime)

    class SlowNativeRuntime:
        def run_blocking(self, _task, _final_output_schema_json=None, _max_model_turns=None):
            time.sleep(0.05)
            return {
                "status": "completed",
                "model_turns_run": 1,
                "final_output": "done",
                "final_output_json": None,
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


async def _assert_runtime_run_passes_max_model_turns_to_native():
    runtime = merry.Runtime.__new__(merry.Runtime)
    seen = {}

    class NativeRuntime:
        def run_blocking(self, task, final_output_schema_json=None, max_model_turns=None):
            seen["task"] = task
            seen["schema"] = final_output_schema_json
            seen["max_model_turns"] = max_model_turns
            return {
                "status": "completed",
                "model_turns_run": 1,
                "final_output": "done",
                "final_output_json": None,
                "events": [],
            }

    runtime._native = NativeRuntime()
    runtime._tools = {}

    await runtime.run("Say done.", max_model_turns=32)

    assert seen == {"task": "Say done.", "schema": None, "max_model_turns": 32}


def test_runtime_run_passes_max_model_turns_to_native():
    asyncio.run(_assert_runtime_run_passes_max_model_turns_to_native())


def test_runtime_rejects_invalid_max_model_turns():
    runtime = merry.Runtime.__new__(merry.Runtime)
    runtime._native = object()
    runtime._tools = {}

    with pytest.raises(ValueError, match="max_model_turns"):
        runtime.stream("Say streamed.", max_model_turns=0)

    with pytest.raises(TypeError, match="max_model_turns"):
        runtime.stream("Say streamed.", max_model_turns=True)


async def _assert_stream_closed_after_bridge_result_returns_blocked_result():
    runtime = merry.Runtime.__new__(merry.Runtime)
    calls = []

    class NativeStream:
        def __init__(self):
            self._events = [
                {
                    "type": "bridge_tool_request",
                    "call": {
                        "id": "call-bridge",
                        "name": "probe_step",
                        "arguments": {"value": "payload"},
                    },
                },
                None,
            ]

        def next_blocking(self):
            return self._events.pop(0)

        def submit_tool_success_json_blocking(self, _call_id, _artifact_id, _content_json):
            raise merry.NativeMerryError(
                '{"code":"runtime.stream_closed","domain":"runtime",'
                '"message":"Runtime event stream closed before accepting the bridge tool result.",'
                '"hint":"Keep the active RuntimeStream open while the SDK bridge driver resolves bridge tools.",'
                '"retryability":"user_action_required","context":{}}'
            )

        def result_blocking(self):
            return {
                "status": "blocked",
                "model_turns_run": 16,
                "final_output": None,
                "final_output_json": None,
                "events": [],
            }

    class NativeRuntime:
        def run_stream_blocking(self, _task, _final_output_schema_json=None, _max_model_turns=None):
            return NativeStream()

    runtime._native = NativeRuntime()

    async def probe_step(value: str):
        calls.append(value)
        return {"ok": True}

    runtime._tools = {"probe_step": merry.Tool.bridge(probe_step, description="Probe.", schema={})}

    stream = runtime.stream("Run probe.", max_model_turns=16)
    event_types = []
    async for event in stream:
        event_types.append(event["type"])

    result = await stream.result()

    assert calls == ["payload"]
    assert event_types == []
    assert result.status == "blocked"
    assert result.model_turns_run == 16


def test_stream_closed_after_bridge_result_returns_blocked_result():
    asyncio.run(_assert_stream_closed_after_bridge_result_returns_blocked_result())


def test_runtime_default_constructor_returns_runtime_object():
    runtime = merry.Runtime()

    assert isinstance(runtime, merry.Runtime)


def _assert_uuid_v4(value: str) -> None:
    parsed = UUID(value)
    assert parsed.version == 4
    assert str(parsed) == value


def test_runtime_default_session_ids_are_random_uuid_v4_values():
    first = merry.Runtime()
    second = merry.Runtime()

    assert first.session_id != second.session_id
    _assert_uuid_v4(first.session_id)
    _assert_uuid_v4(second.session_id)


def test_runtime_accepts_explicit_session_id():
    runtime = merry.Runtime(session_id="tenant-a.debug_1")

    assert runtime.session_id == "tenant-a.debug_1"


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


async def _assert_interactive_run_has_split_stream_input_control():
    runtime = runtime_with_fake_response("done")
    run = runtime.start_interactive()

    assert run.stream is not None
    assert run.input is not None
    assert run.control is not None

    item = await run.input.submit_next("hello")
    assert item.lane == "next"
    assert item.text == "hello"

    events = []
    async for event in run.stream:
        events.append(event)
        if event.get("type") == "queued_input_accepted":
            break

    assert any(event.get("type") == "queued_input_accepted" for event in events)
    assert not hasattr(run.control, "resume_backlog")


def test_interactive_run_has_split_stream_input_control():
    asyncio.run(_assert_interactive_run_has_split_stream_input_control())


async def _assert_interactive_backlog_enqueue_is_accepted_from_python():
    runtime = runtime_with_fake_response("done")
    run = runtime.start_interactive()

    item = await run.input.enqueue("first")
    assert item.lane == "backlog"
    assert item.text == "first"

    events = []
    async for event in run.stream:
        events.append(event)
        if event.get("type") == "queued_input_accepted":
            break

    assert any(
        event.get("type") == "queued_input_accepted"
        and event.get("lane") == "backlog"
        and event.get("inputs") == [
            {
                "text": "first",
                "lane": "backlog",
                "position": 0,
            }
        ]
        for event in events
    )

    await run.control.close()


def test_interactive_backlog_enqueue_is_accepted_from_python():
    asyncio.run(_assert_interactive_backlog_enqueue_is_accepted_from_python())


async def _assert_interactive_replace_pending_order_uses_item_handles():
    seen = {}

    class NativeInput:
        def replace_pending_order_blocking(self, lane, items):
            seen["lane"] = lane
            seen["items"] = items

    first_native = object()
    second_native = object()
    input_handle = merry.AgentLoopInput(NativeInput())
    first = merry.InteractiveInputItem(first_native)
    second = merry.InteractiveInputItem(second_native)

    await input_handle.replace_pending_order("backlog", [second, first])

    assert seen == {
        "lane": "backlog",
        "items": [second_native, first_native],
    }


def test_interactive_replace_pending_order_uses_item_handles():
    asyncio.run(_assert_interactive_replace_pending_order_uses_item_handles())


def test_runtime_skills_defaults_to_empty_list():
    runtime = merry.Runtime()

    assert runtime.skills() == []
