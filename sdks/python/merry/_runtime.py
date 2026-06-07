from __future__ import annotations

import asyncio
import inspect
import json
import os
import queue
import threading
from collections.abc import AsyncIterator, Awaitable, Callable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, get_args, get_type_hints

from pydantic import BaseModel

from . import _merry
from ._errors import MerryConfigError, MerryErrorInfo, NativeMerryError, _decode_native_error

_WORKER_POLL_INTERVAL_SECONDS = 0.01
_MERRY_TOOL_OPTIONS_ATTR = "__merry_tool_options__"


@dataclass(frozen=True)
class RunResult:
    status: str
    model_turns_run: int
    final_output: str | BaseModel | None
    final_output_json: str | None
    events: list[dict[str, Any]]


class RuntimeStream:
    def __init__(
        self,
        runtime: "Runtime",
        task: str,
        final_output_model: type[BaseModel] | None = None,
        max_model_turns: int | None = None,
    ) -> None:
        self._runtime = runtime
        self._task = task
        self._final_output_model = final_output_model
        self._final_output_schema_json = _final_output_schema_json(final_output_model)
        self._max_model_turns = _validate_max_model_turns(max_model_turns)
        self._events: list[dict[str, Any]] = []
        self._result: RunResult | None = None
        self._started = False
        self._finished = False

    def __aiter__(self) -> AsyncIterator[dict[str, Any]]:
        return self.events()

    async def events(self) -> AsyncIterator[dict[str, Any]]:
        if self._finished:
            return
        if self._started:
            raise RuntimeError("RuntimeStream events can only be consumed once.")
        self._started = True

        native_stream = await _run_in_worker(
            self._runtime._native.run_stream_blocking,
            self._task,
            self._final_output_schema_json,
            self._max_model_turns,
        )

        while True:
            event = await _run_in_worker(native_stream.next_blocking)
            if event is None:
                await self._finish_from_native_result(native_stream)
                return
            if not isinstance(event, dict):
                raise TypeError("native stream events must be dicts")
            self._events.append(event)
            yield event

            pending = _bridge_tool_call([event])
            if pending is not None:
                submitted = await self._resolve_bridge_tool_call(native_stream, pending)
                if not submitted:
                    continue

    async def result(self) -> RunResult:
        if not self._finished:
            if self._started:
                raise RuntimeError("RuntimeStream result is available after events finish.")
            async for _event in self.events():
                pass

        if self._result is None:
            raise RuntimeError("RuntimeStream finished without a result.")
        return self._result

    async def _finish_from_native_result(self, native_stream: Any) -> None:
        raw = await _run_in_worker(native_stream.result_blocking)
        result = _run_result_from_native(
            raw,
            final_output_model=self._final_output_model,
        )
        self._result = RunResult(
            status=result.status,
            model_turns_run=result.model_turns_run,
            final_output=result.final_output,
            final_output_json=result.final_output_json,
            events=list(self._events),
        )
        self._finished = True

    async def _resolve_bridge_tool_call(
        self,
        native_stream: Any,
        pending: dict[str, Any],
    ) -> bool:
        tool = self._runtime._tools.get(pending["name"])
        if tool is None:
            return True

        output = await _call_tool(tool, pending["arguments"])
        content_json = json.dumps(output, sort_keys=True)
        artifact_id = f"python-tool-result-{len(self._events) + 1}"
        try:
            await _run_in_worker(
                native_stream.submit_tool_success_json_blocking,
                pending["id"],
                artifact_id,
                content_json,
            )
        except NativeMerryError as error:
            decoded = _decode_native_error(error)
            if decoded.code == "runtime.stream_closed":
                return False
            raise decoded from error
        return True


@dataclass(frozen=True)
class ProviderRetryConfig:
    enabled: bool = True
    max_attempts: int = 6
    initial_delay_ms: int = 1000
    max_delay_ms: int = 120000
    max_elapsed_ms: int = 300000
    jitter: bool = True


@dataclass(frozen=True)
class OpenAICompatibleProvider:
    api_key: str
    model: str
    base_url: str | None = None
    retry: ProviderRetryConfig | None = None


@dataclass(frozen=True)
class WorkspaceConfig:
    root: str | os.PathLike[str]
    enable_read: bool = True
    enable_patch: bool = False


@dataclass(frozen=True)
class RuntimeConfig:
    provider: OpenAICompatibleProvider
    workspace: WorkspaceConfig | None = None
    tools: list["Tool" | Callable[..., object] | Callable[..., Awaitable[object]]] | None = None
    session_id: str = "python-sdk"


