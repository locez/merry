"""Harbor installed-agent adapter for Merry's headless CLI.

The adapter keeps benchmark execution in Harbor. It only installs or locates
the Merry binary in the task environment and forwards the task instruction to
``merry --no-sandbox run --events-jsonl``. Harbor remains responsible for task
provisioning, verifier execution, rewards, and trial artifacts.
"""

from __future__ import annotations

import shlex
from pathlib import Path
from typing import Final, Literal, override

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from pydantic import BaseModel

from merry_benchmark.task_spec import load_task_spec

MERRY_BINARY_PATH_ENV: Final[str] = "MERRY_BINARY_PATH"
MERRY_COMMAND_ENV: Final[str] = "MERRY_COMMAND"
MERRY_CONFIG_PATH_ENV: Final[str] = "MERRY_CONFIG_PATH"
MERRY_API_KEY_FILE_PATH_ENV: Final[str] = "MERRY_API_KEY_FILE_PATH"
MERRY_AGENT_VERSION_ENV: Final[str] = "MERRY_AGENT_VERSION"
MERRY_TASK_SPEC_PATH_ENV: Final[str] = "MERRY_TASK_SPEC_PATH"
DEFAULT_MERRY_COMMAND: Final[str] = "merry"
REMOTE_MERRY_BINARY: Final[str] = "/installed-agent/merry"
REMOTE_CONFIG_HOME: Final[str] = "/installed-agent/config"
REMOTE_CONFIG_PATH: Final[str] = f"{REMOTE_CONFIG_HOME}/merry/config.toml"
REMOTE_API_KEY_PATH: Final[str] = f"{REMOTE_CONFIG_HOME}/merry/secrets/openai.key"


class MerryAgentLoopResult(BaseModel):
    """Terminal JSONL event emitted by Merry's headless run command."""

    type: Literal["agent_loop_result"]
    status: str


def _agent_loop_status(stdout: str | None) -> str | None:
    """Return the terminal Merry status from a JSONL stream, if present."""

    if stdout is None:
        return None
    for line in stdout.splitlines():
        try:
            event = MerryAgentLoopResult.model_validate_json(line)
        except ValueError:
            continue
        if event.type == "agent_loop_result":
            return event.status
    return None


def build_merry_run_command(executable: str, instruction: str) -> str:
    """Build the shell command used for one Harbor agent trial.

    Harbor's environment API accepts a shell command, so both the executable
    and task instruction are quoted at this boundary. Merry's own sandbox is
    disabled because Harbor already owns the container boundary.
    """

    if not executable.strip():
        raise ValueError("Merry executable must not be empty")
    return f"{shlex.quote(executable)} --no-sandbox run --events-jsonl {shlex.quote(instruction)}"


def _single_command_token(value: str, field_name: str) -> str:
    tokens = shlex.split(value)
    if len(tokens) != 1:
        raise ValueError(f"{field_name} must contain exactly one executable token")
    return tokens[0]


