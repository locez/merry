"""Async-first Python facade over the Rust-owned Merry agent."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import AsyncIterator, Callable, Sequence
from typing import TYPE_CHECKING, TypeVar, overload

from pydantic import BaseModel

from . import _merry
from ._errors import (
    MerryConfigError,
    MerryError,
    MerryErrorInfo,
    NativeMerryError,
    _decode_native_error,
)
from ._events import Event
from ._json import dump_json, require_json_object
from ._models import RunResult, ToolCallBatch
from ._run import AgentRun
from ._schema import validate_model
from ._tools import InputT, OutputT, Tool, ToolHandler, ToolRegistration, ToolRegistry

if TYPE_CHECKING:
    from ._builder import AgentBuilder

_LOGGER = logging.getLogger(__name__)
_NativeAgent = _merry.Agent
_NativeBuilder = _merry.AgentBuilder
FinalOutputT = TypeVar("FinalOutputT", bound=BaseModel)


class Agent:
    """A built Rust-owned session with a Python host tool registry."""

    def __init__(self) -> None:
        raise TypeError("Agent instances are created by AgentBuilder")

    @classmethod
    def _from_native(
        cls,
        native_agent: _NativeAgent,
        tools: Sequence[ToolRegistration] = (),
    ) -> Agent:
        """Create an Agent from the native binding; reserved for adapters."""

        instance = cls.__new__(cls)
        instance._initialize(
            native_agent=native_agent, native_builder=None, tools=tools
        )
        instance._session_id = native_agent.session_id()
        return instance

    @classmethod
    def _from_builder(
        cls,
        native_builder: _NativeBuilder,
        tools: Sequence[ToolRegistration] = (),
        *,
        session_id: str,
    ) -> Agent:
        """Create a lazy Agent from the native binding; reserved for builders."""

        instance = cls.__new__(cls)
        instance._initialize(
            native_agent=None, native_builder=native_builder, tools=tools
        )
        instance._session_id = session_id
        return instance

    def _initialize(
        self,
        *,
        native_agent: _NativeAgent | None,
        native_builder: _NativeBuilder | None,
        tools: Sequence[ToolRegistration],
    ) -> None:
        if (native_agent is None) == (native_builder is None):
            raise ValueError("Agent needs exactly one native construction state")
        self._native_agent = native_agent
        self._native_builder = native_builder
        self._session_id: str | None = None
        self._tools = tuple(tools)
        self._registry = ToolRegistry(self._tools)

    @classmethod
    def builder(cls, session_id: str | None = None) -> AgentBuilder:
        """Create a fresh typed AgentBuilder."""

        from ._builder import AgentBuilder

        return AgentBuilder(session_id)

    @property
    def session_id(self) -> str:
        """Return the Rust-owned session identity."""

        if self._session_id is not None:
            return self._session_id
        native_agent = self._native_agent
        if native_agent is None:
            raise RuntimeError("Agent has no native construction state")
        self._session_id = native_agent.session_id()
        return self._session_id

    def register_tool(self, tool: Tool[InputT, OutputT]) -> Tool[InputT, OutputT]:
        """Register a host tool before the first run starts."""

        self._register_tool(tool)
        return tool

    @overload
    def tool(
        self,
        handler: ToolHandler[InputT, OutputT],
        /,
    ) -> Tool[InputT, OutputT]: ...

    @overload
    def tool(
        self,
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[InputT] | None = None,
        output_model: type[OutputT] | None = None,
    ) -> Callable[[ToolHandler[InputT, OutputT]], Tool[InputT, OutputT]]: ...

    def tool(
        self,
        handler: ToolHandler[InputT, OutputT] | None = None,
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[InputT] | None = None,
        output_model: type[OutputT] | None = None,
    ) -> (
        Tool[InputT, OutputT]
        | Callable[[ToolHandler[InputT, OutputT]], Tool[InputT, OutputT]]
    ):
        if self._native_builder is None:
            raise MerryConfigError(
                MerryErrorInfo(
                    code="agent.tool_registry_frozen",
                    domain="config",
                    message="Tools must be registered before the Agent starts a run.",
                    hint="Use @agent.tool before the first stream or @builder.tool before build.",
                    retryability="user_action_required",
                )
            )

        def decorate(
            function: ToolHandler[InputT, OutputT],
        ) -> Tool[InputT, OutputT]:
            tool = Tool.from_function(
                function,
                name=name,
                description=description,
                input_model=input_model,
                output_model=output_model,
            )
            self._register_tool(tool)
            return tool

        if handler is None:
            return decorate
        return decorate(handler)

    def stream(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
    ) -> AgentRun[FinalOutputT]:
        """Start a message-first run with optional typed final output."""

        schema_json: str | None = None
        if final_output_model is not None:
            validate_model(final_output_model, "final output model")
            schema = require_json_object(
                final_output_model.model_json_schema(),
                "final output schema",
            )
            schema_json = dump_json(schema, "final output schema")
        native = self._ensure_native()
        try:
            native_run = native.stream(task, schema_json)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        return AgentRun(native_run, final_output_model)

    async def run(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
    ) -> RunResult[FinalOutputT]:
        """Run a task, executing each host tool batch in Python."""

        run = self.stream(task, final_output_model=final_output_model)
        try:
            async for message in run:
                if isinstance(message, ToolCallBatch):
                    await self._registry.execute(message)
            return await run.result()
        except asyncio.CancelledError:
            await _cancel_run_preserving(run, shield=True)
            raise
        except Exception:
            if not run.finished:
                await _cancel_run_preserving(run, shield=False)
            raise

    async def messages(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
    ) -> AsyncIterator[Event | ToolCallBatch[FinalOutputT]]:
        """Yield run messages while retaining cancellation cleanup ownership."""

        run = self.stream(task, final_output_model=final_output_model)
        cleanup_attempted = False
        try:
            async for message in run:
                yield message
        except BaseException as error:
            cleanup_attempted = True
            if not run.finished:
                await _cancel_run_preserving(
                    run,
                    shield=isinstance(error, asyncio.CancelledError),
                )
            raise
        finally:
            if not cleanup_attempted:
                await run.close()

    async def save_session(self) -> None:
        """Persist the current Rust-owned session to its configured store."""

        try:
            await self._ensure_native().save_session()
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    def _ensure_native(self) -> _NativeAgent:
        if self._native_agent is None:
            if self._native_builder is None:
                raise RuntimeError("Agent has no native construction state")
            native_builder = self._native_builder
            self._native_builder = None
            try:
                self._native_agent = native_builder.build()
            except NativeMerryError as error:
                raise _decode_native_error(error) from error
        return self._native_agent

    def _register_tool(self, tool: ToolRegistration) -> None:
        native_builder = self._native_builder
        if native_builder is None:
            raise MerryConfigError(
                MerryErrorInfo(
                    code="agent.tool_registry_frozen",
                    domain="config",
                    message="Tools must be registered before the Agent starts a run.",
                    hint="Register tools before calling stream or run.",
                    retryability="user_action_required",
                )
            )
        if any(existing.name == tool.name for existing in self._tools):
            raise MerryConfigError(
                MerryErrorInfo(
                    code="tool.duplicate_registration",
                    domain="tool",
                    message=f"Tool {tool.name!r} is already registered.",
                    hint="Use one unique provider-visible name per Agent.",
                    retryability="user_action_required",
                )
            )
        self._native_builder = None
        try:
            native_builder.register_bridge_tool(
                tool.name,
                tool.description,
                dump_json(tool.schema, "tool input schema"),
            )
        except NativeMerryError as error:
            if not native_builder.is_consumed():
                self._native_builder = native_builder
            raise _decode_native_error(error) from error
        self._native_builder = native_builder
        self._tools = (*self._tools, tool)
        self._registry = ToolRegistry(self._tools)


async def _cancel_run_preserving(
    run: AgentRun[FinalOutputT],
    *,
    shield: bool,
) -> None:
    try:
        cancellation = run.cancel()
        if shield:
            await asyncio.shield(cancellation)
        else:
            await cancellation
    except (
        asyncio.CancelledError,
        MerryError,
        OSError,
        RuntimeError,
        TypeError,
    ) as cleanup_error:
        _LOGGER.warning(
            "failed to cancel an AgentRun during cleanup; error_type=%s",
            type(cleanup_error).__name__,
        )
