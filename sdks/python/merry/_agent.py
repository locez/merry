"""Async-first Python facade over the Rust-owned Merry agent."""

from __future__ import annotations

from collections.abc import Sequence
from typing import TYPE_CHECKING, Literal, TypeVar, overload

from pydantic import BaseModel

from . import _merry
from ._config import (
    CompactionConfig,
    GenerationConfig,
    HistoryRetention,
    PathInput,
    PromptProfile,
    Provider,
    StructuredOutputRetryPolicy,
    WorkspaceConfig,
)
from ._errors import NativeMerryError, _decode_native_error
from ._hooks import Hooks
from ._interactive import InteractiveSession, ManualHookInteractiveSession
from ._json import dump_json, require_json_object
from ._models import RunResult, ToolCallBatch, ToolSubmission
from ._run import AgentRun, ManualHookAgentRun
from ._schema import validate_model
from ._tools import ToolRegistration, ToolRegistry

if TYPE_CHECKING:
    from ._builder import AgentBuilder

_NativeAgent = _merry.Agent
FinalOutputT = TypeVar("FinalOutputT", bound=BaseModel)


class Agent:
    """An eagerly built Rust-owned session with a typed Python host tool registry."""

    def __init__(
        self,
        *,
        provider: Provider,
        session_id: str | None = None,
        system_prompt: str | PromptProfile | None = None,
        generation: GenerationConfig | None = None,
        context_window_tokens: int | None = None,
        compaction: CompactionConfig | None = None,
        workspace: WorkspaceConfig | None = None,
        session_store: PathInput | None = None,
        history_retention: HistoryRetention | None = None,
        tools: Sequence[ToolRegistration] = (),
        hooks: Hooks | None = None,
        max_model_turns: int | None = None,
        structured_output_retry_policy: StructuredOutputRetryPolicy | None = None,
        event_buffer_size: int | None = None,
    ) -> None:
        builder = _configure_builder(
            session_id=session_id,
            provider=provider,
            system_prompt=system_prompt,
            generation=generation,
            context_window_tokens=context_window_tokens,
            compaction=compaction,
            workspace=workspace,
            session_store=session_store,
            history_retention=history_retention,
            tools=tools,
            hooks=hooks,
            max_model_turns=max_model_turns,
            structured_output_retry_policy=structured_output_retry_policy,
            event_buffer_size=event_buffer_size,
        )
        built = builder.build()
        self._native_agent: _NativeAgent = built._native_agent
        self._session_id: str = built._session_id
        self._registry: ToolRegistry = built._registry
        self._hooks: Hooks | None = built._hooks

    @classmethod
    async def resume(
        cls,
        session_id: str,
        *,
        provider: Provider,
        session_store: PathInput,
        history_retention: HistoryRetention | None = None,
        system_prompt: str | PromptProfile | None = None,
        generation: GenerationConfig | None = None,
        context_window_tokens: int | None = None,
        compaction: CompactionConfig | None = None,
        workspace: WorkspaceConfig | None = None,
        tools: Sequence[ToolRegistration] = (),
        hooks: Hooks | None = None,
        max_model_turns: int | None = None,
        structured_output_retry_policy: StructuredOutputRetryPolicy | None = None,
        event_buffer_size: int | None = None,
    ) -> Agent:
        """Resume a persisted Rust-owned session from its file-backed store."""

        builder = _configure_builder(
            session_id=session_id,
            provider=provider,
            system_prompt=system_prompt,
            generation=generation,
            context_window_tokens=context_window_tokens,
            compaction=compaction,
            workspace=workspace,
            session_store=session_store,
            history_retention=history_retention,
            tools=tools,
            hooks=hooks,
            max_model_turns=max_model_turns,
            structured_output_retry_policy=structured_output_retry_policy,
            event_buffer_size=event_buffer_size,
        )
        return await builder.resume()

    @classmethod
    def builder(cls, session_id: str | None = None) -> AgentBuilder:
        """Create a fresh typed AgentBuilder for fluent step-by-step construction."""

        from ._builder import AgentBuilder

        return AgentBuilder(session_id)

    @classmethod
    def _from_native(
        cls,
        native_agent: _NativeAgent,
        tools: Sequence[ToolRegistration] = (),
        hooks: Hooks | None = None,
    ) -> Agent:
        """Create an Agent from an already built native handle; reserved for adapters."""

        instance = cls.__new__(cls)
        instance._native_agent = native_agent
        instance._session_id = native_agent.session_id()
        instance._registry = ToolRegistry(tuple(tools))
        instance._hooks = None if hooks is None or hooks.is_empty() else hooks.copy()
        return instance

    @property
    def session_id(self) -> str:
        """Return the Rust-owned session identity."""

        return self._session_id

    @property
    def tools(self) -> ToolRegistry:
        """Return the host tool registry bound to this agent."""

        return self._registry

    async def execute_tool_batch(self, batch: ToolCallBatch) -> ToolSubmission:
        """Execute a pending host tool batch using this agent's registered tools."""

        return await self._registry.execute(batch)

    @overload
    def stream(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
        auto_hooks: Literal[True] = True,
        auto_tools: bool = False,
    ) -> AgentRun[FinalOutputT]: ...

    @overload
    def stream(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
        auto_hooks: Literal[False],
        auto_tools: bool = False,
    ) -> ManualHookAgentRun[FinalOutputT]: ...

    @overload
    def stream(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
        auto_hooks: bool,
        auto_tools: bool = False,
    ) -> AgentRun[FinalOutputT] | ManualHookAgentRun[FinalOutputT]: ...

    def stream(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
        auto_hooks: bool = True,
        auto_tools: bool = False,
    ) -> AgentRun[FinalOutputT] | ManualHookAgentRun[FinalOutputT]:
        """Start a message-first run with optional typed final output.

        When `auto_hooks=True` (the default), registered Python hooks run
        automatically on the caller's event loop before yielding the next
        message. Set `auto_hooks=False` to inspect and resolve `HookInvocation`
        messages manually. Set `auto_tools=True` to execute registered host
        tools automatically during streaming.
        """

        schema_json: str | None = None
        if final_output_model is not None:
            validate_model(final_output_model, "final output model")
            schema = require_json_object(
                final_output_model.model_json_schema(),
                "final output schema",
            )
            schema_json = dump_json(schema, "final output schema")
        try:
            native_run = self._native_agent.stream(task, schema_json)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        if auto_hooks:
            return AgentRun(
                native_run,
                final_output_model,
                hooks=self._hooks,
                tools=self._registry,
                auto_tools=auto_tools,
            )
        return ManualHookAgentRun(
            native_run,
            final_output_model,
            hooks=self._hooks,
            tools=self._registry,
            auto_tools=auto_tools,
        )

    async def run(
        self,
        task: str,
        *,
        final_output_model: type[FinalOutputT] | None = None,
    ) -> RunResult[FinalOutputT]:
        """Run a task, executing registered host tools and lifecycle hooks in Python."""

        async with self.stream(
            task,
            final_output_model=final_output_model,
            auto_hooks=True,
            auto_tools=True,
        ) as run:
            async for message in run:
                if isinstance(message, ToolCallBatch):
                    await self._registry.execute(message)
            return await run.result()

    @overload
    def interactive(
        self,
        *,
        auto_hooks: Literal[True] = True,
        auto_tools: bool = False,
    ) -> InteractiveSession: ...

    @overload
    def interactive(
        self,
        *,
        auto_hooks: Literal[False],
        auto_tools: bool = False,
    ) -> ManualHookInteractiveSession: ...

    @overload
    def interactive(
        self,
        *,
        auto_hooks: bool,
        auto_tools: bool = False,
    ) -> InteractiveSession | ManualHookInteractiveSession: ...

    def interactive(
        self,
        *,
        auto_hooks: bool = True,
        auto_tools: bool = False,
    ) -> InteractiveSession | ManualHookInteractiveSession:
        """Open a long-lived interactive session with input queueing and interrupts."""

        try:
            native_interactive = self._native_agent.start_interactive()
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        if auto_hooks:
            return InteractiveSession(
                native_interactive,
                hooks=self._hooks,
                tools=self._registry,
                auto_tools=auto_tools,
            )
        return ManualHookInteractiveSession(
            native_interactive,
            hooks=self._hooks,
            tools=self._registry,
            auto_tools=auto_tools,
        )

    async def save_session(self) -> None:
        """Persist the current Rust-owned session to its configured store."""

        try:
            await self._native_agent.save_session()
        except NativeMerryError as error:
            raise _decode_native_error(error) from error


