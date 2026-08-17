"""Load the shared TaskSpec manifest at the external benchmark boundary."""

from __future__ import annotations

import tomllib
import unicodedata
from collections.abc import Mapping
from dataclasses import dataclass
from math import isfinite
from pathlib import Path

type TomlValue = str | int | float | bool | list[TomlValue] | dict[str, TomlValue]

MAX_DESCRIPTION_CHARS = 16 * 1024
MAX_DIFF_CHARS = 1024 * 1024
MAX_COMMAND_ARGS = 256
MAX_COMMAND_ARG_CHARS = 16 * 1024
MAX_PATH_CHARS = 512
MAX_SCOPE_CHARS = 512
MAX_TASK_TIMEOUT_SECONDS = 7 * 24 * 60 * 60


def _mapping(value: TomlValue | None, field: str) -> Mapping[str, TomlValue]:
    if not isinstance(value, Mapping):
        raise ValueError(f"{field} must be a TOML table")
    return value


def _list(value: TomlValue | None, field: str) -> list[TomlValue]:
    if not isinstance(value, list):
        raise ValueError(f"{field} must be a TOML array")
    return value


def _strict(mapping: Mapping[str, TomlValue], allowed: set[str], field: str) -> None:
    unknown = set(mapping) - allowed
    if unknown:
        names = ", ".join(sorted(unknown))
        raise ValueError(f"{field} has unknown field(s): {names}")


def _is_control(character: str) -> bool:
    return unicodedata.category(character) == "Cc"


