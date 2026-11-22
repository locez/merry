"""Typed Python host tools and the explicit bridge registry."""

from __future__ import annotations

import asyncio
import functools
import inspect
import logging
from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass
from types import FunctionType
from typing import (
    TYPE_CHECKING,
    Literal,
    Protocol,
    TypeAlias,
    TypeVar,
    get_type_hints,
    overload,
)

if TYPE_CHECKING:
    from typing_extensions import TypeIs

from pydantic import BaseModel, ValidationError

from ._errors import (
    MerryConfigError,
    MerryErrorInfo,
    MerryToolError,
    ToolDomainError,
    tool_handler_error,
    tool_output_error,
)
from ._json import (
    JsonObject,
    JsonPrimitive,
    JsonValue,
    require_json_object,
    require_json_value,
)
from ._models import (
    JsonContent,
    TextContent,
    ToolCall,
    ToolCallBatch,
    ToolContent,
    ToolDiagnostic,
    ToolResult,
    ToolSubmission,
    _validate_text,
    _validate_tool_name,
)
from ._schema import synthesize_input_model, validate_model, validate_output_model

_LOGGER = logging.getLogger(__name__)

ToolOutputValue: TypeAlias = (
    BaseModel
    | ToolContent
    | JsonPrimitive
    | Sequence["ToolOutputValue"]
    | Mapping[str, "ToolOutputValue"]
)
OutputT_co = TypeVar("OutputT_co", bound=ToolOutputValue, covariant=True)
HandlerInputT_contra = TypeVar(
    "HandlerInputT_contra", bound=BaseModel, contravariant=True
)
ModelT = TypeVar("ModelT", bound=BaseModel)
_InvokeMode: TypeAlias = Literal["model", "kwargs"]


class ToolHandler(Protocol[HandlerInputT_contra, OutputT_co]):
    """Callable contract for a single-model async tool handler."""

    def __call__(self, arguments: HandlerInputT_contra, /) -> Awaitable[OutputT_co]: ...


class ToolRegistration(Protocol):
    """Structural contract shared by typed tools and the host registry."""

    @property
    def name(self) -> str: ...

    @property
    def description(self) -> str: ...

    @property
    def schema(self) -> JsonObject: ...

    @property
    def parallel_safe(self) -> bool: ...

    async def execute(self, invocation: ToolCall) -> ToolResult: ...


