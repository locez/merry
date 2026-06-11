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