class MerryAgent(BaseInstalledAgent):
    """Run the Merry CLI as an installed Harbor agent.

    Set ``MERRY_BINARY_PATH`` to a host-side binary that Harbor should upload
    into each task container, or set ``MERRY_COMMAND`` when the task image
    already contains the exact Merry executable. ``MERRY_CONFIG_PATH`` is an
    optional host-side Merry config file uploaded with private permissions.
    Set ``MERRY_TASK_SPEC_PATH`` to consume the shared TaskSpec TOML and
    translate it into Harbor's instruction boundary for a trial.
    When that config references ``secrets/openai.key``, set
    ``MERRY_API_KEY_FILE_PATH`` to upload the key file separately with private
    permissions.
    """

    SUPPORTS_ATIF = False

    @staticmethod
    def name() -> str:
        """Return Harbor's stable agent name."""

        return "merry"

    @override
    def version(self) -> str | None:
        configured = self._get_env(MERRY_AGENT_VERSION_ENV)
        if configured is not None:
            return configured
        return super().version()

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        """Upload a configured Merry binary or verify one already in the image."""

        binary_source = self._get_env(MERRY_BINARY_PATH_ENV)
        if binary_source is not None:
            source_path = Path(binary_source).expanduser()
            if not source_path.is_file():
                raise FileNotFoundError(f"MERRY_BINARY_PATH does not point to a file: {source_path}")
            await environment.upload_file(source_path, REMOTE_MERRY_BINARY)
            await self.exec_as_root(
                environment,
                command=f"chmod 755 {shlex.quote(REMOTE_MERRY_BINARY)}",
            )
        else:
            executable = self._configured_executable()
            result = await environment.exec(
                command=f"command -v {shlex.quote(executable)}",
                user="root",
                timeout_sec=30,
            )
            if result.return_code != 0:
                raise RuntimeError(
                    "Merry is not available in the task environment. Set "
                    "MERRY_BINARY_PATH to a host binary or install the binary "
                    "and set MERRY_COMMAND."
                )

        await self._install_config(environment)

    @override
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        """Execute one task instruction and record a minimal Harbor context."""

        context.metadata = {"merry_status": "running"}
        task_spec_path = self._get_env(MERRY_TASK_SPEC_PATH_ENV)
        if task_spec_path is not None:
            try:
                source_path = Path(task_spec_path).expanduser()
                if not source_path.is_file():
                    raise FileNotFoundError(
                        f"{MERRY_TASK_SPEC_PATH_ENV} does not point to a file: {source_path}"
                    )
                instruction = load_task_spec(source_path).to_harbor_instruction()
            except (OSError, ValueError):
                context.metadata = {"merry_status": "failed"}
                raise
        command = build_merry_run_command(self._remote_executable(), instruction)
        result = await environment.exec(
            command=f"set -o pipefail; {command}",
            env=self._execution_environment(),
        )
        status = _agent_loop_status(result.stdout)
        if result.return_code != 0 and status is None:
            context.metadata = {"merry_status": "failed"}
            raise self._classify_exec_error(command, result)
        context.metadata = {
            "merry_status": status or "completed",
            "merry_exit_code": result.return_code,
        }

    def _configured_executable(self) -> str:
        configured = self._get_env(MERRY_COMMAND_ENV)
        if configured is None:
            configured = DEFAULT_MERRY_COMMAND
        return _single_command_token(configured, MERRY_COMMAND_ENV)

    def _remote_executable(self) -> str:
        if self._get_env(MERRY_BINARY_PATH_ENV) is not None:
            return REMOTE_MERRY_BINARY
        return self._configured_executable()

    async def _install_config(self, environment: BaseEnvironment) -> None:
        config_source = self._get_env(MERRY_CONFIG_PATH_ENV)
        api_key_source = self._get_env(MERRY_API_KEY_FILE_PATH_ENV)
        if config_source is None:
            if api_key_source is not None:
                raise ValueError(f"{MERRY_API_KEY_FILE_PATH_ENV} requires {MERRY_CONFIG_PATH_ENV}")
            return

        source_path = Path(config_source).expanduser()
        if not source_path.is_file():
            raise FileNotFoundError(f"MERRY_CONFIG_PATH does not point to a file: {source_path}")

        api_key_path: Path | None = None
        if api_key_source is not None:
            api_key_path = Path(api_key_source).expanduser()
            if not api_key_path.is_file():
                raise FileNotFoundError(f"MERRY_API_KEY_FILE_PATH does not point to a file: {api_key_path}")

        remote_directories = [Path(REMOTE_CONFIG_PATH).parent]
        if api_key_path is not None:
            remote_directories.append(Path(REMOTE_API_KEY_PATH).parent)
        quoted_directories = " ".join(shlex.quote(str(path)) for path in remote_directories)
        await self.exec_as_root(environment, command=f"mkdir -p {quoted_directories}")
        await environment.upload_file(source_path, REMOTE_CONFIG_PATH)

        remote_private_paths = [REMOTE_CONFIG_PATH]
        if api_key_path is not None:
            await environment.upload_file(api_key_path, REMOTE_API_KEY_PATH)
            remote_private_paths.append(REMOTE_API_KEY_PATH)

        permission_commands = [f"chmod 600 {shlex.quote(path)}" for path in remote_private_paths]
        if environment.default_user is not None:
            owner = shlex.quote(str(environment.default_user))
            permission_commands.insert(
                0,
                f"chown {owner} {' '.join(shlex.quote(path) for path in remote_private_paths)}",
            )
        await self.exec_as_root(
            environment,
            command=" && ".join(permission_commands),
        )

    def _execution_environment(self) -> dict[str, str]:
        if self._get_env(MERRY_CONFIG_PATH_ENV) is None:
            return {}
        return {"XDG_CONFIG_HOME": REMOTE_CONFIG_HOME}
