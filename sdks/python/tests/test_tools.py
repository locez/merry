from __future__ import annotations

import asyncio

import pytest
from _support import scripted_agent
from pydantic import BaseModel, ConfigDict, Field

import merry


class LookupInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier.")


class LookupOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: str = Field(description="Current order status.")


def test_tool_decorator_executes_through_rust_batch_handoff() -> None:
    calls: list[str] = []

    async def scenario() -> merry.RunResult[BaseModel]:
        builder = merry.AgentBuilder("decorated-tool")

        @builder.tool
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
        if isinstance(event.payload, merry.ToolCallFinishedPayload)
    )
    payload = finished_event.payload
    assert isinstance(payload, merry.ToolCallFinishedPayload)
    result_data = payload.result
    assert result_data.status is merry.RuntimeToolResultStatus.SUCCEEDED


def test_tool_domain_error_is_recorded_and_model_loop_continues() -> None:
    async def scenario() -> merry.RunResult[BaseModel]:
        builder = merry.AgentBuilder("domain-tool")

        @builder.tool
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
        if isinstance(event.payload, merry.ToolCallFinishedPayload)
    )
    payload = finished_event.payload
    assert isinstance(payload, merry.ToolCallFinishedPayload)
    result_data = payload.result
    assert result_data.status is merry.RuntimeToolResultStatus.FAILED
    assert result_data.diagnostic == merry.EventDiagnostic(
        code="order.not_found",
        message="Order missing was not found.",
    )


def test_unexpected_tool_exception_is_typed_and_cancels_run() -> None:
    secret = "database-password-that-must-not-leak"

    async def scenario() -> None:
        builder = merry.AgentBuilder("exception-tool")

        @builder.tool
        async def lookup_order(args: LookupInput) -> LookupOutput:
            """Look up an order by id."""
            raise RuntimeError(f"database unavailable: {secret}")

        agent = scripted_agent(
            lookup_order,
            arguments={"order_id": "A123"},
            session_id="exception-tool",
        )
        with pytest.raises(merry.MerryToolError) as raised:
            await agent.run("Check order A123.")
        assert raised.value.code == "tool.handler_exception"
        assert raised.value.retryability == "not_retryable"
        assert secret not in str(raised.value)
        assert secret not in raised.value.info.message
        assert raised.value.__cause__ is None
        assert raised.value.__context__ is None

    asyncio.run(scenario())


def test_tool_requires_described_models() -> None:
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
