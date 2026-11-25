from __future__ import annotations

import json
from collections.abc import Sequence
from pathlib import Path
from typing import Protocol, runtime_checkable

import merry
from merry import _merry
from merry._hooks import Hooks
from merry._json import JsonObject, JsonValue, dump_json
from merry._tools import ToolRegistration


@runtime_checkable
class _FakeResponseFactory(Protocol):
    def __call__(self, session_id: str, final_text: str, /) -> _merry.Agent: ...


@runtime_checkable
class _StreamedTextDeltaFactory(Protocol):
    def __call__(
        self, session_id: str, delta: str, final_text: str, /
    ) -> _merry.Agent: ...


@runtime_checkable
class _ScriptedToolFactory(Protocol):
    def __call__(
        self,
        session_id: str,
        tool_name: str,
        arguments_json: str,
        final_text: str,
        /,
    ) -> _merry.Agent: ...


@runtime_checkable
class _ParallelToolCallsFactory(Protocol):
    def __call__(
        self,
        session_id: str,
        calls_json: str,
        final_text: str,
        /,
    ) -> _merry.Agent: ...


@runtime_checkable
class _ScriptedTurnsFactory(Protocol):
    def __call__(
        self,
        session_id: str,
        turns_json: str,
        tool_names: list[str],
        session_store_path: str | None,
        /,
    ) -> _merry.Agent: ...


@runtime_checkable
class _EchoRequestFactory(Protocol):
    def __call__(
        self,
        session_id: str,
        base_instructions: str | None,
        progress_commentary: bool | None,
        progress_commentary_instructions: str | None,
        stable_blocks: list[tuple[str, str]],
        max_output_tokens: int | None,
        parallel_tool_calls: str | None,
        reasoning_effort: str | None,
        service_tier: str | None,
        /,
    ) -> _merry.Agent: ...


@runtime_checkable
class _FinalOutputFactory(Protocol):
    def __call__(
        self, session_id: str, arguments_json: str, final_text: str, /
    ) -> _merry.Agent: ...


@runtime_checkable
class _PendingResponseFactory(Protocol):
    def __call__(self, session_id: str, /) -> _merry.Agent: ...


@runtime_checkable
class _ContextAndHooksFactory(Protocol):
    def __call__(
        self,
        session_id: str,
        turns_json: str,
        tool_names: list[str],
        provider_context_window_tokens: int | None = None,
        explicit_context_window_tokens: int | None = None,
        hook_subscriptions_json: str | None = None,
        max_model_turns: int | None = None,
        /,
    ) -> _merry.Agent: ...


def _fake_response_native(session_id: str, final_text: str) -> _merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_fake_response", None)
    if not isinstance(factory, _FakeResponseFactory):
        raise TypeError("test-utils native extension is required")
    return factory(session_id, final_text)


def _scripted_tool_native(
    session_id: str,
    tool_name: str,
    arguments_json: str,
    final_text: str,
) -> _merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_scripted_tool_call", None)
    if not isinstance(factory, _ScriptedToolFactory):
        raise TypeError("test-utils native extension is required")
    return factory(session_id, tool_name, arguments_json, final_text)


def streamed_text_delta_agent(
    *,
    delta: str,
    final_text: str = "done",
    session_id: str = "python-streamed-delta",
) -> merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_streamed_text_delta", None)
    if not isinstance(factory, _StreamedTextDeltaFactory):
        raise TypeError("test-utils native extension is required")
    native = factory(session_id, delta, final_text)
    return merry.Agent._from_native(native)


def _final_output_native(
    session_id: str, arguments_json: str, final_text: str
) -> _merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_final_output", None)
    if not isinstance(factory, _FinalOutputFactory):
        raise TypeError("test-utils native extension is required")
    return factory(session_id, arguments_json, final_text)


def pending_native_agent(session_id: str) -> _merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_pending_response", None)
    if not isinstance(factory, _PendingResponseFactory):
        raise TypeError("test-utils native extension is required")
    return factory(session_id)


def fake_agent(
    final_text: str = "done", *, session_id: str = "python-fake"
) -> merry.Agent:
    native = _fake_response_native(session_id, final_text)
    return merry.Agent._from_native(native)


