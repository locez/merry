"""Typed construction configuration for the Python SDK."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from enum import StrEnum
from os import PathLike
from pathlib import Path
from typing import Literal, TypeAlias

from ._json import _contains_control, _sanitize_freeform_text

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
    if _contains_control(value):
        raise ValueError(f"{name} must not contain control characters")


def _sanitize_required_text(name: str, value: str) -> str:
    if type(value) is not str:
        raise TypeError(f"{name} must be a string")
    cleaned = _sanitize_freeform_text(value)
    if not cleaned.strip():
        raise ValueError(f"{name} must not be blank")
    return cleaned


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
    context_window_tokens: int | None = None

    def __post_init__(self) -> None:
        _require_text("api_key", self.api_key)
        _require_text("model", self.model)
        if self.base_url is not None:
            _require_text("base_url", self.base_url)
        if self.protocol not in {"responses", "chat_completions"}:
            raise ValueError("protocol must be 'responses' or 'chat_completions'")
        if self.context_window_tokens is not None:
            require_positive_int("context_window_tokens", self.context_window_tokens)

    def __repr__(self) -> str:
        base_url = None if self.base_url is None else "<configured>"
        return (
            "OpenAICompatible(api_key='<redacted>', "
            f"model={self.model!r}, base_url={base_url!r}, "
            f"protocol={self.protocol!r}, "
            f"context_window_tokens={self.context_window_tokens!r})"
        )


@dataclass(frozen=True, slots=True, repr=False)
class Anthropic:
    """Anthropic Messages provider settings."""

    api_key: str
    model: str
    base_url: str | None = None
    context_window_tokens: int | None = None

    def __post_init__(self) -> None:
        _require_text("api_key", self.api_key)
        _require_text("model", self.model)
        if self.base_url is not None:
            _require_text("base_url", self.base_url)
        if self.context_window_tokens is not None:
            require_positive_int("context_window_tokens", self.context_window_tokens)

    def __repr__(self) -> str:
        base_url = None if self.base_url is None else "<configured>"
        return (
            "Anthropic(api_key='<redacted>', "
            f"model={self.model!r}, base_url={base_url!r}, "
            f"context_window_tokens={self.context_window_tokens!r})"
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
        object.__setattr__(
            self, "text", _sanitize_required_text("prompt block text", self.text)
        )


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
            base_instructions = _sanitize_required_text(
                "base_instructions", base_instructions
            )
        if progress_commentary is not None and not isinstance(
            progress_commentary, bool
        ):
            raise TypeError("progress_commentary must be a boolean or None")
        if progress_commentary_instructions is not None:
            progress_commentary_instructions = _sanitize_required_text(
                "progress_commentary_instructions",
                progress_commentary_instructions,
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


class HistoryRetentionMode(StrEnum):
    """Policy governing how compacted session history is retained."""

    ARCHIVE = "archive"
    CONTEXT_ONLY = "context_only"


@dataclass(frozen=True, slots=True)
class HistoryRetention:
    """Typed compaction history retention policy orthogonal to session_store."""

    mode: HistoryRetentionMode = HistoryRetentionMode.ARCHIVE
    max_segments: int | None = None
    max_bytes: int | None = None
    export_markdown: bool = True

    def __post_init__(self) -> None:
        if not isinstance(self.mode, HistoryRetentionMode):
            raise TypeError("mode must be a HistoryRetentionMode value")
        if type(self.export_markdown) is not bool:
            raise TypeError("export_markdown must be a boolean")
        if self.max_segments is not None:
            require_positive_int("max_segments", self.max_segments)
        if self.max_bytes is not None:
            require_positive_int("max_bytes", self.max_bytes)
        if self.mode != HistoryRetentionMode.ARCHIVE and (
            self.max_segments is not None
            or self.max_bytes is not None
            or self.export_markdown
        ):
            raise ValueError(
                "max_segments, max_bytes, and export_markdown are only valid for archive retention"
            )

    @classmethod
    def archive(
        cls,
        *,
        max_segments: int | None = None,
        max_bytes: int | None = None,
        export_markdown: bool = True,
    ) -> HistoryRetention:
        """Rotate compacted history into `<session_dir>/archive/` (unbounded by default)."""

        return cls(
            mode=HistoryRetentionMode.ARCHIVE,
            max_segments=max_segments,
            max_bytes=max_bytes,
            export_markdown=export_markdown,
        )

    @classmethod
    def context_only(cls) -> HistoryRetention:
        """Prune compacted history from hot memory and live journal.jsonl."""

        return cls(mode=HistoryRetentionMode.CONTEXT_ONLY, export_markdown=False)


@dataclass(frozen=True, slots=True)
class CompactionConfig:
    """Runtime policy for automatic and manual checkpoint compaction."""

    enabled: bool = True
    reasoning_effort: str | None = None

    def __post_init__(self) -> None:
        if type(self.enabled) is not bool:
            raise TypeError("CompactionConfig.enabled must be a boolean")
        if self.reasoning_effort is not None:
            _require_text("reasoning_effort", self.reasoning_effort)

    @classmethod
    def disabled(cls) -> CompactionConfig:
        """Disable automatic checkpoint compaction."""

        return cls(enabled=False)


@dataclass(frozen=True, slots=True)
class StructuredOutputRetryPolicy:
    """Retry policy for structured final-output decoding failures."""

    max_retries: int = 0

    def __post_init__(self) -> None:
        if type(self.max_retries) is not int:
            raise TypeError("max_retries must be an integer")
        if self.max_retries < 0:
            raise ValueError("max_retries must be non-negative")

    @classmethod
    def disabled(cls) -> StructuredOutputRetryPolicy:
        """Disable structured-output retries after the first attempt."""

        return cls(max_retries=0)


class _UnsetSentinel:
    """Sentinel type distinguishing an omitted optional update from explicit None."""

    __slots__ = ()

    def __repr__(self) -> str:
        return "UNSET"


UNSET = _UnsetSentinel()


@dataclass(frozen=True, slots=True)
class InteractiveSettingsUpdate:
    """Settings applied by an interactive session at the next model-request boundary.

    Fields left as `UNSET` remain unchanged. Setting `context_window_tokens=None`
    explicitly clears an existing session override so resolution falls back to
    provider capabilities or defaults.
    """

    context_window_tokens: int | None | _UnsetSentinel = UNSET
    generation: GenerationConfig | None = None
    compaction: CompactionConfig | None = None
    max_parallel_tool_calls: int | None = None

    def __post_init__(self) -> None:
        if (
            not isinstance(self.context_window_tokens, _UnsetSentinel)
            and self.context_window_tokens is not None
        ):
            require_positive_int("context_window_tokens", self.context_window_tokens)
        if self.generation is not None and not isinstance(
            self.generation, GenerationConfig
        ):
            raise TypeError("generation must be a GenerationConfig or None")
        if self.compaction is not None and not isinstance(
            self.compaction, CompactionConfig
        ):
            raise TypeError("compaction must be a CompactionConfig or None")
        if self.max_parallel_tool_calls is not None:
            require_positive_int(
                "max_parallel_tool_calls", self.max_parallel_tool_calls
            )


@dataclass(frozen=True, slots=True)
class CompactionOutcome:
    """Summary returned by a manual interactive compaction that created a checkpoint."""

    checkpoint_id: str
    covered_model_turn_count: int
    covered_history_item_count: int
    retained_history_item_count: int


Provider: TypeAlias = OpenAICompatible | Anthropic
