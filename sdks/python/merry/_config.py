"""Typed construction configuration for the Python SDK."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from enum import StrEnum
from os import PathLike
from pathlib import Path
from typing import Literal, TypeAlias

from ._json import _contains_control

PathInput: TypeAlias = str | PathLike[str]


def require_positive_int(name: str, value: int) -> None:
    """Validate an integer bound before it crosses the native boundary."""

    if type(value) is not int:
        raise TypeError(f"{name} must be an integer")
    if value < 1:
        raise ValueError(f"{name} must be greater than zero")


def _require_text(name: str, value: str, *, allow_newline_tab: bool = False) -> None:
    if type(value) is not str:
        raise TypeError(f"{name} must be a string")
    if not value.strip():
        raise ValueError(f"{name} must not be blank")
    if _contains_control(value, allow_newline_tab=allow_newline_tab):
        raise ValueError(f"{name} must not contain control characters")


def _paths(value: Sequence[PathInput], label: str) -> tuple[Path, ...]:
    if isinstance(value, (str, bytes, PathLike)):
        raise TypeError(f"{label} must be a sequence of paths")
    return tuple(Path(path) for path in value)


@dataclass(frozen=True, slots=True, repr=False)
class OpenAICompatible:
    """OpenAI Responses or Chat Completions compatible provider settings."""

    api_key: str
    model: str
    base_url: str | None = None
    protocol: Literal["responses", "chat_completions"] = "responses"

    def __post_init__(self) -> None:
        _require_text("api_key", self.api_key)
        _require_text("model", self.model)
        if self.base_url is not None:
            _require_text("base_url", self.base_url)
        if self.protocol not in {"responses", "chat_completions"}:
            raise ValueError("protocol must be 'responses' or 'chat_completions'")

    def __repr__(self) -> str:
        base_url = None if self.base_url is None else "<configured>"
        return (
            "OpenAICompatible(api_key='<redacted>', "
            f"model={self.model!r}, base_url={base_url!r}, "
            f"protocol={self.protocol!r})"
        )


@dataclass(frozen=True, slots=True, repr=False)
class Anthropic:
    """Anthropic Messages provider settings."""

    api_key: str
    model: str
    base_url: str | None = None

    def __post_init__(self) -> None:
        _require_text("api_key", self.api_key)
        _require_text("model", self.model)
        if self.base_url is not None:
            _require_text("base_url", self.base_url)

    def __repr__(self) -> str:
        base_url = None if self.base_url is None else "<configured>"
        return (
            "Anthropic(api_key='<redacted>', "
            f"model={self.model!r}, base_url={base_url!r})"
        )


@dataclass(frozen=True, slots=True)
class PromptBlock:
    """One named stable prompt block placed in the cached session prefix."""

    tag: str
    text: str

    def __post_init__(self) -> None:
        _require_text("prompt block tag", self.tag)
        if not all(
            ch.isascii() and (ch.isalnum() or ch in {"_", "-"}) for ch in self.tag
        ):
            raise ValueError(
                "prompt block tag must contain only ASCII letters, digits, '_' or '-'"
            )
        _require_text("prompt block text", self.text, allow_newline_tab=True)


@dataclass(frozen=True, slots=True, init=False)
class PromptProfile:
    """Stable prompt composition applied to the session's cached prefix."""

    base_instructions: str | None
    progress_commentary: bool | None
    progress_commentary_instructions: str | None
    stable_blocks: tuple[PromptBlock, ...]

    def __init__(
        self,
        base_instructions: str | None = None,
        *,
        progress_commentary: bool | None = None,
        progress_commentary_instructions: str | None = None,
        stable_blocks: Sequence[PromptBlock] = (),
    ) -> None:
        if base_instructions is not None:
            _require_text(
                "base_instructions", base_instructions, allow_newline_tab=True
            )
        if progress_commentary is not None and not isinstance(
            progress_commentary, bool
        ):
            raise TypeError("progress_commentary must be a boolean or None")
        if progress_commentary_instructions is not None:
            _require_text(
                "progress_commentary_instructions",
                progress_commentary_instructions,
                allow_newline_tab=True,
            )
        blocks = tuple(stable_blocks)
        seen_tags: set[str] = set()
        for block in blocks:
            if not isinstance(block, PromptBlock):
                raise TypeError("stable_blocks must contain PromptBlock instances")
            if block.tag in seen_tags:
                raise ValueError(f"duplicate prompt block tag: {block.tag!r}")
            seen_tags.add(block.tag)
        object.__setattr__(self, "base_instructions", base_instructions)
        object.__setattr__(self, "progress_commentary", progress_commentary)
        object.__setattr__(
            self,
            "progress_commentary_instructions",
            progress_commentary_instructions,
        )
        object.__setattr__(self, "stable_blocks", blocks)


