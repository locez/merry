"""Validate the Harbor job result used by the CI smoke evaluation."""

from __future__ import annotations

import math
import sys
from collections.abc import Sequence
from pathlib import Path

from harbor.models.job.result import JobResult


def load_result(path: Path) -> JobResult:
    """Load a Harbor job result from its persisted JSON file."""
    return JobResult.model_validate_json(path.read_text(encoding="utf-8"))


def result_failures(result: JobResult) -> tuple[str, ...]:
    """Return CI-failing conditions found in a Harbor job result."""
    failures: list[str] = []
    stats = result.stats

    if result.n_total_trials <= 0:
        failures.append("Harbor ran no trials")
    if stats.n_completed_trials != result.n_total_trials:
        failures.append(f"Harbor did not complete all trials: {stats.n_completed_trials}/{result.n_total_trials}")
    if stats.n_running_trials:
        failures.append(f"Harbor still has {stats.n_running_trials} running trial(s)")
    if stats.n_pending_trials:
        failures.append(f"Harbor still has {stats.n_pending_trials} pending trial(s)")
    if stats.n_cancelled_trials:
        failures.append(f"Harbor cancelled {stats.n_cancelled_trials} trial(s)")
    if stats.n_errored_trials:
        failures.append(f"Harbor recorded {stats.n_errored_trials} errored trial(s)")
    if not stats.evals:
        failures.append("Harbor produced no evaluation groups")

    for eval_name, eval_stats in stats.evals.items():
        reward_stats = eval_stats.reward_stats.get("reward")
        if reward_stats is None:
            failures.append(f"Harbor evaluation {eval_name!r} has no reward values")
            continue

        reward_count = 0
        for reward, trial_names in reward_stats.items():
            reward_count += len(trial_names)
            if not math.isfinite(float(reward)):
                failures.append(f"Harbor evaluation {eval_name!r} has a non-finite reward")
            elif reward <= 0:
                failures.append(f"Harbor evaluation {eval_name!r} has non-positive reward: {reward}")

        if reward_count != eval_stats.n_trials:
            failures.append(
                f"Harbor evaluation {eval_name!r} has incomplete reward data: "
                f"{reward_count}/{eval_stats.n_trials} trials"
            )

    return tuple(failures)


def main(argv: Sequence[str] | None = None) -> int:
    """Validate one Harbor job result and return a process exit code."""
    arguments = sys.argv if argv is None else argv
    if len(arguments) != 2:
        print(f"usage: {arguments[0]} RESULT_JSON", file=sys.stderr)
        return 2

    result_path = Path(arguments[1])
    try:
        result = load_result(result_path)
    except (OSError, ValueError) as exc:
        print(f"::error::Could not load Harbor result: {exc}", file=sys.stderr)
        return 1

    print(
        "Harbor result: "
        f"{result.stats.n_completed_trials}/{result.n_total_trials} completed, "
        f"{result.stats.n_errored_trials} errored"
    )
    failures = result_failures(result)
    for failure in failures:
        print(f"::error::{failure}", file=sys.stderr)

    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
