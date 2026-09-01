"""Typed public protocol models for the Python SDK."""

from __future__ import annotations

import json
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING, Generic, TypeVar

from pydantic import BaseModel

from ._errors import MerryErrorInfo, ToolDomainError
from ._event_types import ArtifactReference, EventDiagnostic
from ._json import (
    JsonObject,
    JsonValue,
    _contains_control,
    dump_json,
    require_json_object,
    require_json_value,
    validate_object_keys,
)

if TYPE_CHECKING:
    from ._events import Event
    from ._run import AgentRun

OutputT = TypeVar("OutputT", bound=BaseModel)


@dataclass(frozen=True, slots=True)
class ToolCall:
    """One provider-neutral host tool invocation."""

    id: str
    name: str
    arguments: JsonObject

    def __post_init__(self) -> None:
        _validate_identifier("tool call id", self.id, 256)
        _validate_tool_name(self.name)
        object.__setattr__(
            self,
            "arguments",
            require_json_object(self.arguments, "tool call arguments"),
        )


@dataclass(frozen=True, slots=True)
class TextContent:
    """Text returned by a host tool."""

    text: str

    def __post_init__(self) -> None:
        _validate_text(
            "tool text content",
            self.text,
            1_048_576,
            allow_blank=True,
            allow_newline_tab=True,
        )


@dataclass(frozen=True, slots=True)
class JsonContent:
    """Validated JSON text returned by a host tool."""

    json: str

    def __post_init__(self) -> None:
        if not isinstance(self.json, str):
            raise TypeError("tool JSON content must be a string")
        if len(self.json) > 1_048_576:
            raise ValueError("tool JSON content is too long")
        try:
            decoded: object = json.loads(self.json)
        except json.JSONDecodeError as error:
            raise ValueError("tool JSON content must be valid JSON") from error
        require_json_value(decoded, "tool JSON content")

    @classmethod
    def from_value(cls, value: JsonValue) -> JsonContent:
        return cls(json=dump_json(value, "tool JSON content"))


ToolContent = TextContent | JsonContent


@dataclass(frozen=True, slots=True)
class ToolDiagnostic:
    """The narrow diagnostic accepted by Rust for one tool result."""

    code: str
    message: str

    def __post_init__(self) -> None:
        _validate_identifier("tool diagnostic code", self.code, 128)
        _validate_text("tool diagnostic message", self.message, 4096)

    @classmethod
    def from_error_info(cls, info: MerryErrorInfo) -> ToolDiagnostic:
        return cls(code=info.code, message=info.message)


@dataclass(frozen=True, slots=True)
class ToolResult:
    """One success or domain-failure result for a tool call."""

    call_id: str
    content: ToolContent
    diagnostic: ToolDiagnostic | MerryErrorInfo | None = None

    def __post_init__(self) -> None:
        _validate_identifier("tool result call id", self.call_id, 256)
        if not isinstance(self.content, (TextContent, JsonContent)):
            raise TypeError("tool result content must be TextContent or JsonContent")
        if self.diagnostic is None:
            return
        if isinstance(self.diagnostic, MerryErrorInfo):
            if self.diagnostic.domain != "tool":
                raise ValueError("tool result diagnostics must use the tool domain")
            return
        if not isinstance(self.diagnostic, ToolDiagnostic):
            raise TypeError(
                "tool result diagnostic must be ToolDiagnostic, MerryErrorInfo, or None"
            )

    @classmethod
    def succeeded(cls, call_id: str, content: ToolContent) -> ToolResult:
        return cls(call_id=call_id, content=content)

    @classmethod
    def failed(
        cls,
        call_id: str,
        content: ToolContent,
        diagnostic: ToolDiagnostic | MerryErrorInfo,
    ) -> ToolResult:
        return cls(call_id=call_id, content=content, diagnostic=diagnostic)

    @classmethod
    def from_domain_error(cls, call_id: str, error: ToolDomainError) -> ToolResult:
        content = error.content
        if isinstance(content, str):
            normalized: ToolContent = TextContent(content)
        else:
            normalized = JsonContent.from_value(
                require_json_value(content, "tool failure content")
            )
        return cls.failed(call_id, normalized, error.info)

    def to_wire(self) -> JsonObject:
        if isinstance(self.content, TextContent):
            content: JsonObject = {"kind": "text", "text": self.content.text}
        else:
            content = {"kind": "json", "json": self.content.json}
        result: JsonObject = {
            "status": "failed" if self.diagnostic is not None else "succeeded",
            "call_id": self.call_id,
            "content": content,
        }
        if self.diagnostic is not None:
            result["diagnostic"] = {
                "code": self.diagnostic.code,
                "message": self.diagnostic.message,
            }
        return result


