from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Annotated

import pytest
from _support import echo_request_agent
from pydantic import BaseModel, ConfigDict, Field

import merry


class LookupInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier to look up.")


class LookupOutput(BaseModel):
    status: str


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


def test_direct_agent_constructor_accepts_pythonic_kwargs(tmp_path: Path) -> None:
    @merry.tool(parallel_safe=True)
    async def lookup_order(
        order_id: Annotated[str, Field(description="Order identifier.")],
    ) -> LookupOutput:
        """Look up an order by id."""
        return LookupOutput(status=order_id)

    agent = merry.Agent(
        session_id="direct-agent",
        provider=openai_provider(),
        system_prompt="You are a concise assistant.",
        generation=merry.GenerationConfig(
            max_output_tokens=512,
            parallel_tool_calls=merry.ParallelToolCalls.ENABLED,
            reasoning_effort="low",
            service_tier=merry.ServiceTier.AUTO,
        ),
        workspace=merry.WorkspaceConfig(root=tmp_path),
        tools=[lookup_order],
        max_model_turns=16,
        event_buffer_size=32,
    )

    assert agent.session_id == "direct-agent"
    assert "lookup_order" in agent.tools
    assert agent.tools.names() == ("lookup_order",)


def test_builder_tool_decorator_derives_typed_schema_and_builds_eagerly() -> None:
    builder = merry.Agent.builder("builder-decorator")

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


def test_prompt_profile_and_generation_config_reach_rust_model_request() -> None:
    prompt = merry.PromptProfile(
        base_instructions="You are Neko, a concise chat assistant.",
        progress_commentary=False,
        stable_blocks=[
            merry.PromptBlock("persona_rules", "Always stay in character."),
        ],
    )
    generation = merry.GenerationConfig(
        max_output_tokens=256,
        parallel_tool_calls=merry.ParallelToolCalls.DISABLED,
        reasoning_effort="medium",
        service_tier=merry.ServiceTier.FLEX,
    )
    agent = echo_request_agent(
        session_id="echo-config",
        prompt=prompt,
        generation=generation,
    )

    result = asyncio.run(agent.run("Hello"))
    assert result.status is merry.RunStatus.COMPLETED
    assert result.final_output is not None
    payload = json.loads(result.final_output)
    stable_prefix = payload["stable_prefix"]
    assert any("You are Neko" in entry for entry in stable_prefix)
    assert any(
        "<persona_rules>" in entry and "Always stay in character." in entry
        for entry in stable_prefix
    )
    assert payload["generation"]["max_output_tokens"] == 256
    assert payload["generation"]["parallel_tool_calls"] == "disabled"
    assert payload["generation"]["reasoning_effort"] == "medium"
    assert payload["generation"]["service_tier"] == "flex"


def test_prompt_profile_and_generation_config_validate_inputs() -> None:
    with pytest.raises(ValueError, match="prompt block tag"):
        merry.PromptBlock("bad tag!", "content")
    with pytest.raises(ValueError, match="duplicate prompt block tag"):
        merry.PromptProfile(
            stable_blocks=[
                merry.PromptBlock("rules", "one"),
                merry.PromptBlock("rules", "two"),
            ]
        )
    with pytest.raises(ValueError, match="max_output_tokens"):
        merry.GenerationConfig(max_output_tokens=0)
    with pytest.raises(TypeError, match="max_output_tokens"):
        merry.GenerationConfig(max_output_tokens=True)


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
        patch=patch,
        limits=merry.WorkspaceLimits(max_read_bytes=2048),
    )

    agent = merry.Agent(
        session_id="workspace-config",
        provider=openai_provider(),
        workspace=workspace,
    )

    assert agent.session_id == "workspace-config"
    assert patch.write_scope == (Path("src"),)
    assert workspace.root == tmp_path
    assert workspace.limits.max_read_bytes == 2048


def test_workspace_limits_defer_defaults_to_rust_and_allow_partial_overrides(
    tmp_path: Path,
) -> None:
    workspace = merry.WorkspaceConfig(root=tmp_path)
    custom = merry.WorkspaceConfig(
        root=tmp_path,
        limits=merry.WorkspaceLimits(max_read_lines=17),
    )

    assert workspace.limits == merry.WorkspaceLimits()
    assert workspace.limits.max_read_lines is None
    assert workspace.limits.max_read_bytes is None
    assert workspace.limits.max_write_bytes is None
    assert workspace.limits.max_patch_bytes is None
    assert custom.limits.max_read_lines == 17
    assert custom.limits.max_read_bytes is None

    for config in (workspace, custom):
        agent = merry.Agent(
            session_id="workspace-defaults",
            provider=openai_provider(),
            workspace=config,
        )
        assert agent.session_id == "workspace-defaults"


def test_invalid_workspace_and_limits_fail_before_native_build(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="write_scope"):
        merry.PatchConfig(write_scope=[])
    with pytest.raises(ValueError, match="root"):
        merry.WorkspaceConfig("")

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
    journal_path = store_root / "resume-failure" / "journal.jsonl"
    journal_path.parent.mkdir(parents=True)
    journal_path.write_text("{invalid", encoding="utf-8")

    async def resume_invalid_session() -> None:
        await builder.session_store(store_root).resume()

    with pytest.raises(merry.MerryRuntimeError):
        asyncio.run(resume_invalid_session())

    with pytest.raises(merry.MerryConfigError) as raised:
        builder.max_model_turns(2)
    assert raised.value.code == "builder_consumed"
