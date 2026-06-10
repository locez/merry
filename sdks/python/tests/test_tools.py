import asyncio

import pytest

import merry
from _support import (
    register_static_tool_exception,
    register_static_tool_failure,
    runtime_with_scripted_tool_call,
)


async def _assert_scripted_tool_domain_failure_resolves_tool_and_continues():
    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="Order was not found.",
    )

    register_static_tool_failure(
        runtime,
        name="lookup_order",
        description="Look up an order.",
        diagnostic_code="tool.domain_failed",
        message="Order was not found.",
        content={"found": False},
    )

    result = await runtime.run("Check order A123.")

    assert result.status == "completed"
    resolved = [
        event
        for event in result.events
        if event["type"] == "tool_call_finished"
    ]
    assert resolved[0]["result"]["status"] == "failed"
    assert resolved[0]["result"]["diagnostic"]["code"] == "tool.domain_failed"


def test_scripted_tool_domain_failure_resolves_tool_and_continues():
    asyncio.run(_assert_scripted_tool_domain_failure_resolves_tool_and_continues())


async def _assert_scripted_tool_executor_exception_raises_tool_error():
    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="unreachable",
    )
    register_static_tool_exception(
        runtime,
        name="lookup_order",
        description="Look up an order.",
        message="database unavailable",
    )

    with pytest.raises(merry.MerryToolError) as raised:
        await runtime.run("Check order A123.")

    assert raised.value.code == "tool.executor_exception"
    assert raised.value.domain == "tool"
    assert raised.value.retryability == "not_retryable"


def test_scripted_tool_executor_exception_raises_tool_error():
    asyncio.run(_assert_scripted_tool_executor_exception_raises_tool_error())


async def _assert_scripted_tool_executor_exception_sanitizes_multiline_message():
    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="unreachable",
    )
    register_static_tool_exception(
        runtime,
        name="lookup_order",
        description="Look up an order.",
        message="database unavailable\n" + ("traceback line " * 80),
    )

    with pytest.raises(merry.MerryToolError) as raised:
        await runtime.run("Check order A123.")

    assert raised.value.code == "tool.executor_exception"
    assert raised.value.domain == "tool"
    assert "\n" not in raised.value.info.message
    assert "database unavailable" in raised.value.info.message
    assert len(raised.value.info.message) < 512


def test_scripted_tool_executor_exception_sanitizes_multiline_message():
    asyncio.run(_assert_scripted_tool_executor_exception_sanitizes_multiline_message())
