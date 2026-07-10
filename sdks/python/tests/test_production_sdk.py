import asyncio

import pytest

import merry
from _support import runtime_with_scripted_tool_call, runtime_with_scripted_tool_calls
from pydantic import BaseModel, ConfigDict, Field


class LookupOrderInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier to look up.")


class LookupOrderOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier that was looked up.")
    status: str = Field(description="Current fulfillment status for the order.")


class OrderStatusFinalOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier in the final answer.")
    status: str = Field(description="Final fulfillment status for the order.")


def test_runtime_config_constructs_openai_runtime():
    config = merry.RuntimeConfig(
        provider=merry.OpenAICompatibleProvider(
            api_key="sk-test",
            model="gpt-test",
            base_url="https://api.example.test/v1",
            retry=merry.ProviderRetryConfig(max_attempts=2, initial_delay_ms=1, max_delay_ms=1),
        ),
    )

    runtime = merry.Runtime(config=config)

    assert isinstance(runtime, merry.Runtime)


def test_runtime_config_constructs_anthropic_runtime():
    config = merry.RuntimeConfig(
        provider=merry.AnthropicProvider(
            api_key="sk-ant-test",
            model="claude-test",
            base_url="https://anthropic.example.test",
            default_max_output_tokens=2048,
        ),
    )

    runtime = merry.Runtime(config=config)

    assert isinstance(runtime, merry.Runtime)


def test_runtime_config_session_id_is_honored():
    config = merry.RuntimeConfig(
        provider=merry.OpenAICompatibleProvider(
            api_key="sk-test",
            model="gpt-test",
            base_url="https://api.example.test/v1",
        ),
        session_id="configured-session_1",
    )

    runtime = merry.Runtime(config=config)

    assert runtime.session_id == "configured-session_1"


def test_runtime_config_rejects_unwired_workspace_config():
    config = merry.RuntimeConfig(
        provider=merry.OpenAICompatibleProvider(
            api_key="sk-test",
            model="gpt-test",
            base_url="https://api.example.test/v1",
        ),
        workspace=merry.WorkspaceConfig(root="."),
    )

    with pytest.raises(merry.MerryConfigError) as raised:
        merry.Runtime(config=config)

    assert raised.value.code == "config.workspace_unsupported"
    assert raised.value.retryability == "user_action_required"


async def _assert_python_tool_executes_through_runtime_loop():
    calls = []

    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        calls.append(args)
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="Order A123 shipped.",
    )
    runtime.register_tool(
        merry.Tool.bridge(
            lookup_order,
            input_model=LookupOrderInput,
            output_model=LookupOrderOutput,
            name="lookup_order",
            description="Look up an order by id.",
        )
    )

    result = await runtime.run("Check order A123.")

    assert result.status == "completed"
    assert result.model_turns_run == 2
    assert result.final_output == "Order A123 shipped."
    assert calls == [LookupOrderInput(order_id="A123")]
    resolved = [
        event
        for event in result.events
        if event["type"] == "tool_call_finished"
    ]
    bridge_requests = [
        event
        for event in result.events
        if event["type"] == "bridge_tool_call_requested"
    ]
    assert bridge_requests == []
    assert resolved[0]["result"]["status"] == "succeeded"
    event_types = [event["type"] for event in result.events]
    assert event_types == [
        "session_started",
        "step_started",
        "tool_call_started",
        "tool_call_finished",
        "step_started",
        "assistant_message",
        "step_completed",
    ]


def test_python_tool_executes_through_runtime_loop():
    asyncio.run(_assert_python_tool_executes_through_runtime_loop())


async def _assert_runtime_stream_executes_python_tool_and_returns_final_result():
    calls = []

    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        """Look up an order by id."""
        calls.append(args.order_id)
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="Order A123 shipped.",
    )
    runtime.register_tool(lookup_order)

    stream = runtime.stream("Check order A123.")
    event_types = []
    async for event in stream:
        event_types.append(event["type"])

    result = await stream.result()

    assert result.status == "completed"
    assert result.model_turns_run == 2
    assert result.final_output == "Order A123 shipped."
    assert calls == ["A123"]
    assert "bridge_tool_call_requested" not in event_types
    assert "tool_call_started" in event_types
    assert "tool_call_finished" in event_types
    assert event_types[-1] == "step_completed"


def test_runtime_stream_executes_python_tool_and_returns_final_result():
    asyncio.run(_assert_runtime_stream_executes_python_tool_and_returns_final_result())


async def _assert_runtime_run_returns_pydantic_final_output_model():
    runtime = runtime_with_scripted_tool_call(
        tool_name="merry_final_output",
        arguments={"order_id": "A123", "status": "shipped"},
        final_text="This text should not be used.",
    )

    result = await runtime.run(
        "Return a structured order status.",
        final_output_model=OrderStatusFinalOutput,
    )

    assert result.status == "completed"
    assert result.model_turns_run == 1
    assert result.final_output == OrderStatusFinalOutput(order_id="A123", status="shipped")
    assert result.final_output_json == '{"order_id":"A123","status":"shipped"}'
    assert [event["type"] for event in result.events] == [
        "session_started",
        "step_started",
        "tool_call_started",
        "final_output_recorded",
    ]