@dataclass(frozen=True)
class _ToolOptions:
    name: str | None = None
    description: str | None = None
    input_model: type[BaseModel] | None = None
    output_model: type[BaseModel] | None = None
    schema: Mapping[str, object] | None = None


@dataclass(frozen=True)
class Tool:
    name: str
    description: str
    schema: Mapping[str, object]
    handler: Callable[..., object] | Callable[..., Awaitable[object]]
    input_model: type[BaseModel] | None = None
    output_model: type[BaseModel] | None = None

    @classmethod
    def bridge(
        cls,
        handler: Callable[..., object] | Callable[..., Awaitable[object]],
        *,
        name: str | None = None,
        description: str,
        input_model: type[BaseModel] | None = None,
        output_model: type[BaseModel] | None = None,
        schema: Mapping[str, object] | None = None,
    ) -> "Tool":
        if input_model is not None:
            if schema is not None:
                raise ValueError("Use input_model instead of raw schema for Pydantic tools.")
            if output_model is None:
                raise ValueError("Pydantic bridge tools require output_model.")
            _validate_pydantic_model(input_model, "input_model")
            _validate_pydantic_model(output_model, "output_model")
            tool_schema = input_model.model_json_schema()
        else:
            if schema is None:
                raise ValueError("Tool.bridge requires input_model or schema.")
            tool_schema = schema

        return cls(
            name=name or handler.__name__,
            description=description,
            schema=tool_schema,
            handler=handler,
            input_model=input_model,
            output_model=output_model,
        )

    @classmethod
    def from_function(
        cls,
        handler: Callable[..., object] | Callable[..., Awaitable[object]],
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[BaseModel] | None = None,
        output_model: type[BaseModel] | None = None,
        schema: Mapping[str, object] | None = None,
    ) -> "Tool":
        options = _tool_options(handler)
        resolved_name = name or options.name or handler.__name__
        resolved_description = description or options.description or _function_description(handler)
        resolved_schema = schema or options.schema

        if resolved_schema is None:
            resolved_input_model = input_model or options.input_model or _function_input_model(handler)
            resolved_output_model = output_model or options.output_model or _function_output_model(
                handler
            )
        else:
            resolved_input_model = input_model or options.input_model
            resolved_output_model = output_model or options.output_model

        return cls.bridge(
            handler,
            name=resolved_name,
            description=resolved_description,
            input_model=resolved_input_model,
            output_model=resolved_output_model,
            schema=resolved_schema,
        )