@dataclass(frozen=True, slots=True)
class Tool:
    """An async Python host tool and its validated Rust bridge schema."""

    name: str
    description: str
    input_model: type[BaseModel]
    handler: Callable[..., Awaitable[ToolOutputValue]]
    schema: JsonObject
    output_model: type[BaseModel] | None = None
    parallel_safe: bool = False
    _invoke_mode: _InvokeMode = "model"

    def __post_init__(self) -> None:
        _validate_tool_name(self.name)
        _validate_text("tool description", self.description, 4096)
        validate_model(self.input_model, "tool input model")
        if self.output_model is not None:
            validate_output_model(self.output_model, "tool output model")
        if not isinstance(self.parallel_safe, bool):
            raise TypeError("parallel_safe must be a boolean")
        _validate_async_handler(self.handler)
        schema = require_json_object(self.schema, "tool input schema")
        expected_schema = require_json_object(
            self.input_model.model_json_schema(),
            "tool input schema",
        )
        if schema != expected_schema:
            raise ValueError("tool input schema must match the input model schema")
        object.__setattr__(self, "schema", schema)

    async def __call__(self, *args: object, **kwargs: object) -> ToolOutputValue:
        return await self.handler(*args, **kwargs)

    @classmethod
    def from_function(
        cls,
        handler: Callable[..., Awaitable[ToolOutputValue]],
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[BaseModel] | None = None,
        output_model: type[BaseModel] | None = None,
        parallel_safe: bool = False,
    ) -> Tool:
        _validate_async_handler(handler)
        handler_name = _callable_name(handler)
        resolved_name = handler_name if name is None else name
        resolved_description = (
            _description(handler) if description is None else description
        )
        resolved_input, invoke_mode = _resolve_input_contract(
            handler,
            handler_name=handler_name,
            explicit_input_model=input_model,
        )
        resolved_output = _resolve_output_model(
            handler,
            explicit_output_model=output_model,
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
            parallel_safe=parallel_safe,
            _invoke_mode=invoke_mode,
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

        output: ToolOutputValue | None = None
        output_received = False
        handler_failure: MerryToolError | None = None
        try:
            if self._invoke_mode == "model":
                output = await self.handler(arguments)
            else:
                output = await self.handler(**dict(arguments))
            output_received = True
        except asyncio.CancelledError:
            raise
        except ToolDomainError:
            raise
        except Exception as error:  # noqa: BLE001 - tool extensions must be isolated at this boundary.
            _LOGGER.warning(
                "python tool handler failed unexpectedly; tool=%s error_type=%s",
                self.name,
                type(error).__name__,
            )
            handler_failure = tool_handler_error()

        if handler_failure is not None:
            raise handler_failure
        if not output_received:
            raise tool_handler_error()

        content = _encode_tool_output(output, self.output_model)
        return ToolResult.succeeded(invocation.id, content)


@overload
def tool(
    handler: Callable[..., Awaitable[ToolOutputValue]],
    /,
) -> Tool: ...


@overload
def tool(
    *,
    name: str | None = None,
    description: str | None = None,
    input_model: type[BaseModel] | None = None,
    output_model: type[BaseModel] | None = None,
    parallel_safe: bool = False,
) -> Callable[[Callable[..., Awaitable[ToolOutputValue]]], Tool]: ...


def tool(
    handler: Callable[..., Awaitable[ToolOutputValue]] | None = None,
    *,
    name: str | None = None,
    description: str | None = None,
    input_model: type[BaseModel] | None = None,
    output_model: type[BaseModel] | None = None,
    parallel_safe: bool = False,
) -> Tool | Callable[[Callable[..., Awaitable[ToolOutputValue]]], Tool]:
    """Create a standalone typed Merry tool from an async Python callable."""

    def decorate(function: Callable[..., Awaitable[ToolOutputValue]]) -> Tool:
        return Tool.from_function(
            function,
            name=name,
            description=description,
            input_model=input_model,
            output_model=output_model,
            parallel_safe=parallel_safe,
        )

    if handler is None:
        return decorate
    return decorate(handler)


def _encode_tool_output(
    output: ToolOutputValue,
    expected_model: type[BaseModel] | None,
) -> ToolContent:
    if expected_model is not None:
        if not isinstance(output, expected_model):
            raise tool_output_error()
        return JsonContent.from_value(_serialize_model_output(output))

    if isinstance(output, BaseModel):
        return JsonContent.from_value(_serialize_model_output(output))
    if isinstance(output, (TextContent, JsonContent)):
        return output
    if isinstance(output, str):
        try:
            return TextContent(output)
        except (TypeError, ValueError):
            raise tool_output_error() from None

    normalized: JsonValue | None = None
    json_failure: MerryToolError | None = None
    try:
        normalized = _normalize_tool_json_output(output)
    except (TypeError, ValueError):
        json_failure = tool_output_error()
    if json_failure is not None:
        raise json_failure
    try:
        return JsonContent.from_value(normalized)
    except (TypeError, ValueError):
        raise tool_output_error() from None


def _normalize_tool_json_output(value: object) -> JsonValue:
    if isinstance(value, BaseModel):
        return _serialize_model_output(value)
    if isinstance(value, Mapping):
        normalized_map: JsonObject = {}
        for key, item in value.items():
            if not isinstance(key, str):
                raise TypeError("tool output mapping keys must be strings")
            normalized_map[key] = _normalize_tool_json_output(item)
        return normalized_map
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes, bytearray)):
        return [_normalize_tool_json_output(item) for item in value]
    return require_json_value(value, "tool output")