def _text(value: TomlValue | None, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{field} must be non-blank text")
    return value


def _optional_text(value: TomlValue | None, field: str) -> str | None:
    if value is None:
        return None
    return _text(value, field)


def _identifier(value: TomlValue | None, field: str, max_chars: int) -> str:
    text = _text(value, field)
    if text.strip() != text:
        raise ValueError(f"{field} must not have leading or trailing whitespace")
    if len(text) > max_chars:
        raise ValueError(f"{field} must contain at most {max_chars} characters")
    if any(_is_control(char) for char in text):
        raise ValueError(f"{field} must not contain control characters")
    return text


def _protocol_text(value: TomlValue | None, field: str, max_chars: int) -> str:
    text = _text(value, field)
    if len(text) > max_chars:
        raise ValueError(f"{field} must contain at most {max_chars} characters")
    if any(char == "\x00" or (_is_control(char) and char not in {"\n", "\r", "\t"}) for char in text):
        raise ValueError(f"{field} must not contain unsafe control characters")
    return text


def _relative_path(value: TomlValue | None, field: str, max_chars: int = MAX_PATH_CHARS) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{field} must not be empty")
    if value.startswith(("/", "\\")) or "\\" in value:
        raise ValueError(f"{field} must be a relative slash-separated path")
    if len(value) > max_chars:
        raise ValueError(f"{field} must contain at most {max_chars} characters")
    if any(_is_control(char) for char in value):
        raise ValueError(f"{field} must not contain control characters")
    if any(
        not component
        or component in {".", ".."}
        or (len(component) > 1 and component[1] == ":")
        for component in value.split("/")
    ):
        raise ValueError(f"{field} must not escape the task workspace")
    return value


def _optional_relative_path(value: TomlValue | None, field: str) -> str | None:
    if value is None:
        return None
    return _relative_path(value, field)


def _integer(value: TomlValue | None, field: str) -> int:
    if isinstance(value, bool):
        raise ValueError(f"{field} must be an integer")
    if isinstance(value, int):
        return value
    if isinstance(value, float) and isfinite(value) and value.is_integer():
        return int(value)
    raise ValueError(f"{field} must be an integer")


def _positive_integer(value: TomlValue | None, field: str) -> int:
    result = _integer(value, field)
    if result <= 0:
        raise ValueError(f"{field} must be greater than zero")
    return result


def _optional_positive_integer(value: TomlValue | None, field: str) -> int | None:
    if value is None:
        return None
    return _positive_integer(value, field)


def _commands(value: TomlValue | None, field: str) -> tuple[CommandSpecDocument, ...]:
    return tuple(
        CommandSpecDocument.from_mapping(_mapping(item, f"{field}[{index}]"), f"{field}[{index}]")
        for index, item in enumerate(_list(value, field))
    )


@dataclass(frozen=True)
class CommandSpecDocument:
    """A command declaration consumed by the benchmark adapter."""

    program: str
    args: tuple[str, ...]
    working_dir: str | None
    timeout_seconds: int | None

    @classmethod
    def from_mapping(cls, value: Mapping[str, TomlValue], field: str) -> CommandSpecDocument:
        _strict(value, {"program", "args", "working_dir", "timeout_seconds"}, field)
        raw_args = value.get("args", [])
        raw_arg_items = _list(raw_args, f"{field}.args")
        if len(raw_arg_items) > MAX_COMMAND_ARGS:
            raise ValueError(f"{field}.args must contain at most {MAX_COMMAND_ARGS} arguments")
        args = tuple(
            _protocol_text(item, f"{field}.args[{index}]", MAX_COMMAND_ARG_CHARS)
            for index, item in enumerate(raw_arg_items)
        )
        timeout_value = value.get("timeout_seconds")
        timeout = None if timeout_value is None else _positive_integer(timeout_value, f"{field}.timeout_seconds")
        return cls(
            program=_protocol_text(value.get("program"), f"{field}.program", MAX_PATH_CHARS),
            args=args,
            working_dir=_optional_relative_path(value.get("working_dir"), f"{field}.working_dir"),
            timeout_seconds=timeout,
        )

    def render(self) -> str:
        command = " ".join((self.program, *self.args))
        if self.working_dir is not None:
            command = f"{command} (cwd: {self.working_dir})"
        if self.timeout_seconds is not None:
            command = f"{command} (timeout: {self.timeout_seconds}s)"
        return command


@dataclass(frozen=True)
class CriterionDocument:
    """A success criterion represented at the external adapter boundary."""

    kind: str
    path: str | None
    text: str | None
    program: str | None
    args: tuple[str, ...]
    working_dir: str | None
    expected: str | None
    timeout_seconds: int | None

    @classmethod
    def from_mapping(cls, value: Mapping[str, TomlValue], field: str) -> CriterionDocument:
        kind = _text(value.get("kind"), f"{field}.kind")
        allowed_by_kind = {
            "file_exists": {"kind", "path"},
            "file_contains": {"kind", "path", "text"},
            "command_passes": {"kind", "program", "args", "working_dir", "timeout_seconds"},
            "diff_matches": {"kind", "path", "expected"},
        }
        allowed = allowed_by_kind.get(kind)
        if allowed is None:
            raise ValueError(f"{field}.kind is unsupported: {kind}")
        _strict(value, allowed, field)
        command = None
        if kind == "command_passes":
            command_value = {key: item for key, item in value.items() if key != "kind"}
            command = CommandSpecDocument.from_mapping(command_value, field)
        path = (
            _relative_path(value.get("path"), f"{field}.path")
            if kind in {"file_exists", "file_contains", "diff_matches"}
            else None
        )
        text = (
            _protocol_text(value.get("text"), f"{field}.text", MAX_DIFF_CHARS)
            if kind == "file_contains"
            else None
        )
        expected = (
            _protocol_text(value.get("expected"), f"{field}.expected", MAX_DIFF_CHARS)
            if kind == "diff_matches"
            else None
        )
        return cls(
            kind=kind,
            path=path,
            text=text,
            program=None if command is None else command.program,
            args=() if command is None else command.args,
            working_dir=None if command is None else command.working_dir,
            expected=expected,
            timeout_seconds=None if command is None else command.timeout_seconds,
        )

    def render(self) -> str:
        if self.kind == "command_passes":
            command = CommandSpecDocument(
                self.program or "",
                self.args,
                self.working_dir,
                self.timeout_seconds,
            )
            return f"run {command.render()}"
        if self.kind == "file_contains":
            return f"verify {self.path} contains the configured text"
        if self.kind == "diff_matches":
            return f"verify the expected diff in {self.path}"
        return f"verify {self.path} exists"


@dataclass(frozen=True)
class ArtifactDocument:
    """An expected artifact declaration consumed by the adapter."""

    path: str
    kind: str
    sha256: str | None

    @classmethod
    def from_mapping(cls, value: Mapping[str, TomlValue], field: str) -> ArtifactDocument:
        _strict(value, {"path", "kind", "sha256"}, field)
        kind = _text(value.get("kind"), f"{field}.kind")
        if kind not in {"file", "directory", "text", "json", "diff"}:
            raise ValueError(f"{field}.kind is unsupported: {kind}")
        sha256 = _optional_text(value.get("sha256"), f"{field}.sha256")
        if sha256 is not None and (len(sha256) != 64 or any(char not in "0123456789abcdefABCDEF" for char in sha256)):
            raise ValueError(f"{field}.sha256 must be a 64-character hexadecimal digest")
        return cls(
            path=_relative_path(value.get("path"), f"{field}.path"),
            kind=kind,
            sha256=sha256,
        )

    def render(self) -> str:
        digest = "" if self.sha256 is None else f", sha256={self.sha256}"
        return f"{self.path} ({self.kind}{digest})"


@dataclass(frozen=True)
class ResourceLimitsDocument:
    """Resource ceilings carried through the external adapter boundary."""

    max_output_bytes: int | None
    max_file_changes: int | None
    max_processes: int | None

    def render(self) -> str:
        values = (
            ("max_output_bytes", self.max_output_bytes),
            ("max_file_changes", self.max_file_changes),
            ("max_processes", self.max_processes),
        )
        return ", ".join(f"{name}={value}" for name, value in values if value is not None)

    @classmethod
    def from_mapping(cls, value: Mapping[str, TomlValue]) -> ResourceLimitsDocument:
        _strict(value, {"max_output_bytes", "max_file_changes", "max_processes"}, "resource_limits")
        return cls(
            max_output_bytes=_optional_positive_integer(
                value.get("max_output_bytes"), "resource_limits.max_output_bytes"
            ),
            max_file_changes=_optional_positive_integer(
                value.get("max_file_changes"), "resource_limits.max_file_changes"
            ),
            max_processes=_optional_positive_integer(
                value.get("max_processes"), "resource_limits.max_processes"
            ),
        )


@dataclass(frozen=True)
class TaskSpecDocument:
    """The provider-neutral TaskSpec view used by the Harbor adapter."""

    schema_version: int
    task_id: str
    task_version: str
    description: str
    repository_path: str | None
    repository_image: str | None
    repository_commit: str | None
    write_scope: tuple[str, ...]
    setup: tuple[CommandSpecDocument, ...]
    tests: tuple[CommandSpecDocument, ...]
    timeout_seconds: int
    resource_limits: ResourceLimitsDocument
    risk_policy: str
    success_criteria: tuple[CriterionDocument, ...]
    expected_artifacts: tuple[ArtifactDocument, ...]
    expected_diff: str | None

    @classmethod
    def from_mapping(cls, value: Mapping[str, TomlValue]) -> TaskSpecDocument:
        _strict(
            value,
            {
                "schema_version",
                "task_id",
                "task_version",
                "description",
                "repository",
                "write_scope",
                "setup",
                "tests",
                "timeout_seconds",
                "resource_limits",
                "risk_policy",
                "success_criteria",
                "expected_artifacts",
                "expected_diff",
            },
            "task",
        )
        repository = _mapping(value.get("repository"), "repository")
        _strict(repository, {"path", "image", "commit"}, "repository")
        repository_path = _optional_relative_path(repository.get("path"), "repository.path")
        repository_image = (
            None
            if repository.get("image") is None
            else _protocol_text(repository.get("image"), "repository.image", MAX_PATH_CHARS)
        )
        repository_commit = (
            _identifier(repository.get("commit"), "repository.commit", 256)
            if repository.get("commit") is not None
            else None
        )
        if (repository_path is None) == (repository_image is None):
            raise ValueError("repository must contain exactly one of path or image")
        if _integer(value.get("schema_version"), "schema_version") != 1:
            raise ValueError("unsupported schema_version")
        raw_scope = _list(value.get("write_scope"), "write_scope")
        if not raw_scope:
            raise ValueError("write_scope must contain at least one path pattern")
        write_scope = tuple(
            _relative_path(item, f"write_scope[{index}]", MAX_SCOPE_CHARS)
            for index, item in enumerate(raw_scope)
        )
        resource_limits = ResourceLimitsDocument.from_mapping(
            _mapping(value.get("resource_limits", {}), "resource_limits")
        )
        risk_policy_value = _text(value.get("risk_policy", "workspace_write"), "risk_policy")
        if risk_policy_value not in {"read_only", "workspace_write", "workspace_write_and_network"}:
            raise ValueError(f"risk_policy is unsupported: {risk_policy_value}")
        expected_diff = (
            None
            if value.get("expected_diff") is None
            else _protocol_text(value.get("expected_diff"), "expected_diff", MAX_DIFF_CHARS)
        )
        timeout_seconds = _positive_integer(value.get("timeout_seconds"), "timeout_seconds")
        if timeout_seconds > MAX_TASK_TIMEOUT_SECONDS:
            raise ValueError("timeout_seconds must not exceed seven days")
        criteria = tuple(
            CriterionDocument.from_mapping(_mapping(item, f"success_criteria[{index}]"), f"success_criteria[{index}]")
            for index, item in enumerate(_list(value.get("success_criteria"), "success_criteria"))
        )
        if not criteria:
            raise ValueError("success_criteria must not be empty")
        artifacts = tuple(
            ArtifactDocument.from_mapping(
                _mapping(item, f"expected_artifacts[{index}]"),
                f"expected_artifacts[{index}]",
            )
            for index, item in enumerate(_list(value.get("expected_artifacts", []), "expected_artifacts"))
        )
        return cls(
            schema_version=_integer(value.get("schema_version"), "schema_version"),
            task_id=_identifier(value.get("task_id"), "task_id", 128),
            task_version=_identifier(value.get("task_version"), "task_version", 128),
            description=_protocol_text(value.get("description"), "description", MAX_DESCRIPTION_CHARS),
            repository_path=repository_path,
            repository_image=repository_image,
            repository_commit=repository_commit,
            write_scope=write_scope,
            setup=_commands(value.get("setup", []), "setup"),
            tests=_commands(value.get("tests", []), "tests"),
            timeout_seconds=timeout_seconds,
            resource_limits=resource_limits,
            risk_policy=risk_policy_value,
            success_criteria=criteria,
            expected_artifacts=artifacts,
            expected_diff=expected_diff,
        )

    def to_harbor_instruction(self) -> str:
        """Convert the shared protocol document into Harbor's instruction text."""

        repository = self.repository_path or self.repository_image or "configured repository"
        if self.repository_commit is not None:
            repository = f"{repository}@{self.repository_commit}"
        lines = [
            f"Task {self.task_id} ({self.task_version})",
            self.description,
            f"Repository: {repository}",
            f"Writable scope: {', '.join(self.write_scope)}",
            f"Task timeout: {self.timeout_seconds}s",
            f"Risk policy: {self.risk_policy}",
        ]
        if self.setup:
            lines.append("Setup: " + "; ".join(command.render() for command in self.setup))
        if self.tests:
            lines.append("Tests: " + "; ".join(command.render() for command in self.tests))
        limits = self.resource_limits.render()
        if limits:
            lines.append(f"Resource limits: {limits}")
        lines.append("Success criteria: " + "; ".join(criterion.render() for criterion in self.success_criteria))
        if self.expected_artifacts:
            artifacts = ", ".join(artifact.render() for artifact in self.expected_artifacts)
            lines.append(f"Expected artifacts: {artifacts}")
        if self.expected_diff is not None:
            lines.append("Expected diff: configured")
        return "\n".join(lines)


def load_task_spec(path: Path) -> TaskSpecDocument:
    """Load and normalize one TaskSpec TOML document for Harbor."""

    source = path.read_bytes()
    value: TomlValue = tomllib.loads(source.decode("utf-8"))
    return TaskSpecDocument.from_mapping(_mapping(value, "task"))