class Runtime:
    def __init__(
        self,
        session_id: str = "python-sdk",
        *,
        config: RuntimeConfig | None = None,
    ) -> None:
        self._tools: dict[str, Tool] = {}
        if config is not None:
            self._init_from_config(config)
            return

        try:
            self._native = _merry.Runtime(session_id)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    def _init_from_config(self, config: RuntimeConfig) -> None:
        if config.workspace is not None:
            raise MerryConfigError(
                MerryErrorInfo(
                    code="config.workspace_unsupported",
                    domain="config",
                    message="RuntimeConfig.workspace is not wired to native workspace tools yet.",
                    hint=(
                        "Register Python bridge tools explicitly for now, or use the Rust CLI "
                        "workspace profile until Python workspace support is implemented."
                    ),
                    retryability="user_action_required",
                )
            )

        runtime = self.with_openai_compatible(
            api_key=config.provider.api_key,
            model=config.provider.model,
            base_url=config.provider.base_url,
            retry=config.provider.retry,
        )
        self._native = runtime._native
        self._tools = {}
        for tool in config.tools or []:
            self.register_tool(tool)

    @classmethod
    def with_openai_compatible(
        cls,
        *,
        api_key: str,
        model: str,
        base_url: str | None = None,
        retry: ProviderRetryConfig | None = None,
    ) -> Runtime:
        instance = cls.__new__(cls)
        instance._tools = {}
        try:
            instance._native = _merry.Runtime.with_openai_compatible(
                api_key,
                model,
                base_url,
                None if retry is None else _provider_retry_dict(retry),
            )
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        return instance

    @classmethod
    def from_env(cls) -> Runtime:
        api_key = os.environ.get("MERRY_OPENAI_API_KEY") or os.environ.get("OPENAI_API_KEY")
        model = os.environ.get("MERRY_OPENAI_MODEL") or os.environ.get("OPENAI_MODEL")
        base_url = os.environ.get("MERRY_OPENAI_BASE_URL")

        if not api_key:
            raise _missing_env_error(
                "config.openai_api_key_missing",
                "Set MERRY_OPENAI_API_KEY or OPENAI_API_KEY.",
            )
        if not model:
            raise _missing_env_error(
                "config.openai_model_missing",
                "Set MERRY_OPENAI_MODEL or OPENAI_MODEL.",
            )

        return cls.with_openai_compatible(
            api_key=api_key,
            model=model,
            base_url=base_url,
        )

    def run_blocking(
        self,
        task: str,
        *,
        final_output_model: type[BaseModel] | None = None,
        max_model_turns: int | None = None,
    ) -> RunResult:
        return asyncio.run(
            self.run(task, final_output_model=final_output_model, max_model_turns=max_model_turns)
        )

    def _run_native_blocking(
        self,
        task: str,
        final_output_model: type[BaseModel] | None,
        max_model_turns: int | None,
    ) -> RunResult:
        try:
            raw = self._native.run_blocking(
                task,
                _final_output_schema_json(final_output_model),
                _validate_max_model_turns(max_model_turns),
            )
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

        return _run_result_from_native(raw, final_output_model=final_output_model)

    async def run(
        self,
        task: str,
        *,
        final_output_model: type[BaseModel] | None = None,
        max_model_turns: int | None = None,
    ) -> RunResult:
        if not getattr(self, "_tools", {}):
            return await _run_in_worker(
                self._run_native_blocking,
                task,
                final_output_model,
                max_model_turns,
            )

        stream = self.stream(
            task,
            final_output_model=final_output_model,
            max_model_turns=max_model_turns,
        )
        async for _event in stream:
            pass
        return await stream.result()

    async def run_stream(
        self,
        task: str,
        *,
        final_output_model: type[BaseModel] | None = None,
        max_model_turns: int | None = None,
    ) -> AsyncIterator[dict[str, Any]]:
        async for event in self.stream(
            task,
            final_output_model=final_output_model,
            max_model_turns=max_model_turns,
        ):
            yield event

    def stream(
        self,
        task: str,
        *,
        final_output_model: type[BaseModel] | None = None,
        max_model_turns: int | None = None,
    ) -> RuntimeStream:
        return RuntimeStream(
            self,
            task,
            final_output_model=final_output_model,
            max_model_turns=max_model_turns,
        )

    def register_tool(
        self,
        tool: Tool | Callable[..., object] | Callable[..., Awaitable[object]],
    ) -> None:
        registered_tool = tool if isinstance(tool, Tool) else Tool.from_function(tool)
        try:
            self._native.register_bridge_tool(
                registered_tool.name,
                registered_tool.description,
                json.dumps(registered_tool.schema, sort_keys=True),
            )
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        self._tools[registered_tool.name] = registered_tool

    def tool(
        self,
        handler: Callable[..., object] | Callable[..., Awaitable[object]] | None = None,
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[BaseModel] | None = None,
        output_model: type[BaseModel] | None = None,
        schema: Mapping[str, object] | None = None,
    ) -> Callable[..., object] | Callable[..., Awaitable[object]]:
        def decorate(
            func: Callable[..., object] | Callable[..., Awaitable[object]],
        ) -> Callable[..., object] | Callable[..., Awaitable[object]]:
            decorated = _decorate_tool(
                func,
                name=name,
                description=description,
                input_model=input_model,
                output_model=output_model,
                schema=schema,
            )
            self.register_tool(decorated)
            return decorated

        if handler is None:
            return decorate
        return decorate(handler)

    def _submit_tool_success_json_blocking(
        self,
        call_id: str,
        artifact_id: str,
        content_json: str,
    ) -> list[dict[str, Any]]:
        try:
            events = self._native.submit_tool_success_json_blocking(
                call_id,
                artifact_id,
                content_json,
            )
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        if not isinstance(events, list):
            raise TypeError("native submit result events must be a list")
        if not all(isinstance(event, dict) for event in events):
            raise TypeError("native submit result events must contain dict items")
        return list(events)


