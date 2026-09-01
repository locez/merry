"""Typed Python errors mapped from Rust-owned Merry diagnostics."""

from __future__ import annotations

import json
from collections.abc import Mapping
from dataclasses import dataclass, field
from types import MappingProxyType
from typing import Literal, TypeAlias

from . import _merry
from ._json import (
    JsonValue,
    _contains_control,
    require_json_object,
    require_json_value,
    validate_object_keys,
)

NativeMerryError = _merry.NativeMerryError

ErrorDomain: TypeAlias = Literal[
    "artifact",
    "compaction",
    "config",
    "context",
    "internal",
    "policy",
    "provider",
    "runtime",
    "sandbox",
    "tool",
]
Retryability: TypeAlias = Literal[
    "cancelled",
    "not_retryable",
    "retryable",
    "unknown",
    "user_action_required",
]

_ERROR_DOMAINS = frozenset(
    {
        "artifact",
        "compaction",
        "config",
        "context",
        "internal",
        "policy",
        "provider",
        "runtime",
        "sandbox",
        "tool",
    }
)
_RETRYABILITIES = frozenset(
    {"cancelled", "not_retryable", "retryable", "unknown", "user_action_required"}
)
_ERROR_CONTEXT_KEYS = frozenset(
    {
        "session_id",
        "turn_id",
        "call_id",
        "tool_name",
        "provider_name",
        "model_role",
        "config_path",
        "field_path",
        "artifact_id",
        "checkpoint_id",
        "http_status",
        "exit_code",
    }
)
_MAX_ERROR_CODE_CHARS = 128
_MAX_ERROR_MESSAGE_CHARS = 4096
_MAX_ERROR_HINT_CHARS = 512
_MAX_ERROR_CONTEXT_CHARS = 512


def _validated_error_domain(value: str) -> ErrorDomain:
    if value == "artifact":
        return "artifact"
    if value == "compaction":
        return "compaction"
    if value == "config":
        return "config"
    if value == "context":
        return "context"
    if value == "internal":
        return "internal"
    if value == "policy":
        return "policy"
    if value == "provider":
        return "provider"
    if value == "runtime":
        return "runtime"
    if value == "sandbox":
        return "sandbox"
    if value == "tool":
        return "tool"
    raise ValueError("error domain is not supported")


def _validated_retryability(value: str) -> Retryability:
    if value == "cancelled":
        return "cancelled"
    if value == "not_retryable":
        return "not_retryable"
    if value == "retryable":
        return "retryable"
    if value == "unknown":
        return "unknown"
    if value == "user_action_required":
        return "user_action_required"
    raise ValueError("error retryability is not supported")


@dataclass(frozen=True, slots=True)
class MerryErrorInfo:
    code: str
    domain: ErrorDomain
    message: str
    hint: str | None = None
    retryability: Retryability = "unknown"
    context: Mapping[str, str] = field(default_factory=dict)

    def __post_init__(self) -> None:
        _validate_text(
            "error code", self.code, _MAX_ERROR_CODE_CHARS, allow_blank=False
        )
        _validate_text(
            "error message", self.message, _MAX_ERROR_MESSAGE_CHARS, allow_blank=False
        )
        if self.domain not in _ERROR_DOMAINS:
            raise ValueError("error domain is not supported")
        if self.retryability not in _RETRYABILITIES:
            raise ValueError("error retryability is not supported")
        if self.hint is not None:
            _validate_text(
                "error hint", self.hint, _MAX_ERROR_HINT_CHARS, allow_blank=False
            )
        if not isinstance(self.context, Mapping):
            raise TypeError("error context must be a string mapping")
        normalized: dict[str, str] = {}
        for key, value in self.context.items():
            if not isinstance(key, str) or key not in _ERROR_CONTEXT_KEYS:
                raise ValueError("error context contains an unsupported key")
            _validate_text(
                "error context value",
                value,
                _MAX_ERROR_CONTEXT_CHARS,
                allow_blank=True,
            )
            normalized[key] = value
        object.__setattr__(self, "context", MappingProxyType(normalized))


class MerryError(Exception):
    """Base class for a stable Rust-originated SDK diagnostic."""

    def __init__(self, info: MerryErrorInfo) -> None:
        self.info = info
        super().__init__(info.message)

    @property
    def code(self) -> str:
        return self.info.code

    @property
    def domain(self) -> ErrorDomain:
        return self.info.domain

    @property
    def retryability(self) -> Retryability:
        return self.info.retryability


class MerryConfigError(MerryError):
    """A configuration or construction contract was rejected."""


class MerryProviderError(MerryError):
    """A model provider configuration or request failed."""


class MerryRuntimeError(MerryError):
    """Rust runtime or run lifecycle failure."""


class MerryToolError(MerryError):
    """Tool declaration, host execution, or result submission failure."""


class MerryPolicyError(MerryError):
    """Runtime policy rejected an operation."""


class MerryContextError(MerryError):
    """Context compilation or budget handling failed."""


class MerryCompactionError(MerryError):
    """Compaction failed."""


class MerryInternalError(MerryError):
    """The native boundary returned an invalid or unknown diagnostic."""


