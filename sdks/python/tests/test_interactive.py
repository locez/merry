from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Annotated

from _support import pending_native_agent, scripted_turns_agent
from pydantic import Field

import merry
from merry import events


def test_interactive_session_handles_multi_turn_conversation_and_tools() -> None:
    @merry.tool(parallel_safe=True)
    async def lookup_user(
        user_id: Annotated[str, Field(description="User identifier.")],
    ) -> str:
        """Look up a display name for a user id."""
        return f"user:{user_id}"

    agent = scripted_turns_agent(
        turns=[
            {
                "kind": "tool_calls",
                "calls": [
                    {
                        "id": "call-1",
                        "name": "lookup_user",
                        "arguments": {"user_id": "42"},
                    }
                ],
            },
            {"kind": "text", "text": "Hello user:42!"},
            {"kind": "text", "text": "Second turn reply."},
        ],
        tools=[lookup_user],
        session_id="interactive-multi-turn",
    )

    async def scenario() -> list[str]:
        replies: list[str] = []
        async with agent.interactive() as session:
            item1 = await session.send("Greet user 42")
            assert item1.text == "Greet user 42"

            async for message in session:
                if isinstance(message, merry.ToolCallBatch):
                    submission = await agent.execute_tool_batch(message)
                    assert submission is merry.ToolSubmission.ACCEPTED
                elif isinstance(message.payload, events.AssistantMessagePayload):
                    replies.append(message.payload.text)
                    if len(replies) == 1:
                        await session.send("Now send a follow-up")
                    elif len(replies) == 2:
                        break
        assert session.closed
        return replies

    replies = asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))
    assert replies == ["Hello user:42!", "Second turn reply."]


def test_interactive_queue_pause_edit_remove_and_flush() -> None:
    @merry.tool
    async def wait_gate(
        tag: Annotated[str, Field(description="Gate tag.")],
    ) -> str:
        """Hold the first turn open while follow-up messages are queued."""
        return f"passed:{tag}"

    agent = scripted_turns_agent(
        turns=[
            {
                "kind": "tool_calls",
                "calls": [
                    {
                        "id": "call-gate",
                        "name": "wait_gate",
                        "arguments": {"tag": "t1"},
                    }
                ],
            },
            {"kind": "text", "text": "First turn done."},
            {"kind": "text", "text": "Handled queued burst."},
        ],
        tools=[wait_gate],
        session_id="interactive-queue-ops",
    )

    async def scenario() -> tuple[events.QueuedInputs, list[str]]:
        replies: list[str] = []
        snap: events.QueuedInputs | None = None
        async with agent.interactive() as session:
            await session.send("start turn one")

            async for message in session:
                if isinstance(message, merry.ToolCallBatch):
                    first = await session.send("draft one")
                    second = await session.send("draft two")
                    await session.queue.pause()
                    await first.update("edited one")
                    await second.remove()
                    snap = await session.queue.snapshot()
                    assert snap.paused is True
                    assert len(snap.items) == 1
                    assert snap.items[0].text == "edited one"
                    await agent.execute_tool_batch(message)
                elif isinstance(message.payload, events.AssistantMessagePayload):
                    replies.append(message.payload.text)
                    if len(replies) == 1:
                        still_paused = await session.queue.snapshot()
                        assert still_paused.paused is True
                        assert len(still_paused.items) == 1
                        await session.queue.flush()
                    elif len(replies) == 2:
                        break
        assert snap is not None
        return snap, replies

    snap, replies = asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))
    assert snap.items[0].text == "edited one"
    assert replies == ["First turn done.", "Handled queued burst."]


def test_interactive_queue_resume_dispatches_paused_inputs() -> None:
    @merry.tool
    async def wait_gate(
        tag: Annotated[str, Field(description="Gate tag.")],
    ) -> str:
        """Hold the first turn open while follow-up messages are queued."""
        return f"passed:{tag}"

    agent = scripted_turns_agent(
        turns=[
            {
                "kind": "tool_calls",
                "calls": [
                    {
                        "id": "call-gate",
                        "name": "wait_gate",
                        "arguments": {"tag": "t1"},
                    }
                ],
            },
            {"kind": "text", "text": "First turn done."},
            {"kind": "text", "text": "Resumed turn done."},
        ],
        tools=[wait_gate],
        session_id="interactive-queue-resume",
    )

    async def scenario() -> list[str]:
        replies: list[str] = []
        async with agent.interactive() as session:
            await session.send("start turn one")
            async for message in session:
                if isinstance(message, merry.ToolCallBatch):
                    queued = await session.send("queued follow-up")
                    await session.queue.pause()
                    await session.queue.update(queued.id, "updated follow-up")
                    await agent.execute_tool_batch(message)
                elif isinstance(message.payload, events.AssistantMessagePayload):
                    replies.append(message.payload.text)
                    if len(replies) == 1:
                        await session.queue.resume()
                    elif len(replies) == 2:
                        break
        return replies

    replies = asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))
    assert replies == ["First turn done.", "Resumed turn done."]


def test_interactive_interrupt_returns_to_idle_and_allows_save_session(
    tmp_path: Path,
) -> None:
    store_root = tmp_path / "sessions"
    agent = merry.Agent._from_native(pending_native_agent("interactive-interrupt"))

    async def scenario() -> list[events.InteractiveRunState]:
        states: list[events.InteractiveRunState] = []
        async with agent.interactive() as session:
            await session.send("Start pending work")
            interrupted = False
            async for message in session:
                if isinstance(message, merry.Event) and isinstance(
                    message.payload, events.InteractiveRunStateChangedPayload
                ):
                    states.append(message.payload.state)
                    if (
                        message.payload.state
                        is events.InteractiveRunState.RUNNING_MODEL
                        and not interrupted
                    ):
                        interrupted = True
                        await session.interrupt()
                    elif (
                        interrupted
                        and message.payload.state
                        is events.InteractiveRunState.WAITING_FOR_INPUT
                    ):
                        await session.save_session(store_root)
                        break
        return states

    states = asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))
    assert events.InteractiveRunState.RUNNING_MODEL in states
    assert states[-1] is events.InteractiveRunState.WAITING_FOR_INPUT
    assert (store_root / "interactive-interrupt" / "journal.jsonl").exists()


def test_interactive_close_with_unresolved_tool_batch_releases_session() -> None:
    @merry.tool
    async def hold_tool(
        tag: Annotated[str, Field(description="Hold tag.")],
    ) -> str:
        """Emit a tool batch that the caller abandons by closing."""
        return tag

    agent = scripted_turns_agent(
        turns=[
            {
                "kind": "tool_calls",
                "calls": [
                    {
                        "id": "call-abandoned",
                        "name": "hold_tool",
                        "arguments": {"tag": "x"},
                    }
                ],
            },
            {"kind": "text", "text": "Recovered on next run."},
        ],
        tools=[hold_tool],
        session_id="interactive-abandoned-batch",
    )

    async def scenario() -> str:
        async with agent.interactive() as session:
            await session.send("trigger tool")
            async for message in session:
                if isinstance(message, merry.ToolCallBatch):
                    # Exit without submitting the batch; close must settle and
                    # release the active step permit without deadlocking.
                    break

        result = await agent.run("follow up after close")
        assert result.status is merry.RunStatus.COMPLETED
        assert result.final_output is not None
        return result.final_output

    output = asyncio.run(asyncio.wait_for(scenario(), timeout=5.0))
    assert output == "Recovered on next run."
