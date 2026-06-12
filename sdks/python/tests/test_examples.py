from __future__ import annotations

import importlib.util
from pathlib import Path


def load_example(name: str):
    path = Path(__file__).resolve().parents[1] / "examples" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"could not load example module {name}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_single_runtime_cache_probe_extracts_last_cached_input_tokens() -> None:
    module = load_example("single_runtime_cache_probe")

    assert (
        module.cached_input_tokens(
            {
                "total": {"cached_input_tokens": 4096},
                "last": {"cached_input_tokens": 2048},
            }
        )
        == 2048
    )


def test_single_runtime_cache_probe_handles_absent_cached_input_tokens() -> None:
    module = load_example("single_runtime_cache_probe")

    assert module.cached_input_tokens(None) is None
    assert module.cached_input_tokens({"last": {}}) is None


def test_checkpoint_ref_compaction_probe_extracts_tool_observations() -> None:
    module = load_example("checkpoint_ref_compaction_probe")

    started = {
        "type": "tool_call_started",
        "call": {
            "name": "merry_read_checkpoint_ref",
            "arguments": {"ref": "r1"},
        },
    }
    finished = {
        "type": "tool_call_finished",
        "result": {"status": "succeeded"},
        "output": {"kind": "json", "json": '{"ref":"r1","excerpt":"probe"}'},
    }

    assert module.tool_call_name(started) == "merry_read_checkpoint_ref"
    assert module.tool_call_arguments(started) == {"ref": "r1"}
    assert module.tool_result_status(finished) == "succeeded"
    assert module.tool_output_text(finished) == '{"ref":"r1","excerpt":"probe"}'
    assert module.saw_checkpoint_ref_tool([started])


def test_checkpoint_ref_compaction_probe_formats_usage_summary() -> None:
    module = load_example("checkpoint_ref_compaction_probe")

    summary = module.compaction_observation(
        {
            "context": {
                "effective_window_tokens": 60800,
                "source": "fallback",
            },
            "compaction": {
                "auto_compaction_enabled": True,
                "body_budget_tokens": 57401,
                "soft_water_tokens": 40180,
                "hard_water_tokens": 51660,
            },
        }
    )

    assert "auto_compaction_enabled=True" in summary
    assert "effective_window_tokens=60800" in summary
    assert "hard_water_tokens=51660" in summary


def test_checkpoint_ref_compaction_probe_keeps_recall_observational() -> None:
    module = load_example("checkpoint_ref_compaction_probe")

    marker_task = module.diagnostic_record_task()
    filler_task = module.filler_task(1)
    recall_task = module.recall_task()

    assert module.TARGET_VALUE in marker_task
    assert module.TARGET_VALUE not in filler_task
    assert module.TARGET_VALUE not in recall_task
    assert "merry_read_checkpoint_ref" not in recall_task
    assert len(marker_task) < 2_000
    assert len(filler_task) < 60_000


def test_checkpoint_ref_compaction_probe_observes_compaction_completed_event() -> None:
    module = load_example("checkpoint_ref_compaction_probe")
    event = {
        "type": "compaction_completed",
        "checkpoint_id": "checkpoint-session-4",
        "covered_history_item_count": 6,
    }

    assert event["type"] == "compaction_completed"
    assert event.get("checkpoint_id") == "checkpoint-session-4"
    assert event.get("covered_history_item_count") == 6
    assert not module.saw_checkpoint_ref_tool([event])
