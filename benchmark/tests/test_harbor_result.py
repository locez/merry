"""Tests for the CI-facing Harbor result validation."""

from __future__ import annotations

import json
from pathlib import Path

from merry_benchmark.harbor_result import load_result, result_failures


def write_result(
    tmp_path: Path,
    *,
    reward: float | None = 1.0,
    errored_trials: int = 0,
) -> Path:
    """Write the smallest valid Harbor job result for a validation test."""
    reward_stats: dict[str, dict[str, list[str]]] = {}
    n_trials = 0
    if reward is not None:
        reward_stats = {"reward": {str(reward): ["trial-1"]}}
        n_trials = 1

    payload = {
        "id": "00000000-0000-0000-0000-000000000001",
        "started_at": "2026-08-08T00:00:00Z",
        "finished_at": "2026-08-08T00:01:00Z",
        "n_total_trials": 1,
        "stats": {
            "n_completed_trials": 1,
            "n_errored_trials": errored_trials,
            "n_running_trials": 0,
            "n_pending_trials": 0,
            "n_cancelled_trials": 0,
            "evals": {
                "merry__model__dataset": {
                    "n_trials": n_trials,
                    "n_errors": errored_trials,
                    "reward_stats": reward_stats,
                }
            },
        },
    }
    result_path = tmp_path / "result.json"
    result_path.write_text(json.dumps(payload), encoding="utf-8")
    return result_path


def test_successful_result_has_no_failures(tmp_path: Path) -> None:
    result = load_result(write_result(tmp_path))

    assert result_failures(result) == ()


def test_errored_trial_fails_validation(tmp_path: Path) -> None:
    result = load_result(write_result(tmp_path, reward=None, errored_trials=1))

    assert any("errored trial" in failure for failure in result_failures(result))


def test_non_positive_reward_fails_validation(tmp_path: Path) -> None:
    result = load_result(write_result(tmp_path, reward=0.0))

    assert any("non-positive reward" in failure for failure in result_failures(result))


def test_missing_reward_fails_validation(tmp_path: Path) -> None:
    result = load_result(write_result(tmp_path, reward=None))

    assert any("no reward values" in failure for failure in result_failures(result))
