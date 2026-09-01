from __future__ import annotations

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
