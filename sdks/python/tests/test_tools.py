from __future__ import annotations

import asyncio
import logging
from types import MappingProxyType
from typing import Annotated, Literal

import pytest
from _support import parallel_tools_agent, scripted_agent
from pydantic import BaseModel, ConfigDict, Field

import merry
from merry import events


class LookupInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier.")


class LookupOutput(BaseModel):
    status: str


def test_tool_handler_accepts_wider_inputs_and_narrower_outputs() -> None:
    class RegionalLookupInput(LookupInput):
        region: str = Field(description="Order fulfillment region.")

    class DetailedLookupOutput(LookupOutput):
        order_id: str

    async def lookup(arguments: LookupInput) -> DetailedLookupOutput:
        return DetailedLookupOutput(status="shipped", order_id=arguments.order_id)

    original: merry.ToolHandler[LookupInput, DetailedLookupOutput] = lookup
    compatible: merry.ToolHandler[RegionalLookupInput, LookupOutput] = original
    tool = merry.Tool.from_function(
        compatible,
        name="lookup_order",
        description="Look up an order using a compatible handler.",
        input_model=RegionalLookupInput,
        output_model=LookupOutput,
    )

    output = asyncio.run(tool(RegionalLookupInput(order_id="A123", region="west")))

    assert isinstance(output, DetailedLookupOutput)
    assert output.status == "shipped"
    assert output.order_id == "A123"


def test_standalone_tool_supports_multi_param_signature_and_relaxed_outputs() -> None:
    @merry.tool
    async def get_weather(
        city: Annotated[str, Field(description="City to query.")],
        unit: Annotated[
            Literal["c", "f"], Field(description="Temperature unit.")
        ] = "c",
    ) -> str:
        """Fetch weather for a city."""
        return f"{city}:22{unit}"

    agent = scripted_agent(
        get_weather,
        arguments={"city": "Tokyo"},
        final_text="Tokyo is 22c.",
        session_id="multi-param-tool",
    )
    result = asyncio.run(agent.run("Check Tokyo weather."))

    assert result.status is merry.RunStatus.COMPLETED
    finished = next(
        event.payload
        for event in result.events
        if isinstance(event.payload, events.ToolCallFinishedPayload)
    )
    assert finished.result.status is events.RuntimeToolResultStatus.SUCCEEDED
    assert finished.output == events.RuntimeToolOutput(
        kind=events.RuntimeToolOutputKind.TEXT,
        value="Tokyo:22c",
    )


def test_zero_parameter_tool_returning_json_dict_works() -> None:
    @merry.tool
    async def server_status() -> dict[str, str]:
        """Return current server health."""
        return {"state": "ok"}

    result = asyncio.run(
        server_status.execute(merry.ToolCall("call-1", "server_status", {}))
    )

    assert result.diagnostic is None
    assert result.content == merry.JsonContent('{"state":"ok"}')


def test_tool_returning_tuple_and_readonly_mapping_serializes_as_json() -> None:
    @merry.tool
    async def list_tags() -> tuple[str, ...]:
        """Return immutable tuple of tags."""
        return ("alpha", "beta")

    @merry.tool
    async def read_counts() -> MappingProxyType[str, int]:
        """Return read-only mapping of counts."""
        return MappingProxyType({"ok": 2})

    tuple_result = asyncio.run(
        list_tags.execute(merry.ToolCall("call-1", "list_tags", {}))
    )
    mapping_result = asyncio.run(
        read_counts.execute(merry.ToolCall("call-2", "read_counts", {}))
    )

    assert tuple_result.content == merry.JsonContent('["alpha","beta"]')
    assert mapping_result.content == merry.JsonContent('{"ok":2}')


