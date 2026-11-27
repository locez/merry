"""Typed Python construction facade for Rust-owned Merry agents."""

from __future__ import annotations

from collections.abc import Awaitable, Callable, Sequence
from typing import TYPE_CHECKING, NoReturn, Self, overload
from uuid import uuid4

from pydantic import BaseModel

from . import _merry
from ._config import (
    Anthropic,
    CompactionConfig,
    GenerationConfig,
    HistoryRetention,
    OpenAICompatible,
    PathInput,
    PromptProfile,
    Provider,
    StructuredOutputRetryPolicy,
    WorkspaceConfig,
    require_positive_int,
)
from ._errors import (
    MerryConfigError,
    MerryErrorInfo,
    NativeMerryError,
    _decode_native_error,
    builder_consumed_error,
)
from ._hooks import Hooks
from ._json import dump_json
from ._tools import Tool, ToolOutputValue, ToolRegistration

if TYPE_CHECKING:
    from ._agent import Agent, PendingAgent

_NativeBuilder = _merry.AgentBuilder


class AgentBuilder:
    """Build one Rust-owned session and its explicit Python tool registry."""

    def __init__(self, session_id: str | None = None) -> None:
        resolved_session_id = str(uuid4()) if session_id is None else session_id
        try:
            self._native: _NativeBuilder = _merry.AgentBuilder(resolved_session_id)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        self._session_id = resolved_session_id
        self._tools: dict[str, ToolRegistration] = {}
        self._hooks: Hooks | None = None
        self._provider_configured = False
        self._consumed = False

    def provider(self, provider: Provider) -> Self:
        """Configure the primary model provider."""

        self._ensure_open()
        try:
            if isinstance(provider, OpenAICompatible):
                self._native.with_openai_compatible(
                    provider.api_key,
                    provider.model,
                    provider.base_url,
                    provider.protocol,
                    provider.context_window_tokens,
                )
            elif isinstance(provider, Anthropic):
                self._native.with_anthropic(
                    provider.api_key,
                    provider.model,
                    provider.base_url,
                    provider.context_window_tokens,
                )
            else:
                raise TypeError("provider must be OpenAICompatible or Anthropic")
        except NativeMerryError as error:
            self._handle_native_error(error)
        self._provider_configured = True
        return self

    def system_prompt(self, prompt: str | PromptProfile) -> Self:
        """Configure stable system instructions or a full PromptProfile."""

        self._ensure_open()
        if isinstance(prompt, str):
            profile = PromptProfile(base_instructions=prompt)
        elif isinstance(prompt, PromptProfile):
            profile = prompt
        else:
            raise TypeError("system_prompt must be a string or PromptProfile")
        try:
            self._native.with_prompt_profile(
                profile.base_instructions,
                profile.progress_commentary,
                profile.progress_commentary_instructions,
                [(block.tag, block.text) for block in profile.stable_blocks],
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def generation(self, config: GenerationConfig) -> Self:
        """Configure provider-neutral model generation controls."""

        self._ensure_open()
        if not isinstance(config, GenerationConfig):
            raise TypeError("generation must be a GenerationConfig")
        try:
            self._native.with_generation_config(
                config.max_output_tokens,
                config.parallel_tool_calls.value,
                config.reasoning_effort,
                None if config.service_tier is None else config.service_tier.value,
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def workspace(self, config: WorkspaceConfig) -> Self:
        """Configure the Rust-owned workspace profile."""

        self._ensure_open()
        if not isinstance(config, WorkspaceConfig):
            raise TypeError("workspace must be a WorkspaceConfig")
        patch_scope: list[str] | None = None
        patch_forbidden: Sequence[str] = ()
        if config.patch is not None:
            patch_scope = [str(path) for path in config.patch.write_scope]
            patch_forbidden = tuple(str(path) for path in config.patch.forbidden_paths)
        forbidden = [str(path) for path in config.forbidden_paths]
        forbidden.extend(patch_forbidden)
        try:
            self._native.with_workspace(
                str(config.root),
                [str(path) for path in config.readonly_resource_roots],
                config.patch is not None,
                patch_scope,
                forbidden,
                config.limits.max_read_bytes,
                config.limits.max_read_lines,
                config.limits.max_write_bytes,
                config.limits.max_patch_bytes,
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def session_store(self, path: PathInput) -> Self:
        """Configure the directory-backed store used by save and resume."""

        self._ensure_open()
        try:
            self._native.with_session_store(str(path))
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def session_lock(self, enabled: bool = True) -> Self:
        """Control whether the session holds an exclusive OS file lock on its store."""

        self._ensure_open()
        if type(enabled) is not bool:
            raise TypeError("session_lock must be a boolean")
        try:
            self._native.with_session_lock(enabled)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def history_retention(self, retention: HistoryRetention) -> Self:
        """Configure how compacted history is retained in memory and on disk."""

        self._ensure_open()
        if not isinstance(retention, HistoryRetention):
            raise TypeError("history_retention must be a HistoryRetention")
        try:
            self._native.with_history_retention(
                retention.mode.value,
                retention.max_segments,
                retention.max_bytes,
                retention.export_markdown,
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def max_model_turns(self, value: int) -> Self:
        """Set the maximum number of model turns for a run."""

        self._ensure_open()
        require_positive_int("max_model_turns", value)
        try:
            self._native.with_max_model_turns(value)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def structured_output_retry_policy(
        self, policy: StructuredOutputRetryPolicy
    ) -> Self:
        """Set retries allowed when structured final-output decoding fails."""

        self._ensure_open()
        if not isinstance(policy, StructuredOutputRetryPolicy):
            raise TypeError(
                "structured_output_retry_policy must be a StructuredOutputRetryPolicy"
            )
        try:
            self._native.with_structured_output_retry_policy(policy.max_retries)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def context_window_tokens(self, tokens: int) -> Self:
        """Set an explicit context-window size in tokens for this session."""

        self._ensure_open()
        require_positive_int("context_window_tokens", tokens)
        try:
            self._native.with_context_window_tokens(tokens)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def compaction(self, config: CompactionConfig) -> Self:
        """Configure automatic checkpoint compaction policy for this session."""

        self._ensure_open()
        if not isinstance(config, CompactionConfig):
            raise TypeError("compaction must be a CompactionConfig")
        try:
            self._native.with_compaction(config.enabled, config.reasoning_effort)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def event_buffer_size(self, value: int) -> Self:
        """Set the bounded runtime event buffer size."""

        self._ensure_open()
        require_positive_int("event_buffer_size", value)
        try:
            self._native.with_event_buffer_size(value)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def hooks(self, hooks: Hooks) -> Self:
        """Install lifecycle hooks on this agent.

        Only the subscribed hook points (and tool filters) are registered with
        Rust; unregistered points emit no bridge messages.
        """

        self._ensure_open()
        if not isinstance(hooks, Hooks):
            raise TypeError("hooks must be a Hooks instance")
        snapshot = hooks.copy()
        self._hooks = None if snapshot.is_empty() else snapshot
        try:
            self._native.with_hook_subscriptions(
                dump_json(snapshot.subscriptions_wire(), "hook subscriptions")
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def register_tool(self, tool_obj: ToolRegistration) -> Self:
        """Register one typed host tool."""

        self._ensure_open()
        self._register_tool(tool_obj)
        return self

    def tools(self, tools: Sequence[ToolRegistration]) -> Self:
        """Register multiple typed host tools in provider-visible order."""

        self._ensure_open()
        for item in tools:
            self._register_tool(item)
        return self

    @overload
    def tool(
        self,
        handler: Callable[..., Awaitable[ToolOutputValue]],
        /,
    ) -> Tool: ...

    @overload
    def tool(
        self,
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[BaseModel] | None = None,
        output_model: type[BaseModel] | None = None,
        parallel_safe: bool = False,
    ) -> Callable[[Callable[..., Awaitable[ToolOutputValue]]], Tool]: ...

    def tool(
        self,
        handler: Callable[..., Awaitable[ToolOutputValue]] | None = None,
        *,
        name: str | None = None,
        description: str | None = None,
        input_model: type[BaseModel] | None = None,
        output_model: type[BaseModel] | None = None,
        parallel_safe: bool = False,
    ) -> Tool | Callable[[Callable[..., Awaitable[ToolOutputValue]]], Tool]:
        """Decorate an async function and register it on this builder."""

        self._ensure_open()

        def decorate(function: Callable[..., Awaitable[ToolOutputValue]]) -> Tool:
            built = Tool.from_function(
                function,
                name=name,
                description=description,
                input_model=input_model,
                output_model=output_model,
                parallel_safe=parallel_safe,
            )
            self._register_tool(built)
            return built

        if handler is None:
            return decorate
        return decorate(handler)

    def build(self) -> Agent:
        """Consume this builder and eagerly create a Rust-owned Agent."""

        self._ensure_open()
        if not self._provider_configured:
            raise MerryConfigError(
                MerryErrorInfo(
                    code="agent.primary_provider_missing",
                    domain="config",
                    message="An AgentBuilder requires a primary provider.",
                    hint="Call .provider(...) before .build().",
                    retryability="user_action_required",
                )
            )
        from ._agent import Agent

        try:
            native_agent = self._native.build()
        except NativeMerryError as error:
            self._handle_native_error(error)
        self._consumed = True
        return Agent._from_native(
            native_agent,
            tools=tuple(self._tools.values()),
            hooks=self._hooks,
        )

    def resume(self, path: PathInput | None = None) -> PendingAgent:
        """Consume this builder and resume from its configured or explicit store.

        Can be used either as `async with builder.resume(...) as agent:` or
        `agent = await builder.resume(...)`.
        """

        from ._agent import PendingAgent

        return PendingAgent(lambda: self._resume(path))

    async def _resume(self, path: PathInput | None = None) -> Agent:
        self._ensure_open()
        if not self._provider_configured:
            raise MerryConfigError(
                MerryErrorInfo(
                    code="agent.primary_provider_missing",
                    domain="config",
                    message="An AgentBuilder requires a primary provider.",
                    hint="Call .provider(...) before .resume().",
                    retryability="user_action_required",
                )
            )
        from ._agent import Agent

        resolved_path = None if path is None else str(path)
        try:
            awaitable = self._native.resume(resolved_path)
            self._consumed = True
            native_agent = await awaitable
        except NativeMerryError as error:
            self._handle_native_error(error)
        return Agent._from_native(
            native_agent,
            tools=tuple(self._tools.values()),
            hooks=self._hooks,
        )

    def _register_tool(self, tool_obj: ToolRegistration) -> None:
        if tool_obj.name in self._tools:
            raise MerryConfigError(
                MerryErrorInfo(
                    code="tool.duplicate_registration",
                    domain="tool",
                    message=f"Tool {tool_obj.name!r} is already registered.",
                    hint="Use one unique provider-visible name per Agent.",
                    retryability="user_action_required",
                )
            )
        try:
            self._native.register_bridge_tool(
                tool_obj.name,
                tool_obj.description,
                dump_json(tool_obj.schema, "tool input schema"),
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        self._tools[tool_obj.name] = tool_obj

    def _handle_native_error(self, error: NativeMerryError) -> NoReturn:
        if self._native.is_consumed():
            self._consumed = True
        raise _decode_native_error(error) from error

    def _ensure_open(self) -> None:
        if self._consumed:
            raise builder_consumed_error()
