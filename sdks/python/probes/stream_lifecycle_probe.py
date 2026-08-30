from __future__ import annotations

from collections.abc import Iterable

import merry


def event_types(
    messages: Iterable[merry.Event | merry.ToolCallBatch],
) -> tuple[str, ...]:
    return tuple(
        message.type.value for message in messages if isinstance(message, merry.Event)
    )
