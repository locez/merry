"""Tests for the shared TaskSpec external adapter boundary."""

from __future__ import annotations

from pathlib import Path

import pytest

from merry_benchmark.task_spec import load_task_spec

FIXTURE = Path(__file__).parents[2] / "crates/merry-eval/tests/fixtures/adapter-task.toml"


def test_shared_task_spec_loads_into_harbor_instruction() -> None:
    task = load_task_spec(FIXTURE)

    assert task.task_id == "adapter-fixture"
    assert task.timeout_seconds == 120
    assert task.setup[0].render() == "cargo fmt --check (timeout: 30s)"
    assert "Task adapter-fixture (v1)" in task.to_harbor_instruction()
    instruction = task.to_harbor_instruction()
    assert (
        "Success criteria: verify src/lib.rs exists; run cargo test "
        "(cwd: fixtures/adapter) (timeout: 90s)"
    ) in instruction


def test_task_spec_adapter_rejects_unknown_top_level_fields(tmp_path: Path) -> None:
    invalid = tmp_path / "invalid.toml"
    invalid.write_text(FIXTURE.read_text(encoding="utf-8") + "unknown = true\n", encoding="utf-8")

    with pytest.raises(ValueError, match="unknown"):
        load_task_spec(invalid)


def test_task_spec_adapter_matches_rust_path_and_timeout_guards(tmp_path: Path) -> None:
    invalid_scope = tmp_path / "invalid-scope.toml"
    invalid_scope.write_text(
        FIXTURE.read_text(encoding="utf-8").replace(
            "write_scope = [\"src/**\"]", "write_scope = [\"../src\"]"
        ),
        encoding="utf-8",
    )
    with pytest.raises(ValueError, match="workspace"):
        load_task_spec(invalid_scope)

    invalid_timeout = tmp_path / "invalid-timeout.toml"
    invalid_timeout.write_text(
        FIXTURE.read_text(encoding="utf-8").replace("timeout_seconds = 120", "timeout_seconds = 604801"),
        encoding="utf-8",
    )
    with pytest.raises(ValueError, match="seven days"):
        load_task_spec(invalid_timeout)
