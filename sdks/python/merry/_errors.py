from __future__ import annotations

from dataclasses import dataclass, field
import json
from typing import Any

from . import _merry


@dataclass(frozen=True)
class MerryErrorInfo:
    code: str
    domain: str
    message: str
    hint: str | None = None
    retryability: str = "unknown"
    context: dict[str, str] = field(default_factory=dict)


class MerryError(Exception):
    def __init__(self, info: MerryErrorInfo) -> None:
        self.info = info
        super().__init__(info.message)

    @property
    def code(self) -> str:
        return self.info.code

    @property
    def domain(self) -> str:
        return self.info.domain

    @property
    def retryability(self) -> str:
        return self.info.retryability


class MerryConfigError(MerryError):
    pass


class MerryProviderError(MerryError):
    pass


class MerryRuntimeError(MerryError):
    pass


class MerryToolError(MerryError):
    pass


class MerryPolicyError(MerryError):
    pass


class MerryContextError(MerryError):
    pass


class MerryCompactionError(MerryError):
    pass


class MerryInternalError(MerryError):
    pass


class MerryTurnError(MerryError):
    pass


NativeMerryError = _merry.NativeMerryError


_DOMAIN_ERRORS: dict[str, type[MerryError]] = {
    "artifact": MerryRuntimeError,
    "config": MerryConfigError,
    "provider": MerryProviderError,
    "runtime": MerryRuntimeError,
    "tool": MerryToolError,
    "policy": MerryPolicyError,
    "context": MerryContextError,
    "compaction": MerryCompactionError,
    "sandbox": MerryRuntimeError,
}


def _decode_native_error(error: NativeMerryError) -> MerryError:
    payload = error.args[0] if error.args else "{}"
    data = json.loads(payload)

    info = MerryErrorInfo(
        code=str(data["code"]),
        domain=str(data["domain"]),
        message=str(data["message"]),
        hint=_optional_str(data.get("hint")),
        retryability=str(data.get("retryability", "unknown")),
        context=_string_dict(data.get("context", {})),
    )
    error_type = _DOMAIN_ERRORS.get(info.domain, MerryInternalError)
    return error_type(info)


def _optional_str(value: Any) -> str | None:
    if value is None:
        return None
    return str(value)


def _string_dict(value: Any) -> dict[str, str]:
    if not isinstance(value, dict):
        return {}
    return {str(key): str(item) for key, item in value.items()}
