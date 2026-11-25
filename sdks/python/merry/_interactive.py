"""Async Python interactive session handle for long-lived multi-turn conversations."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Sequence
from dataclasses import dataclass
from types import TracebackType
from typing import Protocol, Self

from ._config import (
    UNSET,
    CompactionConfig,
    CompactionOutcome,
    GenerationConfig,
    InteractiveSettingsUpdate,
    PathInput,
    _UnsetSentinel,
)
from ._errors import (
    MerryError,
    MerryErrorInfo,
    MerryInternalError,
    NativeMerryError,
    _decode_native_error,
)
from ._event_types import QueuedInputs
from ._events import Event
from ._hooks import HookDecision, HookInvocation, Hooks
from ._json import JsonValue, dump_json
from ._models import ToolCallBatch, ToolResult, ToolSubmission
from ._protocol import (
    parse_compaction_outcome,
    parse_interactive_input_item,
    parse_interactive_snapshot,
    parse_message,
    parse_submission,
)
from ._tools import ToolRegistry

_LOGGER = logging.getLogger(__name__)


class _NativeInteractiveRun(Protocol):
    def next(self) -> Awaitable[str | None]: ...

    def submit_tool_results(
        self, batch_id: str, results_json: str
    ) -> Awaitable[str]: ...

    def submit_hook_decision(
        self, invocation_id: str, decision_json: str
    ) -> Awaitable[None]: ...

    def fail_hook_invocation(
        self, invocation_id: str, message: str
    ) -> Awaitable[None]: ...

    def submit(self, text: str) -> Awaitable[str]: ...

    def update_input(self, input_id: int, text: str) -> Awaitable[None]: ...

    def remove_input(self, input_id: int) -> Awaitable[None]: ...

    def set_paused(self, paused: bool) -> Awaitable[None]: ...

    def flush(self) -> Awaitable[None]: ...

    def snapshot(self) -> Awaitable[str]: ...

    def interrupt(self) -> Awaitable[None]: ...

    def save_session(self, path: str | None = None) -> Awaitable[None]: ...

    def update_settings(self, settings_json: str) -> Awaitable[None]: ...

    def compact(self, focus: str | None = None) -> Awaitable[str | None]: ...

    def close(self) -> Awaitable[None]: ...


@dataclass(frozen=True, slots=True)
class InteractiveInputItem:
    """Handle for one queued interactive input assigned by the Rust runtime."""

    id: int
    text: str
    _queue: InputQueue

    async def update(self, text: str) -> None:
        """Replace the text of this pending input before it is accepted."""

        await self._queue.update(self.id, text)

    async def remove(self) -> None:
        """Remove this pending input before it is accepted."""

        await self._queue.remove(self.id)


class InputQueue:
    """Controls for the Rust-owned pending input queue of an interactive session."""

    def __init__(self, native: _NativeInteractiveRun) -> None:
        self._native = native

    async def pause(self) -> None:
        """Pause automatic dispatch of pending queued inputs at turn boundaries."""

        try:
            await self._native.set_paused(True)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    async def resume(self) -> None:
        """Resume automatic dispatch and send queued inputs if the session is idle."""

        try:
            await self._native.set_paused(False)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    async def flush(self) -> None:
        """Unpause and immediately dispatch all pending queued inputs when idle."""

        try:
            await self._native.flush()
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    async def snapshot(self) -> QueuedInputs:
        """Return a snapshot of the current pending input queue."""

        try:
            payload = await self._native.snapshot()
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        try:
            return parse_interactive_snapshot(payload)
        except (TypeError, KeyError, ValueError):
            raise _protocol_error(
                "protocol.native_interactive_snapshot_invalid",
                "The native interactive queue snapshot was invalid.",
            ) from None

    async def update(self, input_id: int, text: str) -> None:
        """Update a pending queued input by its runtime-assigned id."""

        _require_nonnegative_int("input_id", input_id)
        try:
            await self._native.update_input(input_id, text)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    async def remove(self, input_id: int) -> None:
        """Remove a pending queued input by its runtime-assigned id."""

        _require_nonnegative_int("input_id", input_id)
        try:
            await self._native.remove_input(input_id)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error


class _InteractiveSessionCore:
    """Shared implementation for auto-hook and manual-hook interactive sessions."""

    def __init__(
        self,
        native: _NativeInteractiveRun,
        *,
        hooks: Hooks | None = None,
        tools: ToolRegistry | None = None,
        auto_hooks: bool = True,
        auto_tools: bool = False,
    ) -> None:
        self._native = native
        self._queue = InputQueue(native)
        self._hooks = hooks
        self._tools = tools
        self._auto_hooks = auto_hooks
        self._auto_tools = auto_tools
        self._closed = False

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        if exc_type is asyncio.CancelledError:
            await self._close_after_task_cancellation()
            return
        if exc_type is not None:
            try:
                await self.close()
            except MerryError:
                pass
            return
        await self.close()

    def __aiter__(self) -> Self:
        return self

    @property
    def queue(self) -> InputQueue:
        """Return the pending input queue controller for this session."""

        return self._queue

    @property
    def closed(self) -> bool:
        """Whether this interactive session has reached EOF or been closed."""

        return self._closed

    async def _next_raw(self) -> Event | ToolCallBatch | HookInvocation | None:
        while True:
            if self._closed:
                return None
            try:
                payload = await self._native.next()
            except NativeMerryError as error:
                raise _decode_native_error(error) from error
            except asyncio.CancelledError:
                await self._close_after_task_cancellation()
                raise

            if payload is None:
                self._closed = True
                return None
            if not isinstance(payload, str):
                raise TypeError("native InteractiveRun.next() must return str or None")
            try:
                message = parse_message(payload, self, self)
            except (TypeError, KeyError, ValueError):
                raise _protocol_error(
                    "protocol.native_message_invalid",
                    "The native interactive message did not match the SDK protocol.",
                ) from None

            if (
                isinstance(message, HookInvocation)
                and self._auto_hooks
                and self._hooks is not None
            ):
                try:
                    decision = await self._hooks.evaluate(message)
                except asyncio.CancelledError:
                    await self._close_after_task_cancellation()
                    raise
                except Exception as hook_error:
                    try:
                        await message.fail(f"{type(hook_error).__name__}: {hook_error}")
                    except MerryError:
                        pass
                    raise
                await message.submit(decision)
                continue

            if (
                isinstance(message, ToolCallBatch)
                and self._auto_tools
                and self._tools is not None
            ):
                await self._tools.execute(message)
                continue

            return message

    async def send(self, text: str) -> InteractiveInputItem:
        """Send user input into the session (runs immediately when idle, otherwise queues)."""

        try:
            payload = await self._native.submit(text)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        try:
            input_id, accepted_text = parse_interactive_input_item(payload)
        except (TypeError, KeyError, ValueError):
            raise _protocol_error(
                "protocol.native_interactive_input_invalid",
                "The native interactive input acknowledgement was invalid.",
            ) from None
        return InteractiveInputItem(
            id=input_id,
            text=accepted_text,
            _queue=self._queue,
        )

    async def interrupt(self) -> None:
        """Interrupt the active model or tool turn and return to idle."""

        try:
            await self._native.interrupt()
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    async def save_session(self, path: PathInput | None = None) -> None:
        """Persist the session while the interactive loop is at an idle boundary."""

        resolved = None if path is None else str(path)
        try:
            await self._native.save_session(resolved)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    async def update_settings(
        self,
        update: InteractiveSettingsUpdate | None = None,
        *,
        context_window_tokens: int | None | _UnsetSentinel = UNSET,
        generation: GenerationConfig | None = None,
        compaction: CompactionConfig | None = None,
        max_parallel_tool_calls: int | None = None,
    ) -> None:
        """Apply runtime settings at the next model-request boundary.

        Accepts either an explicit `InteractiveSettingsUpdate` or keyword
        overrides directly. Passing `context_window_tokens=None` clears an
        existing session-level override.
        """

        if update is not None:
            if not isinstance(update, InteractiveSettingsUpdate):
                raise TypeError("update must be an InteractiveSettingsUpdate or None")
            if (
                not isinstance(context_window_tokens, _UnsetSentinel)
                or generation is not None
                or compaction is not None
                or max_parallel_tool_calls is not None
            ):
                raise ValueError(
                    "pass either an InteractiveSettingsUpdate object or keyword fields, not both"
                )
            resolved_update = update
        else:
            resolved_update = InteractiveSettingsUpdate(
                context_window_tokens=context_window_tokens,
                generation=generation,
                compaction=compaction,
                max_parallel_tool_calls=max_parallel_tool_calls,
            )

        wire: dict[str, JsonValue] = {}
        if not isinstance(resolved_update.context_window_tokens, _UnsetSentinel):
            wire["context_window_tokens"] = resolved_update.context_window_tokens
        if resolved_update.generation is not None:
            gen = resolved_update.generation
            wire["generation"] = {
                "max_output_tokens": gen.max_output_tokens,
                "parallel_tool_calls": gen.parallel_tool_calls.value,
                "reasoning_effort": gen.reasoning_effort,
                "service_tier": None
                if gen.service_tier is None
                else gen.service_tier.value,
            }
        if resolved_update.compaction is not None:
            comp = resolved_update.compaction
            wire["compaction"] = {
                "enabled": comp.enabled,
                "reasoning_effort": comp.reasoning_effort,
            }
        if resolved_update.max_parallel_tool_calls is not None:
            wire["max_parallel_tool_calls"] = resolved_update.max_parallel_tool_calls

        payload = dump_json(wire, "interactive settings update")
        try:
            await self._native.update_settings(payload)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error

    async def compact(self, focus: str | None = None) -> CompactionOutcome | None:
        """Manually compact session history while the interactive loop is idle."""

        if focus is not None and not isinstance(focus, str):
            raise TypeError("focus must be a string or None")
        try:
            payload = await self._native.compact(focus)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        if payload is None:
            return None
        if not isinstance(payload, str):
            raise TypeError("native InteractiveRun.compact() must return str or None")
        try:
            return parse_compaction_outcome(payload)
        except (TypeError, KeyError, ValueError):
            raise _protocol_error(
                "protocol.native_compaction_outcome_invalid",
                "The native compaction outcome was invalid.",
            ) from None

    async def close(self) -> None:
        """Close the interactive session and wait for the Rust producer to shut down."""

        if self._closed:
            return
        try:
            await self._native.close()
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        finally:
            self._closed = True

    async def _submit_batch(
        self,
        batch_id: str,
        results: Sequence[ToolResult],
    ) -> ToolSubmission:
        payload = dump_json(
            [result.to_wire() for result in results],
            "tool result batch",
        )
        try:
            native_payload = await self._native.submit_tool_results(batch_id, payload)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        except asyncio.CancelledError:
            await self._close_after_task_cancellation()
            raise

        if not isinstance(native_payload, str):
            raise TypeError("native tool submission must return a JSON string")
        try:
            return parse_submission(native_payload)
        except (TypeError, KeyError, ValueError):
            raise _protocol_error(
                "protocol.native_submission_invalid",
                "The native tool submission did not match the SDK protocol.",
            ) from None

    async def _submit_hook_decision(
        self,
        invocation_id: str,
        decision: HookDecision,
    ) -> None:
        payload = dump_json(decision.to_wire(), "hook decision")
        try:
            await self._native.submit_hook_decision(invocation_id, payload)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        except asyncio.CancelledError:
            await self._close_after_task_cancellation()
            raise

    async def _fail_hook_invocation(
        self,
        invocation_id: str,
        message: str,
    ) -> None:
        try:
            await self._native.fail_hook_invocation(invocation_id, message)
        except NativeMerryError as error:
            raise _decode_native_error(error) from error
        except asyncio.CancelledError:
            await self._close_after_task_cancellation()
            raise

    async def _close_after_task_cancellation(self) -> None:
        try:
            await asyncio.shield(self.close())
        except (
            asyncio.CancelledError,
            MerryError,
            OSError,
            RuntimeError,
            TypeError,
        ) as cleanup_error:
            _LOGGER.warning(
                "failed to close an InteractiveSession during cleanup; error_type=%s",
                type(cleanup_error).__name__,
            )


class InteractiveSession(_InteractiveSessionCore):
    """Long-lived Rust-owned interactive session with automatic hook dispatch."""

    def __init__(
        self,
        native: _NativeInteractiveRun,
        *,
        hooks: Hooks | None = None,
        tools: ToolRegistry | None = None,
        auto_tools: bool = False,
    ) -> None:
        super().__init__(
            native,
            hooks=hooks,
            tools=tools,
            auto_hooks=True,
            auto_tools=auto_tools,
        )

    async def __anext__(self) -> Event | ToolCallBatch:
        message = await self.next()
        if message is None:
            raise StopAsyncIteration
        return message

    async def next(self) -> Event | ToolCallBatch | None:
        """Return the next runtime event or host tool batch, or `None` after close."""

        message = await self._next_raw()
        if isinstance(message, HookInvocation):
            raise _protocol_error(
                "protocol.unexpected_hook_invocation",
                "Received a native hook invocation without registered Python hooks.",
            )
        return message


class ManualHookInteractiveSession(_InteractiveSessionCore):
    """Long-lived interactive session yielding `HookInvocation` messages manually."""

    def __init__(
        self,
        native: _NativeInteractiveRun,
        *,
        hooks: Hooks | None = None,
        tools: ToolRegistry | None = None,
        auto_tools: bool = False,
    ) -> None:
        super().__init__(
            native,
            hooks=hooks,
            tools=tools,
            auto_hooks=False,
            auto_tools=auto_tools,
        )

    async def __anext__(self) -> Event | ToolCallBatch | HookInvocation:
        message = await self.next()
        if message is None:
            raise StopAsyncIteration
        return message

    async def next(self) -> Event | ToolCallBatch | HookInvocation | None:
        """Return the next runtime event, tool batch, or hook invocation, or `None` after close."""

        return await self._next_raw()


def _require_nonnegative_int(name: str, value: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{name} must be an integer")
    if value < 0:
        raise ValueError(f"{name} must be non-negative")


def _protocol_error(code: str, message: str) -> MerryInternalError:
    return MerryInternalError(
        MerryErrorInfo(
            code=code,
            domain="internal",
            message=message,
            hint="Rebuild the Merry native extension from the matching source.",
            retryability="not_retryable",
        )
    )
