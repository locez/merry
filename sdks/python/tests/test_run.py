from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest
from _support import (
    fake_agent,
    final_output_agent,
    pending_native_agent,
    scripted_native_agent,
)
from pydantic import BaseModel, ConfigDict, Field

import merry
from merry._protocol import parse_message
from merry._run import AgentRun


class FinalAnswer(BaseModel):
    model_config = ConfigDict(extra="forbid")

    answer: str = Field(description="Structured answer text.")


class InvalidMessageNative:
    def __init__(self) -> None:
        self.next_calls = 0

    async def next(self) -> str:
        self.next_calls += 1
        return json.dumps(
            {
                "kind": "event",
                "event": {
                    "type": "session_started",
                    "source": {"session_id": "invalid-message", "sequence": 1},
                    "unsupported": True,
                },
            }
        )

    async def submit_tool_results(self, batch_id: str, results_json: str) -> str:
        raise AssertionError(f"unexpected submission: {batch_id}, {results_json}")

    async def result(self) -> str:
        raise AssertionError("invalid message must not request a result")

    async def cancel(self) -> str:
        raise AssertionError("invalid message must not request cancellation")


class InvalidResultNative:
    def __init__(self) -> None:
        self.result_calls = 0

    async def next(self) -> None:
        return None

    async def submit_tool_results(self, batch_id: str, results_json: str) -> str:
        raise AssertionError(f"unexpected submission: {batch_id}, {results_json}")

    async def result(self) -> str:
        self.result_calls += 1
        return '{"status":"completed"}'

    async def cancel(self) -> str:
        raise AssertionError("invalid result must not request cancellation")


def test_agent_run_preserves_event_order_and_terminal_result() -> None:
    async def scenario() -> merry.RunResult[BaseModel]:
        agent = fake_agent("hello", session_id="event-order")
        run = agent.stream("Say hello.")
        messages: list[merry.Event | merry.ToolCallBatch] = []
        async for message in run:
            messages.append(message)
        return await run.result()

    result = asyncio.run(scenario())

    assert result.status is merry.RunStatus.COMPLETED
    assert result.model_turns_run == 1
    assert result.final_output == "hello"
    assert [message.type.value for message in result.events] == [
        "session_started",
        "step_started",
        "assistant_message",
        "step_completed",
    ]
    assert all(isinstance(message, merry.Event) for message in result.events)


def test_result_requires_eof_and_cancel_is_idempotent() -> None:
    async def scenario() -> tuple[
        merry.RunResult[BaseModel], merry.RunResult[BaseModel]
    ]:
        native = scripted_native_agent(
            tool_name="lookup_order",
            arguments={"order_id": "A123"},
            session_id="cancel-run",
        )
        run = merry.Agent._from_native(native).stream("Cancel me.")
        while True:
            message = await run.next()
            assert message is not None
            if isinstance(message, merry.ToolCallBatch):
                break
        with pytest.raises(merry.MerryRuntimeError) as raised:
            await run.result()
        assert raised.value.code == "agent_run_not_finished"
        first = await run.cancel()
        second = await run.cancel()
        return first, second

    first, second = asyncio.run(scenario())

    assert first.status is merry.RunStatus.CANCELLED
    assert second == first


def test_invalid_invocation_fields_are_rejected_at_the_protocol_boundary() -> None:
    native = InvalidMessageNative()
    run = AgentRun[BaseModel](native, None)
    payload = json.dumps(
        {
            "kind": "tool_invocations",
            "id": "batch-1",
            "invocations": [
                {
                    "id": "call-1",
                    "name": "lookup_order",
                    "arguments": {"order_id": "A123"},
                    "unsupported": True,
                }
            ],
        }
    )

    with pytest.raises(TypeError, match="unsupported fields"):
        parse_message(payload, run)


def test_invalid_native_message_error_is_terminal_and_repeatable() -> None:
    native = InvalidMessageNative()
    run = AgentRun[BaseModel](native, None)

    for operation in (run.next, run.next, run.result):
        with pytest.raises(merry.MerryInternalError) as raised:
            asyncio.run(operation())
        assert raised.value.code == "protocol.native_message_invalid"

    assert native.next_calls == 1


def test_invalid_native_result_error_is_terminal_and_repeatable() -> None:
    native = InvalidResultNative()
    run = AgentRun[BaseModel](native, None)

    for _ in range(2):
        with pytest.raises(merry.MerryInternalError) as raised:
            asyncio.run(run.result())
        assert raised.value.code == "protocol.native_result_invalid"

    assert native.result_calls == 1