def _serialize_model_output(output: BaseModel) -> JsonObject:
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
        for item in tools:
            if item.name in self._tools:
                raise MerryConfigError(
                    MerryErrorInfo(
                        code="tool.duplicate_registration",
                        domain="tool",
                        message=f"Tool {item.name!r} is already registered.",
                        hint="Use one unique provider-visible name per Agent.",
                        retryability="user_action_required",
                    )
                )
            self._tools[item.name] = item

    def __len__(self) -> int:
        return len(self._tools)

    def __contains__(self, name: object) -> bool:
        return isinstance(name, str) and name in self._tools

    def names(self) -> tuple[str, ...]:
        """Return registered tool names in registration order."""

        return tuple(self._tools.keys())

    async def execute(self, batch: ToolCallBatch) -> ToolSubmission:
        """Execute a batch, running adjacent parallel-safe calls concurrently."""

        resolved: list[tuple[ToolCall, ToolRegistration]] = []
        for invocation in batch.invocations:
            if invocation.name not in self._tools:
                raise MerryToolError(
                    MerryErrorInfo(
                        code="tool.not_registered",
                        domain="tool",
                        message=f"Tool {invocation.name!r} is not registered in this host.",
                        hint="Register the same Tool with the Agent before running.",
                        retryability="user_action_required",
                    )
                )
            resolved.append((invocation, self._tools[invocation.name]))

        results: list[ToolResult] = []
        parallel_wave: list[tuple[ToolCall, ToolRegistration]] = []

        async def flush_wave() -> None:
            if not parallel_wave:
                return
            if len(parallel_wave) == 1:
                call, registered = parallel_wave[0]
                results.append(await _execute_one(call, registered))
            else:
                wave_results = await asyncio.gather(
                    *(
                        _execute_one(call, registered)
                        for call, registered in parallel_wave
                    )
                )
                results.extend(wave_results)
            parallel_wave.clear()

        for invocation, registered in resolved:
            if registered.parallel_safe:
                parallel_wave.append((invocation, registered))
            else:
                await flush_wave()
                results.append(await _execute_one(invocation, registered))
        await flush_wave()

        return await batch.submit(results)


async def _execute_one(
    invocation: ToolCall, registered: ToolRegistration
) -> ToolResult:
    try:
        return await registered.execute(invocation)
    except ToolDomainError as error:
        return ToolResult.from_domain_error(invocation.id, error)


def _resolve_input_contract(
    handler: Callable[..., object],
    *,
    handler_name: str,
    explicit_input_model: type[BaseModel] | None,
) -> tuple[type[BaseModel], _InvokeMode]:
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
    if explicit_input_model is not None:
        validate_model(explicit_input_model, "tool input model")
        return explicit_input_model, "model"

    hints = _type_hints(handler)
    if len(parameters) == 1:
        only = parameters[0]
        annotation = hints.get(only.name)
        if annotation is None:
            raise TypeError(f"Tool input parameter {only.name!r} must be annotated.")
        if _is_model_type(annotation):
            validate_model(annotation, f"{handler_name}.{only.name}")
            return annotation, "model"

    synthesized = synthesize_input_model(
        f"{handler_name}_Input",
        parameters,
        hints,
        label=handler_name,
    )
    return synthesized, "kwargs"


def _resolve_output_model(
    handler: Callable[..., object],
    *,
    explicit_output_model: type[BaseModel] | None,
) -> type[BaseModel] | None:
    if explicit_output_model is not None:
        validate_output_model(explicit_output_model, "tool output model")
        return explicit_output_model
    annotation = _type_hints(handler).get("return")
    if _is_model_type(annotation):
        validate_output_model(annotation, f"{_callable_name(handler)}.return")
        return annotation
    return None


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
        hints = get_type_hints(_annotation_target(handler), include_extras=True)
    except (NameError, TypeError) as error:
        raise TypeError(f"Could not resolve annotations for {handler_name}.") from error
    return {key: value for key, value in hints.items()}


def _is_model_type(annotation: object) -> TypeIs[type[ModelT]]:
    return isinstance(annotation, type) and issubclass(annotation, BaseModel)


def _annotation_target(handler: Callable[..., object]) -> Callable[..., object]:
    if isinstance(handler, FunctionType):
        return handler
    if isinstance(handler, functools.partial):
        return _annotation_target(handler.func)
    return type(handler).__call__
