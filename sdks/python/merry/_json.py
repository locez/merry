"""Bounded JSON values used at the native protocol boundary."""

from __future__ import annotations

import json
import math
import unicodedata
from collections.abc import Collection
from typing import TypeAlias

JsonPrimitive: TypeAlias = None | bool | int | float | str
JsonValue: TypeAlias = JsonPrimitive | list["JsonValue"] | dict[str, "JsonValue"]
JsonObject: TypeAlias = dict[str, JsonValue]


def _contains_control(value: str, *, allow_newline_tab: bool = False) -> bool:
    return any(
        unicodedata.category(character) == "Cc"
        and not (allow_newline_tab and character in {"\n", "\t"})
        for character in value
    )


def require_json_value(value: object, label: str) -> JsonValue:
    """Normalize a decoded or Pydantic value into bounded JSON data."""

    if value is None or isinstance(value, (str, bool, int)):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise TypeError(f"{label} contains a non-finite number")
        return value
    if isinstance(value, list):
        return [require_json_value(item, label) for item in value]
    if isinstance(value, dict):
        normalized: JsonObject = {}
        for key, item in value.items():
            if not isinstance(key, str):
                raise TypeError(f"{label} object keys must be strings")
            normalized[key] = require_json_value(item, label)
        return normalized
    raise TypeError(f"{label} contains a value that is not JSON serializable")


def require_json_object(value: object, label: str) -> JsonObject:
    """Normalize an object-shaped JSON value."""

    normalized = require_json_value(value, label)
    if not isinstance(normalized, dict):
        raise TypeError(f"{label} must be a JSON object")
    return normalized


def validate_object_keys(
    data: JsonObject,
    label: str,
    *,
    required: Collection[str],
    optional: Collection[str] = (),
) -> None:
    """Enforce the known field set of a strict native object contract."""

    required_keys = frozenset(required)
    allowed_keys = required_keys | frozenset(optional)
    missing = sorted(required_keys - data.keys())
    unknown = sorted(data.keys() - allowed_keys)
    if missing:
        raise TypeError(f"{label} is missing required fields: {', '.join(missing)}")
    if unknown:
        raise TypeError(f"{label} contains unsupported fields: {', '.join(unknown)}")


def dump_json(value: JsonValue, label: str) -> str:
    """Serialize bounded JSON without allowing non-standard numeric values."""

    try:
        return json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        )
    except (TypeError, ValueError) as error:
        raise TypeError(f"{label} could not be serialized as JSON") from error
