from __future__ import annotations

import argparse
import asyncio
import json
import os
import random
import string
import sys
import time
from typing import Any

import merry
from pydantic import BaseModel, ConfigDict, Field


MODEL = "gpt-4.1-mini"
BASE_URL = None


class StepIn(BaseModel):
    model_config = ConfigDict(extra="forbid")

    index: int = Field(description="One-based sequential step number to request.")
    previous_marker: str = Field(
        description="Marker returned by the previous probe_step call, or START for the first call."
    )


class StepOut(BaseModel):
    model_config = ConfigDict(extra="forbid")

    index: int = Field(description="Step number that was requested.")
    marker: str = Field(description="Marker that the next probe_step call must pass back.")
    payload: str = Field(description="Synthetic text payload returned by the probe step.")
    error: str = Field(description="Empty string on success, otherwise a probe error message.")


class BadPayloadIn(BaseModel):
    model_config = ConfigDict(extra="forbid")

    label: str = Field(description="Label for the bad payload probe.")
    repeat: int = Field(description="Number of bad payload blocks to return.")


class BadPayloadOut(BaseModel):
    model_config = ConfigDict(extra="forbid")

    label: str = Field(description="Label echoed from the bad payload probe request.")
    payload: str = Field(
        description="Payload containing unusual binary/control-character-like text."
    )
    error: str = Field(description="Empty string on success, otherwise a probe error message.")