class ParallelToolCalls(StrEnum):
    """Provider-neutral preference for model-generated parallel tool calls."""

    AUTO = "auto"
    ENABLED = "enabled"
    DISABLED = "disabled"


class ServiceTier(StrEnum):
    """Provider service tier selecting request processing priority and pricing."""

    ULTRAFAST = "ultrafast"
    AUTO = "auto"
    DEFAULT = "default"
    FAST = "fast"
    FLEX = "flex"
    PRIORITY = "priority"
    SCALE = "scale"


@dataclass(frozen=True, slots=True)
class GenerationConfig:
    """Provider-neutral model generation controls."""

    max_output_tokens: int | None = None
    parallel_tool_calls: ParallelToolCalls = ParallelToolCalls.AUTO
    reasoning_effort: str | None = None
    service_tier: ServiceTier | None = None

    def __post_init__(self) -> None:
        if self.max_output_tokens is not None:
            require_positive_int("max_output_tokens", self.max_output_tokens)
        if not isinstance(self.parallel_tool_calls, ParallelToolCalls):
            raise TypeError("parallel_tool_calls must be a ParallelToolCalls value")
        if self.reasoning_effort is not None:
            _require_text("reasoning_effort", self.reasoning_effort)
        if self.service_tier is not None and not isinstance(
            self.service_tier, ServiceTier
        ):
            raise TypeError("service_tier must be a ServiceTier value or None")


@dataclass(frozen=True, slots=True)
class WorkspaceLimits:
    """Positive workspace bound overrides; None inherits the Rust default."""

    max_read_bytes: int | None = None
    max_read_lines: int | None = None
    max_write_bytes: int | None = None
    max_patch_bytes: int | None = None

    def __post_init__(self) -> None:
        for name, value in (
            ("max_read_bytes", self.max_read_bytes),
            ("max_read_lines", self.max_read_lines),
            ("max_write_bytes", self.max_write_bytes),
            ("max_patch_bytes", self.max_patch_bytes),
        ):
            if value is not None:
                require_positive_int(name, value)


@dataclass(frozen=True, slots=True, init=False)
class PatchConfig:
    """Explicit workspace patch scope."""

    write_scope: tuple[Path, ...]
    forbidden_paths: tuple[Path, ...]

    def __init__(
        self,
        write_scope: Sequence[PathInput],
        forbidden_paths: Sequence[PathInput] = (),
    ) -> None:
        normalized_scope = _paths(write_scope, "write_scope")
        if not normalized_scope:
            raise ValueError("PatchConfig.write_scope must contain at least one path")
        object.__setattr__(self, "write_scope", normalized_scope)
        object.__setattr__(
            self,
            "forbidden_paths",
            _paths(forbidden_paths, "forbidden_paths"),
        )


@dataclass(frozen=True, slots=True, init=False)
class WorkspaceConfig:
    """Rust coding-profile workspace configuration."""

    root: Path
    readonly_resource_roots: tuple[Path, ...]
    patch: PatchConfig | None
    forbidden_paths: tuple[Path, ...]
    limits: WorkspaceLimits

    def __init__(
        self,
        root: PathInput,
        *,
        readonly_resource_roots: Sequence[PathInput] = (),
        patch: PatchConfig | None = None,
        forbidden_paths: Sequence[PathInput] = (),
        limits: WorkspaceLimits | None = None,
    ) -> None:
        if isinstance(root, str) and not root.strip():
            raise ValueError("WorkspaceConfig.root must not be blank")
        normalized_root = Path(root)
        if patch is not None and not isinstance(patch, PatchConfig):
            raise TypeError("patch must be a PatchConfig or None")
        if limits is not None and not isinstance(limits, WorkspaceLimits):
            raise TypeError("limits must be a WorkspaceLimits or None")
        object.__setattr__(self, "root", normalized_root)
        object.__setattr__(
            self,
            "readonly_resource_roots",
            _paths(readonly_resource_roots, "readonly_resource_roots"),
        )
        object.__setattr__(self, "patch", patch)
        object.__setattr__(
            self,
            "forbidden_paths",
            _paths(forbidden_paths, "forbidden_paths"),
        )
        object.__setattr__(
            self,
            "limits",
            WorkspaceLimits() if limits is None else limits,
        )


Provider: TypeAlias = OpenAICompatible | Anthropic