def test_explicit_tool_batch_must_be_submitted_before_run_advances() -> None:
    async def scenario() -> merry.RunResult[BaseModel]:
        native = scripted_native_agent(
            tool_name="lookup_order",
            arguments={"order_id": "A123"},
            final_text="Order loaded.",
            session_id="manual-batch",
        )
        agent = merry.Agent._from_native(native)
        run = agent.stream("Load the order.")
        batch: merry.ToolCallBatch | None = None
        while batch is None:
            message = await run.next()
            assert message is not None
            if isinstance(message, merry.ToolCallBatch):
                batch = message

        with pytest.raises(merry.MerryToolError) as raised:
            await batch.submit([])
        assert raised.value.code == "tool_batch_mismatch"

        invocation = batch.invocations[0]
        submission = await batch.submit(
            [merry.ToolResult.succeeded(invocation.id, merry.TextContent("loaded"))]
        )
        assert submission is merry.ToolSubmission.ACCEPTED
        while await run.next() is not None:
            pass
        return await run.result()

    result = asyncio.run(scenario())

    assert result.status is merry.RunStatus.COMPLETED
    assert result.final_output == "Order loaded."


def test_structured_output_is_decoded_after_rust_records_it() -> None:
    async def scenario() -> merry.RunResult[FinalAnswer]:
        agent = final_output_agent({"answer": "structured"})
        return await agent.run(
            "Return a structured answer.",
            final_output_model=FinalAnswer,
        )

    result = asyncio.run(scenario())

    assert result.status is merry.RunStatus.COMPLETED
    assert result.structured_output == FinalAnswer(answer="structured")
    assert result.final_output_json == {"answer": "structured"}


def test_session_store_save_and_resume_preserves_session_identity(
    tmp_path: Path,
) -> None:
    async def scenario() -> str:
        path = tmp_path / "session.json"
        provider = merry.OpenAICompatible(
            api_key="sk-test",
            model="gpt-test",
            base_url="https://api.example.test/v1",
        )
        agent = (
            merry.AgentBuilder("resume-test")
            .provider(provider)
            .session_store(path)
            .build()
        )
        await agent.save_session()
        resumed = await (
            merry.AgentBuilder("resume-test")
            .provider(provider)
            .session_store(path)
            .resume()
        )
        return resumed.session_id

    assert asyncio.run(scenario()) == "resume-test"


def test_close_cancels_an_unfinished_run_and_keeps_terminal_result() -> None:
    async def scenario() -> merry.RunResult[BaseModel]:
        run = fake_agent(session_id="close-run").stream("Close me.")
        await run.close()
        return await run.result()

    result = asyncio.run(scenario())

    assert result.status is merry.RunStatus.CANCELLED


def test_direct_next_cancellation_persists_a_terminal_result() -> None:
    async def scenario() -> merry.RunResult[BaseModel]:
        native = pending_native_agent("direct-next-cancel")
        run = merry.Agent._from_native(native).stream("Wait for cancellation.")
        for expected_type in ("session_started", "step_started"):
            message = await run.next()
            assert isinstance(message, merry.Event)
            assert message.type.value == expected_type
        next_task = asyncio.create_task(run.next())

        with pytest.raises(asyncio.TimeoutError):
            await asyncio.wait_for(asyncio.shield(next_task), timeout=0.2)

        next_task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await next_task
        return await asyncio.wait_for(run.result(), timeout=1)

    result = asyncio.run(scenario())

    assert result.status is merry.RunStatus.CANCELLED


def test_concurrent_next_is_rejected_without_disrupting_active_run() -> None:
    async def scenario() -> merry.RunResult[BaseModel]:
        native = pending_native_agent("concurrent-next")
        run = merry.Agent._from_native(native).stream("Wait for cancellation.")
        for expected_type in ("session_started", "step_started"):
            message = await run.next()
            assert isinstance(message, merry.Event)
            assert message.type.value == expected_type

        active_task = asyncio.create_task(run.next())
        with pytest.raises(asyncio.TimeoutError):
            await asyncio.wait_for(asyncio.shield(active_task), timeout=0.2)

        with pytest.raises(merry.MerryRuntimeError) as raised:
            await run.next()
        assert raised.value.code == "runtime.run_state"

        active_task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await active_task
        return await asyncio.wait_for(run.result(), timeout=1)

    result = asyncio.run(scenario())

    assert result.status is merry.RunStatus.CANCELLED
