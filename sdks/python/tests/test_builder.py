from __future__ import annotations

import asyncio
from pathlib import Path

import pytest
from pydantic import BaseModel, ConfigDict, Field

import merry


class LookupInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier to look up.")


class LookupOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: str = Field(description="Current order status.")


def openai_provider() -> merry.OpenAICompatible:
    return merry.OpenAICompatible(
        api_key="sk-test",
        model="gpt-test",
        base_url="https://api.example.test/v1",
    )


def test_provider_repr_redacts_credentials() -> None:
    provider = merry.OpenAICompatible(
        api_key="sk-secret-provider-key",
        model="gpt-test",
        base_url="https://api.example.test/v1",
    )

    rendered = repr(provider)

    assert "sk-secret-provider-key" not in rendered
    assert "<redacted>" in rendered
    assert "<configured>" in rendered


def test_builder_tool_decorator_derives_typed_schema_and_builds_agent() -> None:
    builder = merry.AgentBuilder("builder-decorator")

    @builder.tool
    async def lookup_order(args: LookupInput) -> LookupOutput:
        """Look up an order by id."""
        return LookupOutput(status=args.order_id)

    agent = builder.provider(openai_provider()).build()

    assert isinstance(lookup_order, merry.Tool)
    assert lookup_order.name == "lookup_order"
    assert lookup_order.description == "Look up an order by id."
    assert lookup_order.input_model is LookupInput
    assert lookup_order.output_model is LookupOutput
    assert agent.session_id == "builder-decorator"
    assert lookup_order.schema["type"] == "object"
    assert "Stable order identifier" in str(lookup_order.schema)


def test_agent_tool_decorator_can_extend_a_built_but_not_started_agent() -> None:
    agent = merry.AgentBuilder("agent-decorator").provider(openai_provider()).build()

    @agent.tool(name="lookup_order", description="Look up an order by id.")
    async def lookup_order(args: LookupInput) -> LookupOutput:
        return LookupOutput(status=args.order_id)

    assert lookup_order.name == "lookup_order"
    assert agent.session_id == "agent-decorator"


def test_builder_requires_a_primary_provider() -> None:
    with pytest.raises(merry.MerryConfigError) as raised:
        merry.AgentBuilder("missing-provider").build()

    assert raised.value.code == "agent.primary_provider_missing"
    assert raised.value.domain == "config"


def test_builder_rejects_duplicate_tool_names() -> None:
    builder = merry.AgentBuilder("duplicate-tool")

    @builder.tool
    async def first(args: LookupInput) -> LookupOutput:
        """First tool."""
        return LookupOutput(status=args.order_id)

    with pytest.raises(merry.MerryConfigError) as raised:
        builder.register_tool(first)

    assert raised.value.code == "tool.duplicate_registration"


def test_workspace_and_patch_configuration_are_explicit(tmp_path: Path) -> None:
    patch = merry.PatchConfig(
        write_scope=["src"],
        forbidden_paths=["secrets"],
    )
    workspace = merry.WorkspaceConfig(
        root=tmp_path,
        readonly_resource_roots=["reference"],
        allow_hidden=True,
        patch=patch,
        limits=merry.WorkspaceLimits(max_read_bytes=2048),
    )

    agent = (
        merry.AgentBuilder("workspace-config")
        .provider(openai_provider())
        .workspace(workspace)
        .build()
    )

    assert agent.session_id == "workspace-config"
    assert patch.write_scope == (Path("src"),)
    assert workspace.limits.max_read_bytes == 2048


def test_invalid_workspace_and_limits_fail_before_native_build(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="write_scope"):
        merry.PatchConfig(write_scope=[])
    with pytest.raises(ValueError, match="root or roots"):
        merry.WorkspaceConfig()
    with pytest.raises(ValueError, match="root or roots"):
        merry.WorkspaceConfig(root=tmp_path, roots=[tmp_path])

    builder = merry.AgentBuilder("limits")
    with pytest.raises(ValueError, match="max_model_turns"):
        builder.max_model_turns(0)
    with pytest.raises(TypeError, match="max_model_turns"):
        builder.max_model_turns(True)
    with pytest.raises(ValueError, match="event_buffer_size"):
        builder.event_buffer_size(0)
    with pytest.raises(TypeError, match="max_read_bytes"):
        merry.WorkspaceLimits(max_read_bytes=True)


def test_workspace_diagnostic_does_not_expose_host_paths(tmp_path: Path) -> None:
    provider = openai_provider()
    missing_root = tmp_path / "customer-secret-project" / "missing-root"
    builder = merry.AgentBuilder("workspace-diagnostic").provider(provider)

    with pytest.raises(merry.MerryConfigError) as raised:
        builder.workspace(merry.WorkspaceConfig(root=missing_root))

    assert raised.value.info.message == "workspace tool configuration was rejected"
    assert str(missing_root) not in raised.value.info.message


def test_numeric_configuration_rejects_non_integer_values() -> None:
    class NonExactInt(int):
        pass

    invalid_value = NonExactInt(1)

    with pytest.raises(TypeError, match="max_read_bytes"):
        merry.WorkspaceLimits(max_read_bytes=invalid_value)

    builder = merry.AgentBuilder("numeric-types")
    with pytest.raises(TypeError, match="max_model_turns"):
        builder.max_model_turns(invalid_value)
    with pytest.raises(TypeError, match="event_buffer_size"):
        builder.event_buffer_size(invalid_value)


def test_builder_consumption_is_typed_after_native_consuming_failure(
    tmp_path: Path,
) -> None:
    provider = openai_provider()
    workspace_builder = merry.AgentBuilder("workspace-failure").provider(provider)

    with pytest.raises(merry.MerryConfigError):
        workspace_builder.workspace(merry.WorkspaceConfig(root=tmp_path / "missing"))

    workspace_builder.max_model_turns(2)

    builder = merry.AgentBuilder("resume-failure").provider(provider)
    store_root = tmp_path / "sessions"
    state_path = store_root / "resume-failure" / "state.json"
    state_path.parent.mkdir(parents=True)
    state_path.write_text("{}", encoding="utf-8")

    async def resume_invalid_session() -> None:
        await builder.session_store(store_root).resume()

    with pytest.raises(merry.MerryRuntimeError):
        asyncio.run(resume_invalid_session())

    with pytest.raises(merry.MerryConfigError) as raised:
        builder.max_model_turns(2)
    assert raised.value.code == "builder_consumed"
