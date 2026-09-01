from __future__ import annotations

import json
from typing import Protocol, TypeVar, runtime_checkable

from pydantic import BaseModel

import merry
from merry import _merry
from merry._json import JsonObject

InputT = TypeVar("InputT", bound=BaseModel)
OutputT = TypeVar("OutputT", bound=BaseModel)


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
class _FinalOutputFactory(Protocol):
    def __call__(
        self, session_id: str, arguments_json: str, final_text: str, /
    ) -> _merry.Agent: ...


@runtime_checkable
class _PendingResponseFactory(Protocol):
    def __call__(self, session_id: str, /) -> _merry.Agent: ...


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
    tool: merry.Tool[InputT, OutputT],
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


def final_output_agent(
    arguments: JsonObject,
    *,
    session_id: str = "python-final-output",
) -> merry.Agent:
    arguments_json = json.dumps(arguments, ensure_ascii=False, separators=(",", ":"))
    native = _final_output_native(session_id, arguments_json, "unused")
    return merry.Agent._from_native(native)
