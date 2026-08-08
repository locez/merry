"""Behavioral checks for the Harbor-facing Merry agent contract."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest
from harbor.environments.base import BaseEnvironment, ExecResult
from harbor.models.agent.context import AgentContext

from merry_benchmark.agents.merry import MerryAgent, build_merry_run_command


class RecordingEnvironment(BaseEnvironment):
    """Small Harbor environment double for adapter boundary tests."""

    def __init__(self, result: ExecResult | None = None) -> None:
        self.default_user = "agent"
        self.commands: list[tuple[str, dict[str, str] | None]] = []
        self.uploads: list[tuple[Path, str]] = []
        self.result = (
            result if result is not None else ExecResult(return_code=0, stdout="merry", stderr="")
        )

    @staticmethod
    def type() -> str:
        return "recording"

    def _validate_definition(self) -> None:
        return None

    async def start(self, force_build: bool) -> None:
        raise NotImplementedError

    async def stop(self, delete: bool) -> None:
        raise NotImplementedError

    async def upload_file(self, source_path: Path | str, target_path: str) -> None:
        self.uploads.append((Path(source_path), target_path))

    async def upload_dir(self, source_dir: Path | str, target_dir: str) -> None:
        raise NotImplementedError

    async def download_file(self, source_path: str, target_path: Path | str) -> None:
        raise NotImplementedError

    async def download_dir(self, source_dir: str, target_dir: Path | str) -> None:
        raise NotImplementedError

    async def exec(
        self,
        command: str,
        cwd: str | None = None,
        env: dict[str, str] | None = None,
        timeout_sec: int | None = None,
        user: str | int | None = None,
    ) -> ExecResult:
        del cwd, timeout_sec, user
        self.commands.append((command, env))
        return self.result


def test_merry_agent_exposes_stable_name_and_configured_version(tmp_path: Path) -> None:
    agent = MerryAgent(
        logs_dir=tmp_path,
        extra_env={"MERRY_AGENT_VERSION": "c290cd0"},
    )

    assert agent.name() == "merry"
    assert agent.version() == "c290cd0"


def test_run_command_quotes_instruction_and_disables_nested_sandbox() -> None:
    command = build_merry_run_command(
        "/installed-agent/merry",
        "Fix `a b.py`; then run tests.",
    )

    assert command == ("/installed-agent/merry --no-sandbox run --events-jsonl 'Fix `a b.py`; then run tests.'")


def test_run_command_rejects_empty_executable() -> None:
    with pytest.raises(ValueError, match="must not be empty"):
        build_merry_run_command("   ", "task")


def test_api_key_file_requires_config(tmp_path: Path) -> None:
    api_key = tmp_path / "openai.key"
    api_key.write_text("secret", encoding="utf-8")
    agent = MerryAgent(
        logs_dir=tmp_path,
        extra_env={"MERRY_API_KEY_FILE_PATH": str(api_key)},
    )

    with pytest.raises(ValueError, match="requires MERRY_CONFIG_PATH"):
        asyncio.run(agent.install(RecordingEnvironment()))


def test_install_and_run_upload_binary_config_and_key(tmp_path: Path) -> None:
    binary = tmp_path / "merry"
    binary.write_bytes(b"binary")
    config = tmp_path / "config.toml"
    config.write_text("[providers]\n", encoding="utf-8")
    api_key = tmp_path / "openai.key"
    api_key.write_text("secret", encoding="utf-8")
    environment = RecordingEnvironment()
    agent = MerryAgent(
        logs_dir=tmp_path,
        extra_env={
            "MERRY_BINARY_PATH": str(binary),
            "MERRY_CONFIG_PATH": str(config),
            "MERRY_API_KEY_FILE_PATH": str(api_key),
        },
    )

    async def exercise() -> AgentContext:
        await agent.install(environment)
        context = AgentContext()
        await agent.run("Fix the task.", environment, context)
        return context

    context = asyncio.run(exercise())

    assert environment.uploads == [
        (binary, "/installed-agent/merry"),
        (config, "/installed-agent/config/merry/config.toml"),
        (api_key, "/installed-agent/config/merry/secrets/openai.key"),
    ]
    assert any("chmod 755 /installed-agent/merry" in command for command, _ in environment.commands)
    assert any(
        "mkdir -p /installed-agent/config/merry /installed-agent/config/merry/secrets" in command
        for command, _ in environment.commands
    )
    key_permission_command = "chmod 600 /installed-agent/config/merry/secrets/openai.key"
    assert any(key_permission_command in command for command, _ in environment.commands)
    assert environment.commands[-1][1] == {"XDG_CONFIG_HOME": "/installed-agent/config"}
    assert context.metadata == {"merry_status": "completed", "merry_exit_code": 0}


def test_nonzero_exit_with_terminal_event_is_a_scored_incomplete_attempt(tmp_path: Path) -> None:
    environment = RecordingEnvironment(
        ExecResult(
            return_code=1,
            stdout='{"type":"agent_loop_result","status":"failed"}\n',
            stderr="",
        )
    )
    agent = MerryAgent(logs_dir=tmp_path)
    context = AgentContext()

    asyncio.run(agent.run("Fix the task.", environment, context))

    assert context.metadata == {"merry_status": "failed", "merry_exit_code": 1}


def test_nonzero_exit_without_terminal_event_is_an_agent_error(tmp_path: Path) -> None:
    environment = RecordingEnvironment(
        ExecResult(return_code=1, stdout="provider failed", stderr="connection refused")
    )
    agent = MerryAgent(logs_dir=tmp_path)

    with pytest.raises(RuntimeError, match=r"Command failed \(exit 1\)"):
        asyncio.run(agent.run("Fix the task.", environment, AgentContext()))
