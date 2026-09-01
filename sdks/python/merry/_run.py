"""Async Python run handle for the Rust-owned agent lifecycle."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Sequence
from typing import Generic, Protocol, TypeVar

from pydantic import BaseModel

from ._errors import (
    MerryError,
    MerryErrorInfo,
    MerryInternalError,
    NativeMerryError,
    _decode_native_error,
)
from ._events import Event
from ._json import dump_json
from ._models import RunResult, ToolCallBatch, ToolResult, ToolSubmission
from ._protocol import parse_message, parse_run_result, parse_submission

_LOGGER = logging.getLogger(__name__)
OutputT = TypeVar("OutputT", bound=BaseModel)


class _NativeRun(Protocol):
    def next(self) -> Awaitable[str | None]: ...

    def submit_tool_results(
        self, batch_id: str, results_json: str
    ) -> Awaitable[str]: ...

    def result(self) -> Awaitable[str]: ...

    def cancel(self) -> Awaitable[str]: ...


_NON_TERMINAL_ERROR_CODES = frozenset(
    {
        "agent_run_not_finished",
        "runtime.run_state",
        "tool_batch_mismatch",
        "tool_batch_not_pending",
        "tool_batch_pending",
        "tool_batch_resolved",
        "unsupported_tool_result_content",
    }
)


class AgentRun(Generic[OutputT]):
    """Single-consumer async run with explicit tool batch handoff."""

    def __init__(self, native: _NativeRun, output_model: type[OutputT] | None) -> None:
        self._native = native
        self._output_model = output_model
        self._eof = False
        self._result: RunResult[OutputT] | None = None
        self._terminal_error: MerryError | None = None

    def __aiter__(self) -> AgentRun[OutputT]:
        return self

    async def __anext__(self) -> Event | ToolCallBatch[OutputT]:
        message = await self.next()
        if message is None:
            raise StopAsyncIteration
        return message

    async def next(self) -> Event | ToolCallBatch[OutputT] | None:
        """Return the next event or tool batch, or `None` at message EOF."""

        self._raise_terminal_error()
        if self._eof:
            return None
        try:
            payload = await self._native.next()
            if payload is None:
                self._eof = True
                return None
            if not isinstance(payload, str):
                raise TypeError("native AgentRun.next() must return str or None")
            try:
                return parse_message(payload, self)
            except (TypeError, KeyError, ValueError):
                error = self._protocol_error(
                    "protocol.native_message_invalid",
                    "The native run message did not match the SDK protocol.",
                )
                self._cache_terminal_error(error)
                raise error from None
        except NativeMerryError as error:
            decoded = _decode_native_error(error)
            if self._is_terminal_native_error(decoded):
                self._cache_terminal_error(decoded)
            raise decoded from error
        except asyncio.CancelledError:
            await self._cancel_after_task_cancellation()
            raise

    async def result(self) -> RunResult[OutputT]:
        """Return the durable terminal result after the run reaches EOF."""

        if self._result is not None:
            return self._result
        self._raise_terminal_error()
        try:
            payload = await self._native.result()
        except NativeMerryError as error:
            decoded = _decode_native_error(error)
            if self._eof and decoded.code not in _NON_TERMINAL_ERROR_CODES:
                self._cache_terminal_error(decoded)
            raise decoded from error
        except asyncio.CancelledError:
            await self._cancel_after_task_cancellation()
            raise

        try:
            result = self._parse_result(payload)
        except MerryError as error:
            self._cache_terminal_error(error)
            raise
        self._result = result
        return result

    async def cancel(self) -> RunResult[OutputT]:
        """Cancel the run and return its durable terminal result."""

        if self._result is not None:
            return self._result
        self._raise_terminal_error()
        try:
            payload = await self._native.cancel()
        except NativeMerryError as error:
            decoded = _decode_native_error(error)
            self._cache_terminal_error(decoded)
            raise decoded from error
        except asyncio.CancelledError:
            await self._cancel_after_task_cancellation()
            raise

        try:
            result = self._parse_result(payload)
        except MerryError as error:
            self._cache_terminal_error(error)
            raise
        self._result = result
        self._eof = True
        return result

    async def close(self) -> None:
        """Cancel an unfinished run and wait for its terminal result."""

        if self._terminal_error is not None:
            self._raise_terminal_error()
        if not self.finished:
            await self.cancel()
        elif self._result is None:
            await self.result()

    @property
    def finished(self) -> bool:
        """Whether a terminal result or message EOF has been observed."""

        return self._result is not None or self._terminal_error is not None or self._eof

    async def _submit_batch(
        self,
        batch_id: str,
        results: Sequence[ToolResult],
    ) -> ToolSubmission:
        """Submit the complete result set for one active tool batch."""

        self._raise_terminal_error()
        payload = dump_json(
            [result.to_wire() for result in results],
            "tool result batch",
        )
        try:
            native_payload = await self._native.submit_tool_results(batch_id, payload)
            if not isinstance(native_payload, str):
                raise TypeError("native tool submission must return a JSON string")
            try:
                return parse_submission(native_payload)
            except (TypeError, KeyError, ValueError):
                error = self._protocol_error(
                    "protocol.native_submission_invalid",
                    "The native tool submission did not match the SDK protocol.",
                )
                self._cache_terminal_error(error)
                raise error from None
        except NativeMerryError as error:
            decoded = _decode_native_error(error)
            if self._is_terminal_native_error(decoded):
                self._cache_terminal_error(decoded)
            raise decoded from error
        except asyncio.CancelledError:
            await self._cancel_after_task_cancellation()
            raise

    async def _cancel_after_task_cancellation(self) -> None:
        try:
            cancellation = self.cancel()
            await asyncio.shield(cancellation)
        except (
            asyncio.CancelledError,
            MerryError,
            OSError,
            RuntimeError,
            TypeError,
        ) as cleanup_error:
            _LOGGER.warning(
                "failed to persist cancellation for an AgentRun; error_type=%s",
                type(cleanup_error).__name__,
            )

    def _parse_result(self, payload: object) -> RunResult[OutputT]:
        try:
            if not isinstance(payload, str):
                raise TypeError("native AgentRun result must be a JSON string")
            return parse_run_result(payload, self._output_model)
        except MerryError:
            raise
        except (TypeError, KeyError, ValueError):
            raise self._protocol_error(
                "protocol.native_result_invalid",
                "The native run result did not match the SDK protocol.",
            ) from None

    @staticmethod
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

    def _cache_terminal_error(self, error: MerryError) -> None:
        self._terminal_error = error
        self._eof = True

    def _raise_terminal_error(self) -> None:
        error = self._terminal_error
        if error is not None:
            raise error

    @staticmethod
    def _is_terminal_native_error(error: MerryError) -> bool:
        return error.code not in _NON_TERMINAL_ERROR_CODES