class ToolSubmission(str, Enum):
    """Outcome of a complete Rust-owned tool batch submission."""

    ACCEPTED = "accepted"
    REJECTED_AND_RECORDED = "rejected_and_recorded"


@dataclass(frozen=True, slots=True)
class ToolCallBatch(Generic[OutputT]):
    """An exclusive batch lease that must be submitted before the run advances."""

    id: str
    invocations: tuple[ToolCall, ...]
    _run: AgentRun[OutputT]

    def __post_init__(self) -> None:
        _validate_identifier("tool call batch id", self.id, 128)
        if not self.invocations:
            raise ValueError("tool call batch must contain at least one invocation")
        call_ids = [invocation.id for invocation in self.invocations]
        if len(call_ids) != len(set(call_ids)):
            raise ValueError("tool call batch invocation ids must be unique")

    async def submit(self, results: Sequence[ToolResult]) -> ToolSubmission:
        """Submit one result for every invocation in this batch."""

        return await self._run._submit_batch(self.id, results)


class RunStatus(str, Enum):
    """Terminal status reported by the Rust agent loop."""

    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    BLOCKED = "blocked"
    UNKNOWN = "unknown"


@dataclass(frozen=True, slots=True)
class BlockedReason:
    """Structured reason for a policy-blocked run."""

    kind: str
    data: JsonObject

    def __post_init__(self) -> None:
        _validate_identifier("blocked reason kind", self.kind, 128)
        object.__setattr__(
            self, "data", require_json_object(self.data, "blocked reason data")
        )


@dataclass(frozen=True, slots=True)
class ModelUsage:
    input_tokens: int
    cached_input_tokens: int | None
    output_tokens: int
    reasoning_output_tokens: int | None
    total_tokens: int

    def __post_init__(self) -> None:
        _validate_u64("input tokens", self.input_tokens)
        _validate_optional_u64("cached input tokens", self.cached_input_tokens)
        _validate_u64("output tokens", self.output_tokens)
        _validate_optional_u64("reasoning output tokens", self.reasoning_output_tokens)
        _validate_u64("total tokens", self.total_tokens)


@dataclass(frozen=True, slots=True)
class UsageContextWindow:
    resolved_model_window_tokens: int
    effective_window_tokens: int
    source: str

    def __post_init__(self) -> None:
        _validate_u64("resolved model window tokens", self.resolved_model_window_tokens)
        _validate_u64("effective window tokens", self.effective_window_tokens)
        if self.source not in {
            "explicit_config",
            "provider_capabilities",
            "bundled_catalog",
            "fallback",
        }:
            raise ValueError("usage context window source is unsupported")


@dataclass(frozen=True, slots=True)
class CompactionUsageWindow:
    auto_compaction_enabled: bool
    dynamic_body_estimated_tokens: int | None
    body_budget_tokens: int
    soft_water_tokens: int
    hard_water_tokens: int

    def __post_init__(self) -> None:
        if not isinstance(self.auto_compaction_enabled, bool):
            raise TypeError("auto compaction enabled must be a boolean")
        _validate_optional_u64(
            "dynamic body estimated tokens", self.dynamic_body_estimated_tokens
        )
        _validate_u64("body budget tokens", self.body_budget_tokens)
        _validate_u64("soft water tokens", self.soft_water_tokens)
        _validate_u64("hard water tokens", self.hard_water_tokens)


@dataclass(frozen=True, slots=True)
class SessionUsage:
    total: ModelUsage
    last: ModelUsage
    context: UsageContextWindow | None
    compaction: CompactionUsageWindow | None

    def __post_init__(self) -> None:
        if not isinstance(self.total, ModelUsage):
            raise TypeError("session total usage must be ModelUsage")
        if not isinstance(self.last, ModelUsage):
            raise TypeError("session last usage must be ModelUsage")
        if self.context is not None and not isinstance(
            self.context, UsageContextWindow
        ):
            raise TypeError("session context usage must be UsageContextWindow or None")
        if self.compaction is not None and not isinstance(
            self.compaction, CompactionUsageWindow
        ):
            raise TypeError(
                "session compaction usage must be CompactionUsageWindow or None"
            )