class MerryOutputError(MerryError):
    """A final structured output could not be decoded by its Python model."""


class ToolDomainError(Exception):
    """Expected business failure that should be returned to the model."""

    def __init__(self, info: MerryErrorInfo, content: str | JsonValue) -> None:
        if info.domain != "tool":
            raise ValueError("ToolDomainError diagnostics must use the tool domain")
        self.info = info
        self.content = (
            content
            if isinstance(content, str)
            else require_json_value(content, "tool failure content")
        )
        super().__init__(info.message)


_DOMAIN_ERRORS: Mapping[str, type[MerryError]] = {
    "artifact": MerryRuntimeError,
    "compaction": MerryCompactionError,
    "config": MerryConfigError,
    "context": MerryContextError,
    "internal": MerryInternalError,
    "policy": MerryPolicyError,
    "provider": MerryProviderError,
    "runtime": MerryRuntimeError,
    "sandbox": MerryRuntimeError,
    "tool": MerryToolError,
}


def _info_from_native_payload(payload: object) -> MerryErrorInfo:
    if not isinstance(payload, str):
        return _malformed_native_info("native Merry error payload is not a string")
    try:
        decoded: object = json.loads(payload)
    except json.JSONDecodeError:
        return _malformed_native_info("native Merry error payload is not valid JSON")
    try:
        data = require_json_object(decoded, "native Merry error payload")
        validate_object_keys(
            data,
            "native Merry error payload",
            required={"code", "domain", "message", "hint", "retryability", "context"},
        )
        code = _required_string(data, "code")
        domain = _validated_error_domain(_required_string(data, "domain"))
        message = _required_string(data, "message")
        hint = _optional_string(data, "hint")
        retryability = _validated_retryability(_required_string(data, "retryability"))
        context_value: object = data["context"]
        context_object = require_json_object(
            context_value, "native Merry error context"
        )
        context: dict[str, str] = {}
        for key, value in context_object.items():
            if not isinstance(value, str):
                return _malformed_native_info(
                    "native Merry error context values are not strings"
                )
            context[key] = value
        return MerryErrorInfo(
            code=code,
            domain=domain,
            message=message,
            hint=hint,
            retryability=retryability,
            context=context,
        )
    except (KeyError, TypeError, ValueError) as error:
        return _malformed_native_info(
            f"native Merry error payload is incomplete: {error}"
        )


def _required_string(data: Mapping[str, object], key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"native Merry error field {key!r} is not a string")
    return value


def _optional_string(data: Mapping[str, object], key: str) -> str | None:
    if key not in data:
        return None
    value = data[key]
    if value is None:
        return None
    if not isinstance(value, str):
        raise TypeError(f"native Merry error field {key!r} is not a string or null")
    return value


def _malformed_native_info(message: str) -> MerryErrorInfo:
    return MerryErrorInfo(
        code="protocol.native_error_invalid",
        domain="internal",
        message=message,
        hint="Upgrade or rebuild the Merry native extension.",
        retryability="not_retryable",
    )


def _decode_native_error(error: NativeMerryError) -> MerryError:
    payload: object = error.args[0] if len(error.args) > 0 else None
    info = _info_from_native_payload(payload)
    error_type = _DOMAIN_ERRORS.get(info.domain, MerryInternalError)
    return error_type(info)


def tool_handler_error() -> MerryToolError:
    """Return a public error without exposing arbitrary handler exception text."""

    return MerryToolError(
        MerryErrorInfo(
            code="tool.handler_exception",
            domain="tool",
            message="Python tool handler failed unexpectedly.",
            hint="Handle expected business failures with ToolDomainError.",
            retryability="not_retryable",
        )
    )


def tool_output_error() -> MerryToolError:
    """Return a public error without exposing custom serializer details."""

    return MerryToolError(
        MerryErrorInfo(
            code="tool.output_invalid",
            domain="tool",
            message="Python tool returned a value that could not be serialized.",
            hint="Return an instance of the declared Pydantic output model.",
            retryability="not_retryable",
        )
    )


def output_decode_error() -> MerryOutputError:
    return MerryOutputError(
        MerryErrorInfo(
            code="agent.final_output_decode",
            domain="runtime",
            message="The recorded final output did not match the requested model.",
            hint="Ensure the final output model matches the Rust-owned schema.",
            retryability="not_retryable",
        )
    )


def _validate_text(name: str, value: str, maximum: int, *, allow_blank: bool) -> None:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be a string")
    if not allow_blank and not value.strip():
        raise ValueError(f"{name} must not be blank")
    if len(value) > maximum:
        raise ValueError(f"{name} is too long")
    if _contains_control(value):
        raise ValueError(f"{name} must not contain control characters")


def builder_consumed_error() -> MerryConfigError:
    """Return the stable error for a one-shot builder used after consumption."""

    return MerryConfigError(
        MerryErrorInfo(
            code="builder_consumed",
            domain="config",
            message="AgentBuilder has already been consumed.",
            hint="Create a new builder after a failed consuming operation.",
            retryability="user_action_required",
        )
    )
