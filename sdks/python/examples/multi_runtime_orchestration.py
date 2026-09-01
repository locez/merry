from __future__ import annotations

import asyncio
import logging
import os
from dataclasses import dataclass

import merry

logger = logging.getLogger(__name__)
RUNTIME_LABELS: tuple[str, ...] = ("architecture", "testing", "documentation")


def provider_from_env() -> merry.OpenAICompatible:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL")
    if api_key is None:
        raise SystemExit("Set MERRY_OPENAI_API_KEY before running this example.")
    if model is None:
        raise SystemExit("Set MERRY_OPENAI_MODEL before running this example.")
    return merry.OpenAICompatible(
        api_key=api_key,
        model=model,
        base_url=os.environ.get("MERRY_OPENAI_BASE_URL"),
    )


@dataclass(frozen=True, slots=True)
class RuntimeReport:
    label: str
    session_id: str
    status: merry.RunStatus
    event_types: tuple[str, ...]
    final_output: str | None


def build_runtime(label: str) -> merry.Agent:
    return (
        merry.AgentBuilder(f"python-runtime-orchestration-{label}")
        .provider(provider_from_env())
        .build()
    )


async def run_runtime(label: str) -> RuntimeReport:
    runtime = build_runtime(label)
    run = runtime.stream(
        f"You are the {label} reviewer. Give two concise observations about "
        "why a Rust-owned agent runtime should expose a typed Python SDK."
    )
    event_types: list[str] = []
    try:
        async for message in run:
            if isinstance(message, merry.Event):
                event_types.append(message.type.value)
                logger.info("[%s] event=%s", label, message.type.value)
            else:
                await run.cancel()
                raise TypeError(
                    f"Runtime {label!r} received an unhandled host tool invocation."
                )
        result = await run.result()
    finally:
        await run.close()
    return RuntimeReport(
        label=label,
        session_id=runtime.session_id,
        status=result.status,
        event_types=tuple(event_types),
        final_output=result.final_output,
    )


async def main() -> None:
    reports = await asyncio.gather(*(run_runtime(label) for label in RUNTIME_LABELS))
    for report in reports:
        logger.info(
            "[%s] session=%s status=%s events=%s output=%s",
            report.label,
            report.session_id,
            report.status.value,
            report.event_types,
            report.final_output,
        )


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