def test_runtime_run_returns_pydantic_final_output_model():
    asyncio.run(_assert_runtime_run_returns_pydantic_final_output_model())


async def _assert_runtime_stream_executes_python_tool_before_final_output_model():
    calls = []

    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        """Look up an order by id."""
        calls.append(args.order_id)
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    runtime = runtime_with_scripted_tool_calls(
        calls=[
            {"name": "lookup_order", "arguments": {"order_id": "A123"}},
            {
                "name": "merry_final_output",
                "arguments": {"order_id": "A123", "status": "shipped"},
            },
        ],
        final_text="This text should not be used.",
    )
    runtime.register_tool(lookup_order)

    stream = runtime.stream(
        "Look up order A123 and return structured status.",
        final_output_model=OrderStatusFinalOutput,
    )
    event_types = []
    async for event in stream:
        event_types.append(event["type"])

    result = await stream.result()

    assert result.status == "completed"
    assert result.model_turns_run == 2
    assert result.final_output == OrderStatusFinalOutput(order_id="A123", status="shipped")
    assert result.final_output_json == '{"order_id":"A123","status":"shipped"}'
    assert calls == ["A123"]
    assert event_types == [
        "session_started",
        "step_started",
        "tool_call_started",
        "tool_call_finished",
        "step_started",
        "tool_call_started",
        "final_output_recorded",
    ]
    assert [event["type"] for event in result.events] == event_types


def test_runtime_stream_executes_python_tool_before_final_output_model():
    asyncio.run(_assert_runtime_stream_executes_python_tool_before_final_output_model())


async def _assert_final_output_model_blocks_plain_text_completion():
    runtime = runtime_with_scripted_tool_calls(
        calls=[],
        final_text="Order A123 shipped.",
    )

    result = await runtime.run(
        "Return structured order status.",
        final_output_model=OrderStatusFinalOutput,
    )

    assert result.status == "blocked"
    assert result.final_output is None
    assert result.final_output_json is None


def test_final_output_model_blocks_plain_text_completion():
    asyncio.run(_assert_final_output_model_blocks_plain_text_completion())


def test_pydantic_tool_schema_uses_field_descriptions():
    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    tool = merry.Tool.bridge(
        lookup_order,
        input_model=LookupOrderInput,
        output_model=LookupOrderOutput,
        name="lookup_order",
        description="Look up an order by id.",
    )

    assert tool.schema["properties"]["order_id"]["description"] == (
        "Stable order identifier to look up."
    )
    assert tool.schema["additionalProperties"] is False
    assert tool.output_model is LookupOrderOutput


def test_pydantic_tool_requires_descriptions_for_all_fields():
    class MissingDescriptionInput(BaseModel):
        order_id: str

    async def lookup_order(args: MissingDescriptionInput) -> LookupOrderOutput:
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    with pytest.raises(ValueError, match="MissingDescriptionInput.order_id"):
        merry.Tool.bridge(
            lookup_order,
            input_model=MissingDescriptionInput,
            output_model=LookupOrderOutput,
            name="lookup_order",
            description="Look up an order by id.",
        )


def test_pydantic_tool_requires_output_field_descriptions():
    class MissingDescriptionOutput(BaseModel):
        order_id: str

    async def lookup_order(args: LookupOrderInput) -> MissingDescriptionOutput:
        return MissingDescriptionOutput(order_id=args.order_id)

    with pytest.raises(ValueError, match="MissingDescriptionOutput.order_id"):
        merry.Tool.bridge(
            lookup_order,
            input_model=LookupOrderInput,
            output_model=MissingDescriptionOutput,
            name="lookup_order",
            description="Look up an order by id.",
        )


def test_tool_from_function_infers_name_description_and_models():
    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        """Look up an order by id.

        Additional implementation notes do not enter the tool description.
        """
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    tool = merry.Tool.from_function(lookup_order)

    assert tool.name == "lookup_order"
    assert tool.description == "Look up an order by id."
    assert tool.input_model is LookupOrderInput
    assert tool.output_model is LookupOrderOutput
    assert tool.schema["properties"]["order_id"]["description"] == (
        "Stable order identifier to look up."
    )


async def _assert_runtime_register_tool_accepts_function():
    calls = []

    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        """Look up an order by id."""
        calls.append(args.order_id)
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="Order A123 shipped.",
    )
    runtime.register_tool(lookup_order)

    result = await runtime.run("Check order A123.")

    assert result.status == "completed"
    assert result.final_output == "Order A123 shipped."
    assert calls == ["A123"]


def test_runtime_register_tool_accepts_function():
    asyncio.run(_assert_runtime_register_tool_accepts_function())


async def _assert_runtime_tool_decorator_registers_function():
    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="Order A123 shipped.",
    )
    calls = []

    @runtime.tool
    async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
        """Look up an order by id."""
        calls.append(args.order_id)
        return LookupOrderOutput(order_id=args.order_id, status="shipped")

    result = await runtime.run("Check order A123.")

    assert result.status == "completed"
    assert calls == ["A123"]


def test_runtime_tool_decorator_registers_function():
    asyncio.run(_assert_runtime_tool_decorator_registers_function())