@dataclass(frozen=True, slots=True)
class FinalOutputRecord:
    """Recorded structured output metadata and its decoded JSON value."""

    call_id: str
    artifact: ArtifactReference
    value: JsonValue

    def __post_init__(self) -> None:
        _validate_identifier("final output call id", self.call_id, 256)
        if not isinstance(self.artifact, ArtifactReference):
            raise TypeError("final output artifact must be an ArtifactReference")
        object.__setattr__(
            self, "value", require_json_value(self.value, "final output JSON")
        )


@dataclass(frozen=True, slots=True)
class RunResult(Generic[OutputT]):
    """Rust-owned terminal result projected into typed Python values."""

    status: RunStatus
    events: tuple[Event, ...]
    model_turns_run: int
    final_output: str | None
    final_output_json: JsonValue | None
    structured_output: OutputT | None
    session_usage: SessionUsage | None
    diagnostic: EventDiagnostic | None = None
    blocked_reason: BlockedReason | None = None
    final_output_record: FinalOutputRecord | None = None

    def __post_init__(self) -> None:
        from ._events import Event

        if not isinstance(self.status, RunStatus):
            raise TypeError("run result status must be a RunStatus")
        if not isinstance(self.events, tuple) or any(
            not isinstance(event, Event) for event in self.events
        ):
            raise TypeError("run result events must be a tuple of Event values")
        _validate_usize("model turns run", self.model_turns_run)
        if self.final_output is not None:
            _validate_text(
                "final output",
                self.final_output,
                1_048_576,
                allow_newline_tab=True,
            )
        if self.final_output_json is not None:
            object.__setattr__(
                self,
                "final_output_json",
                require_json_value(self.final_output_json, "final output JSON"),
            )
        if self.structured_output is not None and not isinstance(
            self.structured_output, BaseModel
        ):
            raise TypeError("structured output must be a Pydantic BaseModel or None")
        if self.session_usage is not None and not isinstance(
            self.session_usage, SessionUsage
        ):
            raise TypeError("run session usage must be SessionUsage or None")
        if self.diagnostic is not None and not isinstance(
            self.diagnostic, EventDiagnostic
        ):
            raise TypeError("run diagnostic must be EventDiagnostic or None")
        if self.blocked_reason is not None and not isinstance(
            self.blocked_reason, BlockedReason
        ):
            raise TypeError("run blocked reason must be BlockedReason or None")
        if self.final_output_record is not None and not isinstance(
            self.final_output_record, FinalOutputRecord
        ):
            raise TypeError("run final output record must be FinalOutputRecord or None")
        if (
            self.final_output_record is not None
            and self.final_output_json != self.final_output_record.value
        ):
            raise ValueError("final output record value must match final_output_json")

        match self.status:
            case RunStatus.COMPLETED:
                if self.diagnostic is not None or self.blocked_reason is not None:
                    raise ValueError("completed runs must not have failure metadata")
            case RunStatus.FAILED | RunStatus.CANCELLED:
                if self.diagnostic is None or self.blocked_reason is not None:
                    raise ValueError(
                        "failed or cancelled runs require a diagnostic and no blocked reason"
                    )
            case RunStatus.BLOCKED:
                if self.blocked_reason is None or self.diagnostic is not None:
                    raise ValueError(
                        "blocked runs require a blocked reason and no diagnostic"
                    )
            case RunStatus.UNKNOWN:
                if self.diagnostic is not None or self.blocked_reason is not None:
                    raise ValueError("unknown runs must not have terminal metadata")


def _validate_identifier(name: str, value: str, maximum: int) -> None:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be a string")
    if not value or not value.strip() or value != value.strip():
        raise ValueError(f"{name} must be non-blank and trimmed")
    if len(value) > maximum:
        raise ValueError(f"{name} is too long")
    if _contains_control(value):
        raise ValueError(f"{name} must not contain control characters")


def _validate_tool_name(value: str) -> None:
    _validate_identifier("tool name", value, 64)
    first = value[0]
    if not (first.isascii() and (first.isalpha() or first == "_")):
        raise ValueError("tool name must start with an ASCII letter or underscore")
    if not all(
        character.isascii() and (character.isalnum() or character in {"_", "-"})
        for character in value
    ):
        raise ValueError(
            "tool name must use ASCII letters, digits, underscore, or hyphen"
        )