def test_parallel_safe_tools_execute_concurrently_and_exclusive_acts_as_barrier() -> (
    None
):
    first_entered = asyncio.Event()
    second_entered = asyncio.Event()
    trace: list[str] = []

    @merry.tool(parallel_safe=True)
    async def fetch_a(
        key: Annotated[str, Field(description="Lookup key.")],
    ) -> str:
        """Fetch record A concurrently."""
        trace.append(f"start:{key}")
        first_entered.set()
        await asyncio.wait_for(second_entered.wait(), timeout=2.0)
        trace.append(f"end:{key}")
        return f"a:{key}"

    @merry.tool(parallel_safe=True)
    async def fetch_b(
        key: Annotated[str, Field(description="Lookup key.")],
    ) -> str:
        """Fetch record B concurrently."""
        trace.append(f"start:{key}")
        second_entered.set()
        await asyncio.wait_for(first_entered.wait(), timeout=2.0)
        trace.append(f"end:{key}")
        return f"b:{key}"

    @merry.tool(parallel_safe=False)
    async def commit_step(
        tag: Annotated[str, Field(description="Commit marker.")],
    ) -> str:
        """Commit after concurrent reads complete."""
        trace.append(f"exclusive:{tag}")
        return f"committed:{tag}"

    agent = parallel_tools_agent(
        [fetch_a, fetch_b, commit_step],
        calls=[
            {"id": "call-1", "name": "fetch_a", "arguments": {"key": "k1"}},
            {"id": "call-2", "name": "fetch_b", "arguments": {"key": "k2"}},
            {"id": "call-3", "name": "commit_step", "arguments": {"tag": "c1"}},
        ],
        final_text="All three finished.",
        session_id="parallel-wave-tools",
    )

    result = asyncio.run(agent.run("Run two parallel reads then commit."))

    assert result.status is merry.RunStatus.COMPLETED
    assert result.final_output == "All three finished."
    assert set(trace[:2]) == {"start:k1", "start:k2"}
    assert trace[-1] == "exclusive:c1"


def test_tool_decorator_executes_through_rust_batch_handoff() -> None:
    calls: list[str] = []

    async def scenario() -> merry.RunResult[BaseModel]:
        @merry.tool
        async def lookup_order(args: LookupInput) -> LookupOutput:
            """Look up an order by id."""
            calls.append(args.order_id)
            return LookupOutput(status="shipped")

        agent = scripted_agent(
            lookup_order,
            arguments={"order_id": "A123"},
            final_text="Order A123 shipped.",
            session_id="decorated-tool",
        )
        return await agent.run("Check order A123.")

    result = asyncio.run(scenario())

    assert calls == ["A123"]
    assert result.status is merry.RunStatus.COMPLETED
    assert result.final_output == "Order A123 shipped."
    finished_event = next(
        event
        for event in result.events
        if isinstance(event.payload, events.ToolCallFinishedPayload)
    )
    payload = finished_event.payload
    assert isinstance(payload, events.ToolCallFinishedPayload)
    assert payload.result.status is events.RuntimeToolResultStatus.SUCCEEDED


def test_agent_execute_tool_batch_reuses_registered_tools_in_stream() -> None:
    @merry.tool
    async def lookup_order(args: LookupInput) -> LookupOutput:
        """Look up an order by id."""
        return LookupOutput(status=f"found:{args.order_id}")

    async def scenario() -> merry.RunResult[BaseModel]:
        agent = scripted_agent(
            lookup_order,
            arguments={"order_id": "B456"},
            final_text="Done.",
            session_id="stream-execute-batch",
        )
        async with agent.stream("Check B456.") as run:
            async for message in run:
                if isinstance(message, merry.ToolCallBatch):
                    submission = await agent.execute_tool_batch(message)
                    assert submission is merry.ToolSubmission.ACCEPTED
            return await run.result()

    result = asyncio.run(scenario())
    assert result.status is merry.RunStatus.COMPLETED


def test_tool_domain_error_is_recorded_and_model_loop_continues() -> None:
    async def scenario() -> merry.RunResult[BaseModel]:
        @merry.tool
        async def lookup_order(args: LookupInput) -> LookupOutput:
            """Look up an order by id."""
            raise merry.ToolDomainError(
                merry.MerryErrorInfo(
                    code="order.not_found",
                    domain="tool",
                    message=f"Order {args.order_id} was not found.",
                    retryability="not_retryable",
                ),
                {"found": False},
            )

        agent = scripted_agent(
            lookup_order,
            arguments={"order_id": "missing"},
            final_text="The order was not found.",
            session_id="domain-tool",
        )
        return await agent.run("Check the missing order.")

    result = asyncio.run(scenario())

    assert result.status is merry.RunStatus.COMPLETED
    assert result.final_output == "The order was not found."
    finished_event = next(
        event
        for event in result.events
        if isinstance(event.payload, events.ToolCallFinishedPayload)
    )
    payload = finished_event.payload
    assert isinstance(payload, events.ToolCallFinishedPayload)
    assert payload.result.status is events.RuntimeToolResultStatus.FAILED
    assert payload.result.diagnostic == events.EventDiagnostic(
        code="order.not_found",
        message="Order missing was not found.",
    )