def _configure_builder(
    *,
    session_id: str | None,
    provider: Provider,
    system_prompt: str | PromptProfile | None,
    generation: GenerationConfig | None,
    context_window_tokens: int | None,
    compaction: CompactionConfig | None,
    workspace: WorkspaceConfig | None,
    session_store: PathInput | None,
    history_retention: HistoryRetention | None,
    tools: Sequence[ToolRegistration],
    hooks: Hooks | None,
    max_model_turns: int | None,
    structured_output_retry_policy: StructuredOutputRetryPolicy | None,
    event_buffer_size: int | None,
) -> AgentBuilder:
    from ._builder import AgentBuilder

    builder = AgentBuilder(session_id).provider(provider)
    if system_prompt is not None:
        builder.system_prompt(system_prompt)
    if generation is not None:
        builder.generation(generation)
    if context_window_tokens is not None:
        builder.context_window_tokens(context_window_tokens)
    if compaction is not None:
        builder.compaction(compaction)
    if workspace is not None:
        builder.workspace(workspace)
    if session_store is not None:
        builder.session_store(session_store)
    if history_retention is not None:
        builder.history_retention(history_retention)
    if max_model_turns is not None:
        builder.max_model_turns(max_model_turns)
    if structured_output_retry_policy is not None:
        builder.structured_output_retry_policy(structured_output_retry_policy)
    if event_buffer_size is not None:
        builder.event_buffer_size(event_buffer_size)
    if hooks is not None:
        builder.hooks(hooks)
    if tools:
        builder.tools(tools)
    return builder