def _run_result_from_native(
    raw: dict[str, Any],
    *,
    final_output_model: type[BaseModel] | None = None,
) -> RunResult:
    status = raw["status"]
    if not isinstance(status, str):
        raise TypeError("native run result status must be a str")
    model_turns_run = raw["model_turns_run"]
    if not isinstance(model_turns_run, int) or isinstance(model_turns_run, bool):
        raise TypeError("native run result model_turns_run must be an int")
    final_output = raw["final_output"]
    if final_output is not None and not isinstance(final_output, str):
        raise TypeError("native run result final_output must be a str or None")
    final_output_json = raw.get("final_output_json")
    if final_output_json is not None and not isinstance(final_output_json, str):
        raise TypeError("native run result final_output_json must be a str or None")
    events = raw["events"]
    if not isinstance(events, list):
        raise TypeError("native run result events must be a list")
    if not all(isinstance(event, dict) for event in events):
        raise TypeError("native run result events must contain dict items")

    structured_final_output = _validate_final_output_json(
        final_output_json,
        final_output_model,
    )

    return RunResult(
        status=status,
        model_turns_run=model_turns_run,
        final_output=structured_final_output if final_output_model is not None else final_output,
        final_output_json=final_output_json,
        events=list(events),
    )


def _provider_retry_dict(retry: ProviderRetryConfig) -> dict[str, object]:
    return {
        "enabled": retry.enabled,
        "max_attempts": retry.max_attempts,
        "initial_delay_ms": retry.initial_delay_ms,
        "max_delay_ms": retry.max_delay_ms,
        "max_elapsed_ms": retry.max_elapsed_ms,
        "jitter": retry.jitter,
    }


def _final_output_schema_json(model: type[BaseModel] | None) -> str | None:
    if model is None:
        return None
    _validate_pydantic_model(model, "final_output_model")
    return json.dumps(model.model_json_schema(), sort_keys=True)


def _validate_final_output_json(
    final_output_json: str | None,
    final_output_model: type[BaseModel] | None,
) -> BaseModel | None:
    if final_output_model is None:
        return None
    if final_output_json is None:
        return None
    return final_output_model.model_validate_json(final_output_json)


def _validate_max_model_turns(max_model_turns: int | None) -> int | None:
    if max_model_turns is None:
        return None
    if isinstance(max_model_turns, bool) or not isinstance(max_model_turns, int):
        raise TypeError("max_model_turns must be an int greater than zero.")
    if max_model_turns < 1:
        raise ValueError("max_model_turns must be greater than zero.")
    return max_model_turns


def _bridge_tool_call(events: list[dict[str, Any]]) -> dict[str, Any] | None:
    pending = [
        event["kind"]["call"]
        for event in events
        if event.get("kind", {}).get("type") == "bridge_tool_call_requested"
    ]
    if not pending:
        return None
    call = pending[-1]
    if not isinstance(call, dict):
        raise TypeError("bridge_tool_call_requested call must be a dict")
    arguments = call.get("arguments")
    if not isinstance(arguments, dict):
        raise TypeError("bridge_tool_call_requested arguments must be a dict")
    name = call.get("name")
    call_id = call.get("id")
    if not isinstance(name, str) or not isinstance(call_id, str):
        raise TypeError("bridge_tool_call_requested id and name must be str")
    return {"id": call_id, "name": name, "arguments": arguments}


def _decorate_tool(
    handler: Callable[..., object] | Callable[..., Awaitable[object]],
    *,
    name: str | None = None,
    description: str | None = None,
    input_model: type[BaseModel] | None = None,
    output_model: type[BaseModel] | None = None,
    schema: Mapping[str, object] | None = None,
) -> Callable[..., object] | Callable[..., Awaitable[object]]:
    setattr(
        handler,
        _MERRY_TOOL_OPTIONS_ATTR,
        _ToolOptions(
            name=name,
            description=description,
            input_model=input_model,
            output_model=output_model,
            schema=schema,
        ),
    )
    return handler


def _tool_options(handler: Callable[..., object] | Callable[..., Awaitable[object]]) -> _ToolOptions:
    options = getattr(handler, _MERRY_TOOL_OPTIONS_ATTR, None)
    if isinstance(options, _ToolOptions):
        return options
    return _ToolOptions()


def _function_description(handler: Callable[..., object] | Callable[..., Awaitable[object]]) -> str:
    doc = inspect.getdoc(handler)
    if doc is None:
        raise ValueError(
            f"Tool function {handler.__name__} must define a docstring description "
            "or pass description=..."
        )
    first_paragraph = doc.split("\n\n", 1)[0].strip()
    if not first_paragraph:
        raise ValueError(
            f"Tool function {handler.__name__} must define a docstring description "
            "or pass description=..."
        )
    return " ".join(first_paragraph.split())


