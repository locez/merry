from __future__ import annotations

import asyncio

import pytest
from _support import echo_request_agent, scripted_agent
from pydantic import BaseModel

import merry
from merry._event_payloads import (
    AssistantMessageDeltaPayload,
    AssistantMessagePayload,
    ReasoningDeltaPayload,
)
from merry._event_types import EventSource, RuntimeToolOutput, RuntimeToolOutputKind


class _DummyOutput(BaseModel):
    answer: str


def test_prompt_profile_and_block_sanitize_freeform_control_chars() -> None:
    block = merry.PromptBlock("persona", "nick\x07\r\nrule\x1b\0\tkeep")
    assert block.text == "nick\nrule\tkeep"

    profile = merry.PromptProfile(
        base_instructions="base\x07\r\nline\0",
        progress_commentary_instructions="progress\x0c\rstep",
        stable_blocks=(block,),
    )
    assert profile.base_instructions == "base\nline"
    assert profile.progress_commentary_instructions == "progress\nstep"

    with pytest.raises(ValueError, match="must not be blank"):
        merry.PromptBlock("persona", "\x07\0\x1b")

    with pytest.raises(ValueError, match="control characters"):
        merry.PromptBlock("bad\x07tag", "content")


def test_tool_text_content_and_hooks_sanitize_freeform_control_chars() -> None:
    content = merry.TextContent("chat log\r\nuser\x07: hi\x1b[31m\0\tend")
    assert content.text == "chat log\nuser: hi[31m\tend"

    before_model = merry.BeforeModelDecision.continue_normally(
        dynamic_steering_note="steer\x07\r\nnow\0"
    )
    assert before_model.dynamic_steering_note == "steer\nnow"

    stop_decision = merry.BeforeModelDecision.stop_turn(
        "stop\x07reason\0", final_output="final\x07text\r\n"
    )
    assert stop_decision.reason == "stopreason"
    assert stop_decision.final_output == "finaltext\n"

    deny_decision = merry.BeforeToolDecision.deny("denied\x07\r\nfor policy\0")
    assert deny_decision.message == "denied\nfor policy"

    with pytest.raises(ValueError, match="must not be blank"):
        merry.BeforeToolDecision.deny("\x07\0\x1b")


def test_event_payloads_and_run_result_sanitize_freeform_text() -> None:
    source = EventSource(session_id="sanitize-events", sequence=1)
    msg = AssistantMessagePayload(text="reply\x07\r\n\0ok", source=source)
    delta = AssistantMessageDeltaPayload(delta="chunk\x07\0", source=source)
    reasoning = ReasoningDeltaPayload(delta="think\x0b\r\nstep", source=source)
    tool_out = RuntimeToolOutput(kind=RuntimeToolOutputKind.TEXT, value="out\x07\r\n\0")

    assert msg.text == "reply\r\nok"
    assert delta.delta == "chunk"
    assert reasoning.delta == "think\r\nstep"
    assert tool_out.value == "out\r\n"

    result = merry.RunResult[_DummyOutput](
        status=merry.RunStatus.COMPLETED,
        events=(),
        model_turns_run=1,
        final_output="done\x07\r\n\0",
        final_output_json=None,
        structured_output=None,
        session_usage=None,
    )
    assert result.final_output == "done\r\n"


def test_native_agent_run_and_host_tool_sanitize_dirty_text() -> None:
    @merry.tool(name="read_chat")
    async def read_chat() -> str:
        """Read chat history containing control characters."""
        return "群友\x07: hello\r\nworld\x1b\0"

    async def scenario() -> None:
        agent = echo_request_agent(
            session_id="dirty-user-input",
            prompt=merry.PromptProfile(base_instructions="system\x07\r\nrules\0"),
        )
        result = await agent.run("群友\x07你好\r\n世界\x1b\0\t!")
        assert result.status is merry.RunStatus.COMPLETED
        assert result.final_output is not None
        assert "system\\nrules" in result.final_output
        assert "\x07" not in result.final_output
        assert "\0" not in result.final_output

        with pytest.raises(merry.MerryError):
            await agent.run("\x07\0\x1b")

        async with agent.interactive() as session:
            await session.queue.pause()
            queued = await session.send("群友\x07你好\r\n世界\x1b\0\t!")
            assert queued.text == "群友你好\n世界\t!"
            with pytest.raises(merry.MerryError):
                await session.send("\x07\0\x1b")

        tool_agent = scripted_agent(
            read_chat,
            arguments={},
            final_text="handled",
            session_id="dirty-tool-output",
        )
        tool_result = await tool_agent.run("check chat")
        assert tool_result.status is merry.RunStatus.COMPLETED
        assert tool_result.final_output == "handled"

    asyncio.run(scenario())
