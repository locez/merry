from __future__ import annotations

from typing import Annotated

from pydantic import BaseModel, ConfigDict, Field

import merry


class FutureInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    value: str = Field(description="Value passed to the tool.")


class FutureOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    value: str = Field(description="Value returned by the tool.")


def test_tool_from_function_resolves_future_annotations() -> None:
    async def echo_value(args: FutureInput) -> FutureOutput:
        """Echo a value."""
        return FutureOutput(value=args.value)

    tool = merry.Tool.from_function(echo_value)

    assert tool.name == "echo_value"
    assert tool.description == "Echo a value."
    assert tool.input_model is FutureInput
    assert tool.output_model is FutureOutput


def test_multi_param_tool_resolves_future_annotations() -> None:
    @merry.tool
    async def greet(
        name: Annotated[str, Field(description="Recipient name.")],
        times: Annotated[int, Field(description="Repeat count.")] = 1,
    ) -> str:
        """Greet a user."""
        return ",".join([name] * times)

    assert greet.name == "greet"
    assert "Recipient name." in str(greet.schema)
    assert "Repeat count." in str(greet.schema)
