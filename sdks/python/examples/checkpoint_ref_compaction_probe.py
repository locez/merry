from __future__ import annotations

import asyncio
import json
import os
from collections.abc import Iterable, Mapping

import merry


SESSION_ID = "checkpoint-ref-compaction-probe"
FILLER_ROUNDS = int(os.environ.get("MERRY_REF_PROBE_FILLER_ROUNDS", "6"))
FILLER_LINES_PER_ROUND = int(os.environ.get("MERRY_REF_PROBE_FILLER_LINES", "220"))
PROBE_MARKER = "MERRY-CHECKPOINT-REF-PROBE-7E3B"
TARGET_FIELD = "delta_nonce"
TARGET_VALUE = "saffron-river-418"


def runtime_from_env_config() -> merry.Runtime:
    api_key = os.environ.get("MERRY_OPENAI_API_KEY") or os.environ.get("OPENAI_API_KEY")
    model = os.environ.get("MERRY_OPENAI_MODEL") or os.environ.get("OPENAI_MODEL")
    base_url = os.environ.get("MERRY_OPENAI_BASE_URL")

    if not api_key:
        raise SystemExit(
            "config.openai_api_key_missing: Set MERRY_OPENAI_API_KEY or OPENAI_API_KEY."
        )
    if not model:
        raise SystemExit(
            "config.openai_model_missing: Set MERRY_OPENAI_MODEL or OPENAI_MODEL."
        )

    return merry.Runtime(
        config=merry.RuntimeConfig(
            provider=merry.OpenAICompatibleProvider(
                api_key=api_key,
                model=model,
                base_url=base_url,
            ),
            session_id=SESSION_ID,
        )
    )


def usage_text(usage: Mapping[str, object] | None) -> str:
    if usage is None:
        return "None"
    return json.dumps(usage, sort_keys=True)


def usage_path(usage: Mapping[str, object] | None, *path: str) -> object | None:
    value: object | None = usage
    for key in path:
        if not isinstance(value, Mapping):
            return None
        value = value.get(key)
    return value


def tool_call_name(event: Mapping[str, object]) -> str | None:
    call = event.get("call")
    if not isinstance(call, Mapping):
        return None
    name = call.get("name")
    return name if isinstance(name, str) else None


def tool_call_arguments(event: Mapping[str, object]) -> Mapping[str, object] | None:
    call = event.get("call")
    if not isinstance(call, Mapping):
        return None
    arguments = call.get("arguments")
    return arguments if isinstance(arguments, Mapping) else None


def tool_result_status(event: Mapping[str, object]) -> str | None:
    result = event.get("result")
    if not isinstance(result, Mapping):
        return None
    status = result.get("status")
    return status if isinstance(status, str) else None


def tool_output_text(event: Mapping[str, object]) -> str | None:
    output = event.get("output")
    if not isinstance(output, Mapping):
        return None
    kind = output.get("kind")
    if kind == "text":
        text = output.get("text")
        return text if isinstance(text, str) else None
    if kind == "json":
        text = output.get("json")
        return text if isinstance(text, str) else None
    return None


def saw_checkpoint_ref_tool(events: Iterable[Mapping[str, object]]) -> bool:
    return any(
        event.get("type") == "tool_call_started"
        and tool_call_name(event) == "merry_read_checkpoint_ref"
        for event in events
    )


def compaction_observation(usage: Mapping[str, object] | None) -> str:
    enabled = usage_path(usage, "compaction", "auto_compaction_enabled")
    hard = usage_path(usage, "compaction", "hard_water_tokens")
    soft = usage_path(usage, "compaction", "soft_water_tokens")
    body = usage_path(usage, "compaction", "body_budget_tokens")
    window = usage_path(usage, "context", "effective_window_tokens")
    source = usage_path(usage, "context", "source")
    return (
        f"auto_compaction_enabled={enabled} "
        f"effective_window_tokens={window} "
        f"context_source={source} "
        f"body_budget_tokens={body} "
        f"soft_water_tokens={soft} "
        f"hard_water_tokens={hard}"
    )


def diagnostic_record_task() -> str:
    lines = [
        "Store this diagnostic record for later continuity.",
        "Do not repeat the record or any field values.",
        "Reply exactly: marker record stored.",
        "",
        "<diagnostic-record>",
        f"marker: {PROBE_MARKER}",
        "alpha_route: basalt-harbor-102",
        "beta_trace: ember-canyon-573",
        "gamma_slot: pine-observatory-219",
        f"{TARGET_FIELD}: {TARGET_VALUE}",
        "epsilon_checksum: slate-mirror-884",
        "zeta_owner: copper-violet-036",
        "eta_hint: the target value is intentionally ordinary and low salience",
        "</diagnostic-record>",
    ]
    return "\n".join(lines)


