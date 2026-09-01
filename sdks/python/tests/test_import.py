from __future__ import annotations

import merry


def test_import_exposes_new_agent_sdk_contract() -> None:
    assert isinstance(merry.__version__, str)
    assert merry.__version__
    assert merry.Agent is not None
    assert merry.AgentBuilder is not None
    assert merry.Tool is not None
    assert merry.ToolCallBatch is not None


def test_import_does_not_expose_global_tool_decorator() -> None:
    assert "tool" not in merry.__all__