def test_unexpected_tool_exception_is_typed_logged_without_secrets_and_cancels_run(
    caplog: pytest.LogCaptureFixture,
) -> None:
    secret = "database-password-that-must-not-leak"

    async def scenario() -> None:
        @merry.tool
        async def lookup_order(args: LookupInput) -> LookupOutput:
            """Look up an order by id."""
            raise RuntimeError(f"database unavailable: {secret}")

        agent = scripted_agent(
            lookup_order,
            arguments={"order_id": "A123"},
            session_id="exception-tool",
        )
        with (
            caplog.at_level(logging.WARNING, logger="merry._tools"),
            pytest.raises(merry.MerryToolError) as raised,
        ):
            await agent.run("Check order A123.")
        assert raised.value.code == "tool.handler_exception"
        assert raised.value.retryability == "not_retryable"
        assert secret not in str(raised.value)
        assert secret not in raised.value.info.message
        assert raised.value.__cause__ is None
        assert raised.value.__context__ is None
        assert any("RuntimeError" in record.message for record in caplog.records)
        assert all(secret not in record.message for record in caplog.records)

    asyncio.run(scenario())


def test_tool_requires_described_input_models() -> None:
    class UndescribedInput(BaseModel):
        value: str

    async def missing_description(args: UndescribedInput) -> LookupOutput:
        """A tool with an invalid input schema."""
        return LookupOutput(status=args.value)

    with pytest.raises(ValueError, match=r"Field\(description=\.\.\.\)"):
        merry.Tool.from_function(
            missing_description,
            input_model=UndescribedInput,
            output_model=LookupOutput,
        )


def test_invalid_tool_input_becomes_a_typed_failure_result() -> None:
    async def lookup(args: LookupInput) -> LookupOutput:
        """Look up an order."""
        return LookupOutput(status=args.order_id)

    async def scenario() -> merry.ToolResult:
        tool = merry.Tool.from_function(lookup)
        return await tool.execute(
            merry.ToolCall("invalid-input", "lookup", {"unexpected": "value"})
        )

    result = asyncio.run(scenario())

    assert result.diagnostic is not None
    assert result.diagnostic.code == "tool.input_invalid"
    assert result.to_wire()["status"] == "failed"


def test_tool_result_keeps_extended_domain_diagnostic_until_wire_projection() -> None:
    info = merry.MerryErrorInfo(
        code="order.not_found",
        domain="tool",
        message="The order was not found.",
        hint="Ask for a different order id.",
        retryability="user_action_required",
        context={"call_id": "call-1"},
    )

    result = merry.ToolResult.failed("call-1", merry.TextContent("not found"), info)

    assert result.diagnostic is info
    assert result.diagnostic.hint == "Ask for a different order id."
    assert result.diagnostic.retryability == "user_action_required"
    assert result.diagnostic.context == {"call_id": "call-1"}
    assert result.to_wire()["diagnostic"] == {
        "code": "order.not_found",
        "message": "The order was not found.",
    }


def test_tool_schema_rejects_bare_collections_and_non_strict_models() -> None:
    class BareInput(BaseModel):
        model_config = ConfigDict(extra="forbid")

        values: dict = Field(description="Unbounded values.")

    class NonStrictInput(BaseModel):
        value: str = Field(description="A value.")

    async def bare_tool(args: BareInput) -> LookupOutput:
        """Reject an unbounded mapping."""
        return LookupOutput(status=str(args.values))

    async def non_strict_tool(args: NonStrictInput) -> LookupOutput:
        """Reject a non-strict model."""
        return LookupOutput(status=args.value)

    with pytest.raises(TypeError, match="parameterized collection"):
        merry.Tool.from_function(
            bare_tool,
            input_model=BareInput,
            output_model=LookupOutput,
        )
    with pytest.raises(ValueError, match="extra='forbid'"):
        merry.Tool.from_function(
            non_strict_tool,
            input_model=NonStrictInput,
            output_model=LookupOutput,
        )


def test_nested_tool_models_require_descriptions_at_every_level() -> None:
    class NestedInput(BaseModel):
        value: str

    class OuterInput(BaseModel):
        nested: NestedInput = Field(description="Nested input values.")

    async def nested_tool(args: OuterInput) -> LookupOutput:
        """Use a nested input model."""
        return LookupOutput(status=args.nested.value)

    with pytest.raises(ValueError, match=r"NestedInput\.value"):
        merry.Tool.from_function(
            nested_tool,
            input_model=OuterInput,
            output_model=LookupOutput,
        )


def test_tool_registry_rejects_duplicate_names() -> None:
    async def lookup(args: LookupInput) -> LookupOutput:
        """Look up an order."""
        return LookupOutput(status=args.order_id)

    tool = merry.Tool.from_function(lookup)

    with pytest.raises(merry.MerryConfigError) as raised:
        merry.ToolRegistry([tool, tool])

    assert raised.value.code == "tool.duplicate_registration"
