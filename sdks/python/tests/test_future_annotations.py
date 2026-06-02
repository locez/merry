from __future__ import annotations

import merry
from pydantic import BaseModel, ConfigDict, Field


class FutureInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    value: str = Field(description="Value passed to the future-annotated tool.")


class FutureOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    value: str = Field(description="Value returned by the future-annotated tool.")


def test_tool_from_function_resolves_future_annotations():
    async def echo_value(args: FutureInput) -> FutureOutput:
        """Echo a value."""
        return FutureOutput(value=args.value)

    tool = merry.Tool.from_function(echo_value)

    assert tool.name == "echo_value"
    assert tool.description == "Echo a value."
    assert tool.input_model is FutureInput
    assert tool.output_model is FutureOutput
