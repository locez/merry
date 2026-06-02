from __future__ import annotations

import json
from collections.abc import Mapping

import merry
from merry import _merry
from merry._errors import NativeMerryError, _decode_native_error


def runtime_with_fake_response(final_text: str) -> merry.Runtime:
    runtime = merry.Runtime.__new__(merry.Runtime)
    runtime._tools = {}
    try:
        runtime._native = _merry.Runtime._with_fake_response(final_text)
    except NativeMerryError as error:
        raise _decode_native_error(error) from error
    return runtime


def runtime_with_scripted_tool_call(
    *,
    tool_name: str,
    arguments: Mapping[str, object],
    final_text: str,
) -> merry.Runtime:
    runtime = merry.Runtime.__new__(merry.Runtime)
    runtime._tools = {}
    try:
        runtime._native = _merry.Runtime._with_scripted_tool_call(
            tool_name,
            json.dumps(arguments, sort_keys=True),
            final_text,
        )
    except NativeMerryError as error:
        raise _decode_native_error(error) from error
    return runtime


def register_static_tool_failure(
    runtime: merry.Runtime,
    *,
    name: str,
    description: str,
    diagnostic_code: str,
    message: str,
    content: Mapping[str, object],
) -> None:
    try:
        runtime._native._register_static_tool_failure(
            name,
            description,
            diagnostic_code,
            message,
            json.dumps(content, sort_keys=True),
        )
    except NativeMerryError as error:
        raise _decode_native_error(error) from error


def register_static_tool_exception(
    runtime: merry.Runtime,
    *,
    name: str,
    description: str,
    message: str,
) -> None:
    try:
        runtime._native._register_static_tool_exception(name, description, message)
    except NativeMerryError as error:
        raise _decode_native_error(error) from error