def _function_input_model(
    handler: Callable[..., object] | Callable[..., Awaitable[object]],
) -> type[BaseModel]:
    signature = inspect.signature(handler)
    parameters = [
        parameter
        for parameter in signature.parameters.values()
        if parameter.kind
        in (
            inspect.Parameter.POSITIONAL_ONLY,
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.KEYWORD_ONLY,
        )
    ]
    if len(parameters) != 1:
        raise ValueError(
            f"Tool function {handler.__name__} must declare exactly one Pydantic "
            "BaseModel parameter or pass input_model=..."
        )
    parameter = parameters[0]
    annotation = _type_hints(handler).get(parameter.name, parameter.annotation)
    if annotation is inspect.Signature.empty:
        raise ValueError(
            f"Tool function {handler.__name__} parameter {parameter.name} must be annotated "
            "with a Pydantic BaseModel subclass."
        )
    return _ensure_pydantic_model(annotation, f"{handler.__name__}.{parameter.name}")


def _function_output_model(
    handler: Callable[..., object] | Callable[..., Awaitable[object]],
) -> type[BaseModel]:
    signature = inspect.signature(handler)
    annotation = _type_hints(handler).get("return", signature.return_annotation)
    if annotation is inspect.Signature.empty:
        raise ValueError(
            f"Tool function {handler.__name__} must be annotated with a Pydantic "
            "BaseModel return type or pass output_model=..."
        )
    return _ensure_pydantic_model(annotation, f"{handler.__name__}.return")


def _type_hints(handler: Callable[..., object] | Callable[..., Awaitable[object]]) -> dict[str, Any]:
    try:
        return get_type_hints(handler)
    except (NameError, TypeError) as error:
        raise TypeError(f"Could not resolve annotations for tool function {handler.__name__}.") from error


def _ensure_pydantic_model(annotation: object, label: str) -> type[BaseModel]:
    if inspect.isclass(annotation) and issubclass(annotation, BaseModel):
        return annotation
    raise TypeError(f"{label} must be a pydantic BaseModel subclass.")


async def _call_tool(tool: Tool, arguments: Mapping[str, object]) -> object:
    if tool.input_model is None:
        output = tool.handler(**arguments)
    else:
        input_value = tool.input_model.model_validate(dict(arguments))
        output = tool.handler(input_value)
    if inspect.isawaitable(output):
        output = await output
    if tool.output_model is not None:
        if isinstance(output, tool.output_model):
            output_value = output
        else:
            output_value = tool.output_model.model_validate(output)
        return output_value.model_dump(mode="json")
    return output


def _validate_pydantic_model(model: type[BaseModel], role: str) -> None:
    if not inspect.isclass(model) or not issubclass(model, BaseModel):
        raise TypeError(f"{role} must be a pydantic BaseModel subclass.")

    missing = _missing_field_descriptions(model, set())
    if missing:
        fields = ", ".join(missing)
        raise ValueError(f"{role} fields must define Field(description=...): {fields}")


def _missing_field_descriptions(
    model: type[BaseModel],
    seen: set[type[BaseModel]],
) -> list[str]:
    if model in seen:
        return []

    seen.add(model)
    missing: list[str] = []
    for name, field in model.model_fields.items():
        description = field.description
        if not isinstance(description, str) or not description.strip():
            missing.append(f"{model.__name__}.{name}")
        for nested_model in _iter_pydantic_model_types(field.annotation):
            missing.extend(_missing_field_descriptions(nested_model, seen))
    return missing


def _iter_pydantic_model_types(annotation: object) -> list[type[BaseModel]]:
    if inspect.isclass(annotation) and issubclass(annotation, BaseModel):
        return [annotation]

    nested: list[type[BaseModel]] = []
    for arg in get_args(annotation):
        nested.extend(_iter_pydantic_model_types(arg))
    return nested


async def _run_in_worker(
    func: Callable[..., Any],
    *args: object,
) -> Any:
    result_queue: queue.Queue[tuple[bool, Any]] = queue.Queue(maxsize=1)

    def worker() -> None:
        try:
            result_queue.put((True, func(*args)))
        except BaseException as error:
            result_queue.put((False, error))

    thread = threading.Thread(target=worker, name="merry-sdk-worker", daemon=True)
    thread.start()

    while True:
        try:
            succeeded, value = result_queue.get_nowait()
            thread.join()
            if succeeded:
                return value
            raise value
        except queue.Empty:
            await asyncio.sleep(_WORKER_POLL_INTERVAL_SECONDS)


def _missing_env_error(code: str, message: str) -> MerryConfigError:
    return MerryConfigError(
        MerryErrorInfo(
            code=code,
            domain="config",
            message=message,
            hint=(
                "Use Runtime.with_openai_compatible(...) to pass values directly, "
                "or export the environment variables before running the example."
            ),
            retryability="user_action_required",
        )
    )