def filler_task(round_index: int) -> str:
    # Auto-compaction only compresses previous history, so this probe grows
    # history across several successful moderate-size rounds instead of sending
    # one oversized current input.
    lines = [
        f"Append diagnostic filler block {round_index}.",
        "Preserve earlier diagnostic records for continuity.",
        f"Reply exactly: filler block {round_index} stored.",
        "",
        f"<filler-block index=\"{round_index}\">",
    ]
    for line_index in range(FILLER_LINES_PER_ROUND):
        lines.append(
            f"filler {round_index:02d}-{line_index:04d}: "
            "deterministic context ballast for Merry checkpoint compaction. "
            "This line carries no answer to the probe; it only makes older "
            "history eligible for citation-backed summary and ref recovery."
        )
    lines.append("</filler-block>")
    return "\n".join(lines)


def recall_task() -> str:
    return (
        f"What is the exact {TARGET_FIELD} value in the diagnostic record marked "
        f"{PROBE_MARKER}? Answer with only the value if you can determine it."
    )


async def run_round(
    runtime: merry.Runtime,
    label: str,
    task: str,
    *,
    max_model_turns: int | None = None,
) -> merry.RunResult:
    print(f"[{label}] input_chars={len(task)}")
    stream = runtime.stream(task, max_model_turns=max_model_turns)
    async for event in stream:
        event_type = event["type"]
        if event_type == "tool_call_started":
            print(
                f"[{label}] event=tool_call_started "
                f"name={tool_call_name(event)} args={tool_call_arguments(event)}"
            )
        elif event_type == "tool_call_finished":
            output = tool_output_text(event)
            if output is not None and len(output) > 240:
                output = output[:240] + "...<truncated>"
            print(
                f"[{label}] event=tool_call_finished "
                f"status={tool_result_status(event)} output={output}"
            )
        elif event_type == "compaction_completed":
            print(
                f"[{label}] event=compaction_completed "
                f"checkpoint_id={event.get('checkpoint_id')} "
                f"covered_history_item_count={event.get('covered_history_item_count')}"
            )
        else:
            print(f"[{label}] event={event_type}")

    result = await stream.result()
    print(f"[{label}] status={result.status}")
    print(f"[{label}] model_turns_run={result.model_turns_run}")
    print(f"[{label}] final_output={result.final_output!r}")
    print(f"[{label}] usage_summary={compaction_observation(result.session_usage)}")
    print(f"[{label}] result_session_usage={usage_text(result.session_usage)}")
    return result


def require_completed(result: merry.RunResult, label: str) -> None:
    if result.status != "completed":
        raise SystemExit(
            f"{label} round failed before recall; reduce "
            "MERRY_REF_PROBE_FILLER_LINES or use a model/provider with a "
            "larger reported context window."
        )


async def main() -> None:
    runtime = runtime_from_env_config()
    print(f"runtime_session={runtime.session_id}")
    print(
        "note=This probe does not ask the model to use merry_read_checkpoint_ref; "
        "it only observes whether the model naturally calls it after compaction."
    )
    print(
        "note=The probe grows previous history across successful rounds because "
        "the current user input is not part of the compaction input."
    )
    print(
        "note=If compaction does not trigger, increase "
        "MERRY_REF_PROBE_FILLER_ROUNDS or MERRY_REF_PROBE_FILLER_LINES."
    )

    marker_result = await run_round(
        runtime,
        "marker",
        diagnostic_record_task(),
        max_model_turns=1,
    )
    require_completed(marker_result, "marker")
    print(
        "[marker] checkpoint_ref_tool_called="
        f"{saw_checkpoint_ref_tool(marker_result.events)}"
    )

    for round_index in range(1, FILLER_ROUNDS + 1):
        label = f"filler-{round_index:02d}"
        filler_result = await run_round(
            runtime,
            label,
            filler_task(round_index),
            max_model_turns=1,
        )
        require_completed(filler_result, label)
        print(
            f"[{label}] checkpoint_ref_tool_called="
            f"{saw_checkpoint_ref_tool(filler_result.events)}"
        )

    recall_result = await run_round(runtime, "recall", recall_task(), max_model_turns=4)
    called = saw_checkpoint_ref_tool(recall_result.events)
    print(f"[recall] checkpoint_ref_tool_called={called}")
    print(f"[recall] target_field={TARGET_FIELD}")
    print(f"[recall] expected_value={TARGET_VALUE}")
    print(f"runtime_usage={usage_text(await runtime.usage())}")


if __name__ == "__main__":
    asyncio.run(main())