def scripted_native_agent(
    *,
    tool_name: str,
    arguments: JsonObject,
    final_text: str = "done",
    session_id: str = "python-scripted",
) -> _merry.Agent:
    arguments_json = json.dumps(arguments, ensure_ascii=False, separators=(",", ":"))
    return _scripted_tool_native(session_id, tool_name, arguments_json, final_text)


def scripted_agent(
    tool: ToolRegistration,
    *,
    arguments: JsonObject,
    final_text: str = "done",
    session_id: str = "python-scripted",
) -> merry.Agent:
    native = scripted_native_agent(
        tool_name=tool.name,
        arguments=arguments,
        final_text=final_text,
        session_id=session_id,
    )
    return merry.Agent._from_native(native, tools=(tool,))


def parallel_tools_agent(
    tools: Sequence[ToolRegistration],
    *,
    calls: Sequence[JsonObject],
    final_text: str = "done",
    session_id: str = "python-parallel-tools",
) -> merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_parallel_tool_calls", None)
    if not isinstance(factory, _ParallelToolCallsFactory):
        raise TypeError("test-utils native extension is required")
    calls_json = json.dumps(list(calls), ensure_ascii=False, separators=(",", ":"))
    native = factory(session_id, calls_json, final_text)
    return merry.Agent._from_native(native, tools=tuple(tools))


def scripted_turns_agent(
    turns: Sequence[JsonValue],
    *,
    tools: Sequence[ToolRegistration] = (),
    session_store: Path | None = None,
    session_id: str = "python-scripted-turns",
) -> merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_scripted_turns", None)
    if not isinstance(factory, _ScriptedTurnsFactory):
        raise TypeError("test-utils native extension is required")
    turns_json = json.dumps(list(turns), ensure_ascii=False, separators=(",", ":"))
    native = factory(
        session_id,
        turns_json,
        [item.name for item in tools],
        None if session_store is None else str(session_store),
    )
    return merry.Agent._from_native(native, tools=tuple(tools))


def echo_request_agent(
    *,
    session_id: str = "python-echo-request",
    prompt: merry.PromptProfile | None = None,
    generation: merry.GenerationConfig | None = None,
) -> merry.Agent:
    factory: object = getattr(_merry, "test_agent_echoing_request", None)
    if not isinstance(factory, _EchoRequestFactory):
        raise TypeError("test-utils native extension is required")
    native = factory(
        session_id,
        None if prompt is None else prompt.base_instructions,
        None if prompt is None else prompt.progress_commentary,
        None if prompt is None else prompt.progress_commentary_instructions,
        []
        if prompt is None
        else [(block.tag, block.text) for block in prompt.stable_blocks],
        None if generation is None else generation.max_output_tokens,
        None if generation is None else generation.parallel_tool_calls.value,
        None if generation is None else generation.reasoning_effort,
        None
        if generation is None or generation.service_tier is None
        else generation.service_tier.value,
    )
    return merry.Agent._from_native(native)


def final_output_agent(
    arguments: JsonObject,
    *,
    session_id: str = "python-final-output",
) -> merry.Agent:
    arguments_json = json.dumps(arguments, ensure_ascii=False, separators=(",", ":"))
    native = _final_output_native(session_id, arguments_json, "unused")
    return merry.Agent._from_native(native)


def context_and_hooks_agent(
    session_id: str,
    turns: Sequence[JsonValue],
    *,
    tools: Sequence[ToolRegistration] = (),
    hooks: Hooks | None = None,
    provider_context_window_tokens: int | None = None,
    explicit_context_window_tokens: int | None = None,
    max_model_turns: int | None = None,
) -> merry.Agent:
    factory: object = getattr(_merry, "test_agent_with_context_and_hooks", None)
    if not isinstance(factory, _ContextAndHooksFactory):
        raise TypeError("test-utils native extension is required")
    subs_json: str | None = None
    if hooks is not None and not hooks.is_empty():
        subs_json = dump_json(hooks.subscriptions_wire(), "hook subscriptions")
    turns_json = json.dumps(list(turns), ensure_ascii=False, separators=(",", ":"))
    native = factory(
        session_id,
        turns_json,
        [item.name for item in tools],
        provider_context_window_tokens,
        explicit_context_window_tokens,
        subs_json,
        max_model_turns,
    )
    return merry.Agent._from_native(native, tools=tuple(tools), hooks=hooks)
