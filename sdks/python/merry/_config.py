"""Typed construction configuration for the Python SDK."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from os import PathLike
from pathlib import Path
from typing import Literal, TypeAlias

PathInput: TypeAlias = str | PathLike[str]


def require_positive_int(name: str, value: int) -> None:
    """Validate an integer bound before it crosses the native boundary."""

    if type(value) is not int:
        raise TypeError(f"{name} must be an integer")
    if value < 1:
        raise ValueError(f"{name} must be greater than zero")


def _require_text(name: str, value: str) -> None:
    if type(value) is not str:
        raise TypeError(f"{name} must be a string")
    if not value.strip():
        raise ValueError(f"{name} must not be blank")


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
class WorkspaceLimits:
    """Positive bounds applied by Rust-owned workspace tools."""

    max_read_bytes: int = 1024 * 1024
    max_write_bytes: int = 1024 * 1024
    max_patch_bytes: int = 128 * 1024
    max_list_entries: int = 512
    max_search_matches: int = 100
    max_search_files: int = 1_000
    max_search_entries: int = 10_000
    max_search_bytes: int = 8 * 1024 * 1024
    max_search_line_bytes: int = 8 * 1024
    max_search_query_bytes: int = 1024

    def __post_init__(self) -> None:
        for name, value in (
            ("max_read_bytes", self.max_read_bytes),
            ("max_write_bytes", self.max_write_bytes),
            ("max_patch_bytes", self.max_patch_bytes),
            ("max_list_entries", self.max_list_entries),
            ("max_search_matches", self.max_search_matches),
            ("max_search_files", self.max_search_files),
            ("max_search_entries", self.max_search_entries),
            ("max_search_bytes", self.max_search_bytes),
            ("max_search_line_bytes", self.max_search_line_bytes),
            ("max_search_query_bytes", self.max_search_query_bytes),
        ):
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

    roots: tuple[Path, ...]
    readonly_resource_roots: tuple[Path, ...]
    allow_hidden: bool
    patch: PatchConfig | None
    forbidden_paths: tuple[Path, ...]
    limits: WorkspaceLimits

    def __init__(
        self,
        root: PathInput | None = None,
        *,
        roots: Sequence[PathInput] | None = None,
        readonly_resource_roots: Sequence[PathInput] = (),
        allow_hidden: bool = False,
        patch: PatchConfig | None = None,
        forbidden_paths: Sequence[PathInput] = (),
        limits: WorkspaceLimits | None = None,
    ) -> None:
        if root is not None and roots is not None:
            raise ValueError("WorkspaceConfig accepts root or roots, not both")
        if roots is None:
            if root is None:
                raise ValueError("WorkspaceConfig requires root or roots")
            normalized_roots = (Path(root),)
        else:
            normalized_roots = _paths(roots, "roots")
        if not normalized_roots:
            raise ValueError("WorkspaceConfig.roots must contain at least one path")
        if type(allow_hidden) is not bool:
            raise TypeError("allow_hidden must be a boolean")
        if patch is not None and not isinstance(patch, PatchConfig):
            raise TypeError("patch must be a PatchConfig or None")
        if limits is not None and not isinstance(limits, WorkspaceLimits):
            raise TypeError("limits must be a WorkspaceLimits or None")
        object.__setattr__(self, "roots", normalized_roots)
        object.__setattr__(
            self,
            "readonly_resource_roots",
            _paths(readonly_resource_roots, "readonly_resource_roots"),
        )
        object.__setattr__(self, "allow_hidden", allow_hidden)
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


Provider = OpenAICompatible | Anthropic
