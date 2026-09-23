"""Small validated primitives shared by event JSON decoders."""

from collections.abc import Mapping
from enum import Enum
from typing import TypeVar

from ._json import JsonValue

EnumT = TypeVar("EnumT", bound=Enum)


def _enum_value(enum_type: type[EnumT], value: object, label: str) -> EnumT:
    if not isinstance(value, str):
        raise TypeError(f"{label} must be a string")
    try:
        return enum_type(value)
    except ValueError as error:
        raise TypeError(f"{label} is unsupported") from error


def _event_text(data: Mapping[str, JsonValue], key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"event field {key!r} must be a string")
    return value


def _required_string(data: Mapping[str, JsonValue], key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"event field {key!r} must be a string")
    if not value.strip():
        raise ValueError(f"event field {key!r} must not be blank")
    return value


def _optional_string(value: object) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise TypeError("optional event field must be a string or null")
    return value


def _required_int(data: Mapping[str, JsonValue], key: str) -> int:
    value = data[key]
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"event field {key!r} must be an integer")
    return value


def _required_strings(value: object, label: str) -> tuple[str, ...]:
    if not isinstance(value, list):
        raise TypeError(f"{label} must be a list")
    values: list[str] = []
    for item in value:
        if not isinstance(item, str):
            raise TypeError(f"{label} must contain only strings")
        values.append(item)
    return tuple(values)
