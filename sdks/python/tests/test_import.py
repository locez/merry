from __future__ import annotations

import merry
from merry import config, errors, events, tools


def test_import_exposes_pythonic_agent_sdk_contract() -> None:
    assert isinstance(merry.__version__, str)
    assert merry.__version__
    assert merry.Agent is not None
    assert merry.AgentBuilder is not None
    assert merry.AgentRun is not None
    assert merry.InteractiveSession is not None
    assert merry.InputQueue is not None
    assert merry.Tool is not None
    assert callable(merry.tool)
    assert merry.ToolCallBatch is not None
    assert merry.PromptProfile is not None
    assert merry.GenerationConfig is not None


def test_import_exposes_public_submodules_and_keeps_top_level_curated() -> None:
    assert config.OpenAICompatible is merry.OpenAICompatible
    assert errors.MerryError is merry.MerryError
    assert events.Event is merry.Event
    assert tools.tool is merry.tool
    assert "AssistantMessagePayload" not in merry.__all__
    assert hasattr(events, "AssistantMessagePayload")
    assert "AnthropicProvider" not in merry.__all__
    assert "OpenAICompatibleProvider" not in merry.__all__