def _validate_text(
    name: str,
    value: str,
    maximum: int,
    *,
    allow_blank: bool = False,
    allow_newline_tab: bool = False,
) -> None:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be a string")
    if not allow_blank and not value.strip():
        raise ValueError(f"{name} must not be blank")
    if len(value) > maximum:
        raise ValueError(f"{name} is too long")
    if _contains_control(value, allow_newline_tab=allow_newline_tab):
        raise ValueError(f"{name} must not contain control characters")


_U64_MAX = (1 << 64) - 1
_USIZE_MAX = sys.maxsize


def _validate_u64(name: str, value: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{name} must be an integer")
    if value < 0 or value > _U64_MAX:
        raise ValueError(f"{name} must fit in an unsigned 64-bit integer")


def _validate_usize(name: str, value: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{name} must be an integer")
    if value < 0 or value > _USIZE_MAX:
        raise ValueError(f"{name} must fit in a native usize")


def _validate_optional_u64(name: str, value: int | None) -> None:
    if value is not None:
        _validate_u64(name, value)


def parse_session_usage(value: object) -> SessionUsage:
    data = require_json_object(value, "session usage")
    validate_object_keys(
        data,
        "session usage",
        required={"total", "last", "context", "compaction"},
    )
    total = _parse_model_usage(data["total"])
    last = _parse_model_usage(data["last"])
    context_value = data.get("context", None)
    compaction_value = data.get("compaction", None)
    context = None if context_value is None else _parse_context_window(context_value)
    compaction = (
        None if compaction_value is None else _parse_compaction_window(compaction_value)
    )
    return SessionUsage(total=total, last=last, context=context, compaction=compaction)


def _parse_model_usage(value: object) -> ModelUsage:
    data = require_json_object(value, "model usage")
    validate_object_keys(
        data,
        "model usage",
        required={
            "input_tokens",
            "cached_input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "total_tokens",
        },
    )
    return ModelUsage(
        input_tokens=_required_int(data, "input_tokens"),
        cached_input_tokens=_optional_int(data, "cached_input_tokens"),
        output_tokens=_required_int(data, "output_tokens"),
        reasoning_output_tokens=_optional_int(data, "reasoning_output_tokens"),
        total_tokens=_required_int(data, "total_tokens"),
    )


def _parse_context_window(value: object) -> UsageContextWindow:
    data = require_json_object(value, "usage context window")
    validate_object_keys(
        data,
        "usage context window",
        required={
            "resolved_model_window_tokens",
            "effective_window_tokens",
            "source",
        },
    )
    return UsageContextWindow(
        resolved_model_window_tokens=_required_int(
            data, "resolved_model_window_tokens"
        ),
        effective_window_tokens=_required_int(data, "effective_window_tokens"),
        source=_required_string(data, "source"),
    )


def _parse_compaction_window(value: object) -> CompactionUsageWindow:
    data = require_json_object(value, "compaction usage window")
    validate_object_keys(
        data,
        "compaction usage window",
        required={
            "auto_compaction_enabled",
            "dynamic_body_estimated_tokens",
            "body_budget_tokens",
            "soft_water_tokens",
            "hard_water_tokens",
        },
    )
    return CompactionUsageWindow(
        auto_compaction_enabled=_required_bool(data, "auto_compaction_enabled"),
        dynamic_body_estimated_tokens=_optional_int(
            data, "dynamic_body_estimated_tokens"
        ),
        body_budget_tokens=_required_int(data, "body_budget_tokens"),
        soft_water_tokens=_required_int(data, "soft_water_tokens"),
        hard_water_tokens=_required_int(data, "hard_water_tokens"),
    )


def _required_string(data: JsonObject, key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"{key} must be a string")
    return value


def _required_int(data: JsonObject, key: str) -> int:
    value = data[key]
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{key} must be an integer")
    return value


def _optional_int(data: JsonObject, key: str) -> int | None:
    if key not in data or data[key] is None:
        return None
    return _required_int(data, key)


def _required_bool(data: JsonObject, key: str) -> bool:
    value = data[key]
    if not isinstance(value, bool):
        raise TypeError(f"{key} must be a boolean")
    return value
