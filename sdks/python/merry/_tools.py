"""Typed Python host tools and the explicit bridge registry."""

from __future__ import annotations

import asyncio
import functools
import inspect
from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass
from types import FunctionType
from typing import (
    TYPE_CHECKING,
    Generic,
    Protocol,
    TypeVar,
    get_type_hints,
)

from pydantic import BaseModel, ValidationError

from ._errors import (
    MerryConfigError,
    MerryErrorInfo,
    MerryToolError,
    ToolDomainError,
    tool_handler_error,
    tool_output_error,
)
from ._json import JsonObject, require_json_object
from ._models import (
    JsonContent,
    TextContent,
    ToolCall,
    ToolDiagnostic,
    ToolResult,
    _validate_text,
    _validate_tool_name,
)
from ._schema import validate_model

if TYPE_CHECKING:
    from ._models import ToolCallBatch, ToolSubmission

InputT = TypeVar("InputT", bound=BaseModel)
OutputT = TypeVar("OutputT", bound=BaseModel)
RunOutputT = TypeVar("RunOutputT", bound=BaseModel)


class ToolHandler(Protocol[InputT, OutputT]):
    """Callable contract for an async Pydantic tool handler."""

    def __call__(self, arguments: InputT, /) -> Awaitable[OutputT]: ...


class ToolRegistration(Protocol):
    """Structural contract shared by the typed tool and host registry."""

    @property
    def name(self) -> str: ...

    @property
    def description(self) -> str: ...

    @property
    def schema(self) -> JsonObject: ...

    async def execute(self, invocation: ToolCall) -> ToolResult: ...


@dataclass(frozen=True, slots=True)
class Tool(Generic[InputT, OutputT]):
    """An async Pydantic tool and its Rust bridge schema."""

    name: str
    description: str
    input_model: type[InputT]
    output_model: type[OutputT]
    handler: ToolHandler[InputT, OutputT]
    schema: JsonObject

    def __post_init__(self) -> None:
        _validate_tool_name(self.name)
        _validate_text("tool description", self.description, 4096)
        validate_model(self.input_model, "tool input model")
        validate_model(self.output_model, "tool output model")
        _validate_async_handler(self.handler)
        schema = require_json_object(self.schema, "tool input schema")
        expected_schema = require_json_object(
            self.input_model.model_json_schema(),
            "tool input schema",
        )
        if schema != expected_schema:
            raise ValueError("tool input schema must match the input model schema")
        object.__setattr__(self, "schema", schema)

    async def __call__(self, arguments: InputT) -> OutputT:
        return await self.handler(arguments)

    @classmethod
    def from_function(
        cls,
        handler: ToolHandler[InputT, OutputT],
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[InputT] | None = None,
        output_model: type[OutputT] | None = None,
    ) -> Tool[InputT, OutputT]:
        _validate_async_handler(handler)
        handler_name = _callable_name(handler)
        resolved_name = handler_name if name is None else name
        resolved_description = (
            _description(handler) if description is None else description
        )
        resolved_input = (
            _infer_input_model(handler) if input_model is None else input_model
        )
        resolved_output = (
            _infer_output_model(handler) if output_model is None else output_model
        )
        schema = require_json_object(
            resolved_input.model_json_schema(),
            "tool input schema",
        )
        return cls(
            name=resolved_name,
            description=resolved_description,
            input_model=resolved_input,
            output_model=resolved_output,
            handler=handler,
            schema=schema,
        )

    async def execute(self, invocation: ToolCall) -> ToolResult:
        try:
            arguments = self.input_model.model_validate(invocation.arguments)
        except ValidationError:
            return ToolResult.failed(
                invocation.id,
                TextContent("Tool arguments did not match the declared input schema."),
                ToolDiagnostic(
                    code="tool.input_invalid",
                    message="Tool arguments did not match the declared input schema.",
                ),
            )

        output: OutputT | None = None
        handler_failure: MerryToolError | None = None
        try:
            output = await self.handler(arguments)
        except asyncio.CancelledError:
            raise
        except ToolDomainError:
            raise
        except Exception:  # noqa: BLE001 - tool extensions must be isolated at this boundary.
            handler_failure = tool_handler_error()

        if handler_failure is not None:
            raise handler_failure
        if output is None:
            raise tool_handler_error()

        if not isinstance(output, self.output_model):
            raise tool_output_error()
        output_value = _serialize_tool_output(output)
        return ToolResult.succeeded(invocation.id, JsonContent.from_value(output_value))