class ProbeReport(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: str = Field(description="ok when the requested probe completed, otherwise failed.")
    observed_steps: int = Field(description="Number of probe_step calls observed by the model.")
    conclusion: str = Field(description="Short conclusion about whether the stream survived.")


def make_payload(index: int, payload_chars: int) -> str:
    rng = random.Random(index)
    alphabet = string.ascii_letters + string.digits + " .,:;_-/"
    chunk = "".join(rng.choice(alphabet) for _ in range(max(0, payload_chars)))
    return f"STEP={index}\nBEGIN_PAYLOAD\n{chunk}\nEND_PAYLOAD\n"


def make_bad_payload(repeat: int) -> str:
    # JSON can represent these, but they stress SDK/provider sanitation boundaries.
    block = "pdf-ish:%PDF-1.5\x00\x01\x02\x08\x0b\x0c\x1b\x7f\x80\x9f endstream \ufffd\n"
    return block * max(1, min(repeat, 20))


def make_runtime(payload_chars: int) -> tuple[merry.Runtime, dict[str, int]]:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY") or os.environ.get("OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL") or os.environ.get("OPENAI_MODEL") or MODEL
    base_url = os.environ.get("MERRY_OPENAI_BASE_URL") or BASE_URL

    if not api_key:
        raise SystemExit(
            "config.openai_api_key_missing: Set MERRY_OPENAI_API_KEY or OPENAI_API_KEY."
        )

    stats = {"probe_step": 0, "bad_payload": 0}
    runtime = merry.Runtime.with_openai_compatible(
        api_key=api_key,
        model=model,
        base_url=base_url,
    )

    @runtime.tool(description="Return one synthetic step result. Call this sequentially, not in parallel.")
    def probe_step(request: StepIn) -> StepOut:
        stats["probe_step"] += 1
        marker = f"marker-{request.index}-{stats['probe_step']}"
        return StepOut(
            index=request.index,
            marker=marker,
            payload=make_payload(request.index, payload_chars),
            error="",
        )

    @runtime.tool(description="Return a deliberately binary/control-character-like payload.")
    def bad_payload(request: BadPayloadIn) -> BadPayloadOut:
        stats["bad_payload"] += 1
        return BadPayloadOut(
            label=request.label,
            payload=make_bad_payload(request.repeat),
            error="",
        )

    return runtime, stats


def long_chain_prompt(steps: int, payload_chars: int) -> str:
    return f"""
You are testing an agent runtime stream/tool-call implementation.

Goal:
- Execute exactly {steps} sequential calls to probe_step.
- Start with previous_marker="START".
- For call i, use index=i and previous_marker equal to the marker from call i-1.
- Do not skip steps.
- Do not call probe_step in parallel.
- After all tool calls, return a final ProbeReport.

This is intentionally a stress test:
- Each probe_step result contains about {payload_chars} payload characters.
- The important behavior is whether the runtime stream can submit every tool result
  and continue until final output.

Final output rules:
- status="ok" only if all {steps} tool calls completed.
- observed_steps must be the number of probe_step calls you successfully observed.
- conclusion should say whether the stream survived the long tool-result chain.
"""


def control_chars_prompt(repeat: int) -> str:
    return f"""
You are testing whether an agent runtime can safely pass unusual tool results.

Call bad_payload exactly once with label="control-chars" and repeat={repeat}.
Then return a final ProbeReport.

Final output rules:
- status="ok" if you received and handled the bad_payload result.
- observed_steps should be 0 because probe_step is not used in this mode.
- conclusion should mention whether the stream survived the bad payload.
"""


def short_event(event: object) -> str:
    try:
        if not isinstance(event, dict):
            return f"event: <non-dict {type(event).__name__}>"
        kind = event.get("kind", {})
        if not isinstance(kind, dict):
            return f"event: <bad-kind {type(kind).__name__}>"
        typ = kind.get("type")
        call = kind.get("call")
        if isinstance(call, dict):
            name = call.get("name")
            args = call.get("arguments")
            if isinstance(args, dict):
                compact_args = {
                    key: (value[:120] + "...")
                    if isinstance(value, str) and len(value) > 120
                    else value
                    for key, value in args.items()
                }
            else:
                compact_args = args
            return f"event: {typ} tool={name} args={compact_args}"
        diagnostic = kind.get("diagnostic")
        if diagnostic:
            return f"event: {typ} diagnostic={diagnostic}"
        return f"event: {typ}"
    except Exception as exc:
        return f"event: <unprintable {type(exc).__name__}: {exc}>"


async def run_probe(args: argparse.Namespace) -> int:
    runtime, stats = make_runtime(payload_chars=args.payload_chars)
    if args.mode == "long-chain":
        prompt = long_chain_prompt(args.steps, args.payload_chars)
        max_steps = args.max_steps or args.steps + 2
    else:
        prompt = control_chars_prompt(args.repeat)
        max_steps = args.max_steps or 4

    started = time.time()
    print(
        json.dumps(
            {
                "probe": "merry_stream_bridge_stress",
                "mode": args.mode,
                "steps": args.steps,
                "payload_chars": args.payload_chars,
                "repeat": args.repeat,
                "max_steps": max_steps,
                "model": os.environ.get("MERRY_OPENAI_MODEL")
                or os.environ.get("OPENAI_MODEL")
                or MODEL,
                "base_url": os.environ.get("MERRY_OPENAI_BASE_URL") or BASE_URL,
            },
            ensure_ascii=False,
        )
    )

    try:
        stream = runtime.stream(
            prompt,
            final_output_model=ProbeReport,
            max_steps=max_steps,
        )
        async for event in stream:
            print(short_event(event), flush=True)

        result = await stream.result()
        elapsed = time.time() - started
        print(
            json.dumps(
                {"elapsed_seconds": round(elapsed, 3), "tool_stats": stats},
                ensure_ascii=False,
            )
        )

        if result.final_output is None:
            print(
                json.dumps(
                    {"result_status": result.status, "final_output": None},
                    ensure_ascii=False,
                )
            )
            return 2

        report = result.final_output
        if not isinstance(report, ProbeReport):
            print(
                json.dumps(
                    {
                        "result_status": result.status,
                        "final_output_type": type(report).__name__,
                    },
                    ensure_ascii=False,
                )
            )
            return 2

        print("JSON:")
        print(report.model_dump_json(indent=2))
        if args.mode == "long-chain" and stats["probe_step"] < args.steps:
            return 3
        return 0 if report.status == "ok" else 4
    except Exception as exc:
        elapsed = time.time() - started
        print(
            json.dumps(
                {
                    "exception_type": type(exc).__name__,
                    "exception": str(exc),
                    "elapsed_seconds": round(elapsed, 3),
                    "tool_stats": stats,
                },
                ensure_ascii=False,
            ),
            file=sys.stderr,
        )
        return 1


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Probe Merry stream/tool-call failure modes."
    )
    parser.add_argument(
        "--mode",
        choices=["long-chain", "control-chars"],
        default="long-chain",
        help="Failure mode to stress.",
    )
    parser.add_argument(
        "--steps",
        type=int,
        default=18,
        help="Sequential probe_step calls in long-chain mode.",
    )
    parser.add_argument(
        "--payload-chars",
        type=int,
        default=5000,
        help="Payload size per step in long-chain mode.",
    )
    parser.add_argument(
        "--repeat",
        type=int,
        default=8,
        help="Bad payload repeat count in control-chars mode.",
    )
    parser.add_argument(
        "--max-steps",
        type=int,
        default=None,
        help=(
            "Agent loop step budget. Defaults to steps + 2 for long-chain "
            "and 4 for control-chars."
        ),
    )
    args = parser.parse_args(argv)
    if args.steps < 1:
        parser.error("--steps must be >= 1")
    if args.payload_chars < 0:
        parser.error("--payload-chars must be >= 0")
    if args.repeat < 1:
        parser.error("--repeat must be >= 1")
    if args.max_steps is not None and args.max_steps < 1:
        parser.error("--max-steps must be >= 1")
    return args


def main() -> int:
    return asyncio.run(run_probe(parse_args()))


if __name__ == "__main__":
    raise SystemExit(main())
