"""Typed Python construction facade for Rust-owned Merry agents."""

from __future__ import annotations

from collections.abc import Callable, Sequence
from typing import TYPE_CHECKING, NoReturn, overload
from uuid import uuid4

from . import _merry
from ._config import (
    Anthropic,
    OpenAICompatible,
    PathInput,
    Provider,
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
from ._json import dump_json
from ._tools import InputT, OutputT, Tool, ToolHandler, ToolRegistration

if TYPE_CHECKING:
    from ._agent import Agent

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
        self._provider_configured = False
        self._consumed = False

    def provider(self, provider: Provider) -> AgentBuilder:
        """Configure the primary model provider."""

        self._ensure_open()
        try:
            if isinstance(provider, OpenAICompatible):
                self._native.with_openai_compatible(
                    provider.api_key,
                    provider.model,
                    provider.base_url,
                    provider.protocol,
                )
            elif isinstance(provider, Anthropic):
                self._native.with_anthropic(
                    provider.api_key,
                    provider.model,
                    provider.base_url,
                )
            else:
                raise TypeError("provider must be OpenAICompatible or Anthropic")
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        self._provider_configured = True
        return self

    def workspace(self, config: WorkspaceConfig) -> AgentBuilder:
        """Configure the Rust-owned workspace profile."""

        self._ensure_open()
        patch_scope: list[str] | None = None
        patch_forbidden: Sequence[str] = ()
        if config.patch is not None:
            patch_scope = [str(path) for path in config.patch.write_scope]
            patch_forbidden = tuple(str(path) for path in config.patch.forbidden_paths)
        forbidden = [str(path) for path in config.forbidden_paths]
        forbidden.extend(patch_forbidden)
        try:
            self._native.with_workspace(
                [str(path) for path in config.roots],
                [str(path) for path in config.readonly_resource_roots],
                config.allow_hidden,
                config.patch is not None,
                patch_scope,
                forbidden,
                config.limits.max_read_bytes,
                config.limits.max_write_bytes,
                config.limits.max_patch_bytes,
                config.limits.max_list_entries,
                config.limits.max_search_matches,
                config.limits.max_search_files,
                config.limits.max_search_entries,
                config.limits.max_search_bytes,
                config.limits.max_search_line_bytes,
                config.limits.max_search_query_bytes,
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def session_store(self, path: PathInput) -> AgentBuilder:
        """Configure the file store used by save and resume operations."""

        self._ensure_open()
        try:
            self._native.with_session_store(str(path))
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def max_model_turns(self, value: int) -> AgentBuilder:
        """Set the maximum number of model turns for a run."""

        self._ensure_open()
        require_positive_int("max_model_turns", value)
        try:
            self._native.with_max_model_turns(value)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def event_buffer_size(self, value: int) -> AgentBuilder:
        """Set the bounded runtime event buffer size."""

        self._ensure_open()
        require_positive_int("event_buffer_size", value)
        try:
            self._native.with_event_buffer_size(value)
        except NativeMerryError as error:
            self._handle_native_error(error)
        return self

    def register_tool(self, tool: Tool[InputT, OutputT]) -> Tool[InputT, OutputT]:
        """Register one typed host tool."""

        self._ensure_open()
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
        self._ensure_open()

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

    def build(self) -> Agent:
        """Consume this builder and create a lazy Rust-owned Agent."""

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

        native_builder = self._native
        self._consumed = True
        return Agent._from_builder(
            native_builder,
            tools=tuple(self._tools.values()),
            session_id=self._session_id,
        )

    async def resume(self, path: PathInput | None = None) -> Agent:
        """Consume this builder and resume from its configured or explicit store."""

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
        self._consumed = True
        try:
            native_agent = await self._native.resume(resolved_path)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        return Agent._from_native(native_agent, tools=tuple(self._tools.values()))

    def _register_tool(self, tool: ToolRegistration) -> None:
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
        try:
            self._native.register_bridge_tool(
                tool.name,
                tool.description,
                dump_json(tool.schema, "tool input schema"),
            )
        except NativeMerryError as error:
            self._handle_native_error(error)
        self._tools[tool.name] = tool

    def _handle_native_error(self, error: NativeMerryError) -> NoReturn:
        if self._native.is_consumed():
            self._consumed = True
        raise _decode_native_error(error) from error

    def _ensure_open(self) -> None:
        if self._consumed:
            raise builder_consumed_error()