def _serialize_tool_output(output: BaseModel) -> JsonObject:
    dump_failure: MerryToolError | None = None
    dumped: object = None
    try:
        dumped = output.model_dump(mode="json")
    except Exception:  # noqa: BLE001 - custom Pydantic serializers are extension code.
        dump_failure = tool_output_error()
    if dump_failure is not None:
        raise dump_failure

    value_failure: MerryToolError | None = None
    value: JsonObject | None = None
    try:
        value = require_json_object(dumped, "tool output")
    except (TypeError, ValueError):
        value_failure = tool_output_error()
    if value_failure is not None:
        raise value_failure
    if value is None:
        raise tool_output_error()
    return value


class ToolRegistry:
    """Host-owned callable registry; Rust remains the source of tool state."""

    def __init__(self, tools: Sequence[ToolRegistration]) -> None:
        self._tools: dict[str, ToolRegistration] = {}
        for tool in tools:
            if tool.name in self._tools:
                raise MerryConfigError(
                    MerryErrorInfo(
                        code="tool.duplicate_registration",
                        domain="tool",
                        message=f"Tool {tool.name!r} is already registered.",
                        hint="Use one unique provider-visible name per Agent.",
                        retryability="user_action_required",
                    )
                )
            self._tools[tool.name] = tool

    async def execute(self, batch: ToolCallBatch[RunOutputT]) -> ToolSubmission:
        results: list[ToolResult] = []
        for invocation in batch.invocations:
            if invocation.name not in self._tools:
                raise MerryToolError(
                    MerryErrorInfo(
                        code="tool.not_registered",
                        domain="tool",
                        message=f"Tool {invocation.name!r} is not registered in this host.",
                        hint="Register the same Tool with the AgentBuilder before building.",
                        retryability="user_action_required",
                    )
                )
            tool = self._tools[invocation.name]
            try:
                result = await tool.execute(invocation)
            except ToolDomainError as error:
                result = ToolResult.from_domain_error(invocation.id, error)
            results.append(result)
        return await batch.submit(results)


def _description(handler: Callable[..., object]) -> str:
    handler_name = _callable_name(handler)
    doc = inspect.getdoc(handler)
    if doc is None:
        raise ValueError(
            f"Tool function {handler_name} needs a docstring or description=..."
        )
    first_paragraph = doc.split("\n\n", 1)[0].strip()
    if not first_paragraph:
        raise ValueError(
            f"Tool function {handler_name} needs a docstring or description=..."
        )
    return " ".join(first_paragraph.split())


def _callable_name(handler: Callable[..., object]) -> str:
    if isinstance(handler, FunctionType):
        return handler.__name__
    if isinstance(handler, functools.partial):
        return _callable_name(handler.func)
    return type(handler).__name__


def _validate_async_handler(handler: Callable[..., object]) -> None:
    if not callable(handler):
        raise TypeError("Merry tools must be callable")
    if inspect.iscoroutinefunction(handler):
        return
    if inspect.iscoroutinefunction(type(handler).__call__):
        return
    raise TypeError("Merry tools must be async callables")


def _type_hints(handler: Callable[..., object]) -> Mapping[str, object]:
    handler_name = _callable_name(handler)
    try:
        hints = get_type_hints(_annotation_target(handler))
    except (NameError, TypeError) as error:
        raise TypeError(f"Could not resolve annotations for {handler_name}.") from error
    return {key: value for key, value in hints.items()}


def _infer_input_model(handler: Callable[..., object]) -> type[BaseModel]:
    handler_name = _callable_name(handler)
    parameters = [
        parameter
        for parameter in inspect.signature(handler).parameters.values()
        if parameter.kind
        in (
            inspect.Parameter.POSITIONAL_ONLY,
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.KEYWORD_ONLY,
        )
    ]
    if len(parameters) != 1:
        raise ValueError(
            f"Tool function {handler_name} must declare exactly one Pydantic input model."
        )
    parameter = parameters[0]
    annotation = _type_hints(handler).get(parameter.name)
    if annotation is None:
        raise TypeError(f"Tool input parameter {parameter.name!r} must be annotated.")
    return _model_from_annotation(annotation, f"{handler_name}.{parameter.name}")


def _infer_output_model(handler: Callable[..., object]) -> type[BaseModel]:
    handler_name = _callable_name(handler)
    annotation = _type_hints(handler).get("return")
    if annotation is None:
        raise TypeError(f"Tool function {handler_name} must have a return annotation.")
    return _model_from_annotation(annotation, f"{handler_name}.return")


def _model_from_annotation(annotation: object, label: str) -> type[BaseModel]:
    if isinstance(annotation, type) and issubclass(annotation, BaseModel):
        return annotation
    raise TypeError(f"{label} must be a Pydantic BaseModel subclass.")


def _annotation_target(handler: Callable[..., object]) -> Callable[..., object]:
    if isinstance(handler, FunctionType):
        return handler
    if isinstance(handler, functools.partial):
        return _annotation_target(handler.func)
    return type(handler).__call__
