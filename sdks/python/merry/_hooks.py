"""Typed lifecycle hooks and turn-control strategies for the Merry Python SDK."""

from __future__ import annotations

import inspect
from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass
from enum import StrEnum
from typing import Literal, Protocol, TypeAlias, overload

from ._config import GenerationConfig
from ._event_types import EventDiagnostic
from ._json import (
    JsonObject,
    JsonValue,
    _contains_control,
    _sanitize_freeform_text,
    require_json_object,
)
from ._models import JsonContent, SessionUsage, TextContent, ToolContent

HookToolStatus: TypeAlias = Literal["succeeded", "failed"]


class HookPoint(StrEnum):
    """Lifecycle boundary where a Merry hook can intercept execution."""

    BEFORE_MODEL_STEP = "before_model_step"
    AFTER_MODEL_STEP = "after_model_step"
    BEFORE_TOOL_CALL = "before_tool_call"
    AFTER_TOOL_CALL = "after_tool_call"


def _require_nonblank(name: str, value: str, *, allow_newline_tab: bool = True) -> str:
    if type(value) is not str:
        raise TypeError(f"{name} must be a string")
    if allow_newline_tab:
        cleaned = _sanitize_freeform_text(value)
        if not cleaned.strip():
            raise ValueError(f"{name} must not be blank")
        return cleaned
    if not value.strip():
        raise ValueError(f"{name} must not be blank")
    if _contains_control(value, allow_newline_tab=False):
        raise ValueError(f"{name} must not contain control characters")
    return value


def _sanitize_optional_text(name: str, value: str | None) -> str | None:
    if value is None:
        return None
    if type(value) is not str:
        raise TypeError(f"{name} must be a string or None")
    return _sanitize_freeform_text(value)


def _tool_output_to_wire(output: ToolContent | str) -> JsonObject:
    if isinstance(output, str):
        output = TextContent(output)
    if isinstance(output, TextContent):
        return {"kind": "text", "text": output.text}
    if isinstance(output, JsonContent):
        return {"kind": "json", "json": output.json}
    raise TypeError("tool output must be TextContent, JsonContent, or str")


def _generation_to_wire(config: GenerationConfig) -> JsonObject:
    return {
        "max_output_tokens": config.max_output_tokens,
        "store": False,
        "parallel_tool_calls": config.parallel_tool_calls.value,
        "reasoning_effort": config.reasoning_effort,
        "service_tier": None
        if config.service_tier is None
        else config.service_tier.value,
    }


@dataclass(frozen=True, slots=True)
class BeforeModelStepContext:
    """Read-only context passed to `before_model_step` hooks."""

    session_id: str
    model_turn: int
    max_model_turns: int
    session_usage: SessionUsage | None
    generation: GenerationConfig


@dataclass(frozen=True, slots=True)
class BeforeModelDecision:
    """Decision returned by a `before_model_step` hook."""

    action: Literal["continue", "stop_turn"]
    generation_override: GenerationConfig | None = None
    dynamic_steering_note: str | None = None
    force_compact: bool = False
    reason: str | None = None
    final_output: str | None = None

    def __post_init__(self) -> None:
        if self.action == "continue":
            if self.generation_override is not None and not isinstance(
                self.generation_override, GenerationConfig
            ):
                raise TypeError("generation_override must be GenerationConfig or None")
            if self.dynamic_steering_note is not None:
                object.__setattr__(
                    self,
                    "dynamic_steering_note",
                    _require_nonblank(
                        "dynamic_steering_note", self.dynamic_steering_note
                    ),
                )
            if type(self.force_compact) is not bool:
                raise TypeError("force_compact must be a boolean")
        elif self.action == "stop_turn":
            if self.reason is None:
                raise ValueError("stop_turn requires a non-blank reason")
            object.__setattr__(self, "reason", _require_nonblank("reason", self.reason))
            object.__setattr__(
                self,
                "final_output",
                _sanitize_optional_text("final_output", self.final_output),
            )
        else:
            raise ValueError(f"unsupported BeforeModelDecision action: {self.action!r}")

    @classmethod
    def continue_normally(
        cls,
        *,
        generation_override: GenerationConfig | None = None,
        dynamic_steering_note: str | None = None,
        force_compact: bool = False,
    ) -> BeforeModelDecision:
        """Continue to the model step with optional per-step dynamic overrides."""

        return cls(
            action="continue",
            generation_override=generation_override,
            dynamic_steering_note=dynamic_steering_note,
            force_compact=force_compact,
        )

    @classmethod
    def stop_turn(
        cls,
        reason: str,
        *,
        final_output: str | None = None,
    ) -> BeforeModelDecision:
        """Stop the current turn cleanly without calling the model provider."""

        return cls(action="stop_turn", reason=reason, final_output=final_output)

    def to_wire(self) -> JsonObject:
        if self.action == "continue":
            return {
                "point": HookPoint.BEFORE_MODEL_STEP.value,
                "action": "continue",
                "generation_override": None
                if self.generation_override is None
                else _generation_to_wire(self.generation_override),
                "dynamic_steering_note": self.dynamic_steering_note,
                "force_compact": self.force_compact,
            }
        return {
            "point": HookPoint.BEFORE_MODEL_STEP.value,
            "action": "stop_turn",
            "reason": self.reason,
            "final_output": self.final_output,
        }


@dataclass(frozen=True, slots=True)
class AfterModelStepContext:
    """Read-only context passed to `after_model_step` hooks."""

    session_id: str
    model_turn: int
    max_model_turns: int
    assistant_text: str | None
    pending_tool_names: tuple[str, ...]
    is_final_turn: bool
    session_usage: SessionUsage | None


@dataclass(frozen=True, slots=True)
class AfterModelDecision:
    """Decision returned by an `after_model_step` hook."""

    action: Literal["accept", "continue_with_input", "stop_turn"]
    text: str | None = None
    reason: str | None = None
    final_output: str | None = None

    def __post_init__(self) -> None:
        if self.action == "accept":
            return
        if self.action == "continue_with_input":
            if self.text is None:
                raise ValueError("continue_with_input requires non-blank text")
            object.__setattr__(self, "text", _require_nonblank("text", self.text))
            return
        if self.action == "stop_turn":
            if self.reason is None:
                raise ValueError("stop_turn requires a non-blank reason")
            object.__setattr__(self, "reason", _require_nonblank("reason", self.reason))
            object.__setattr__(
                self,
                "final_output",
                _sanitize_optional_text("final_output", self.final_output),
            )
            return
        raise ValueError(f"unsupported AfterModelDecision action: {self.action!r}")

    @classmethod
    def accept(cls) -> AfterModelDecision:
        """Accept default runtime control flow for this model step."""

        return cls(action="accept")

    @classmethod
    def continue_with(cls, text: str) -> AfterModelDecision:
        """Force another model turn with dynamic feedback text."""

        return cls(action="continue_with_input", text=text)

    @classmethod
    def stop_turn(
        cls,
        reason: str,
        *,
        final_output: str | None = None,
    ) -> AfterModelDecision:
        """Stop the current turn cleanly (settling any pending tool calls)."""

        return cls(action="stop_turn", reason=reason, final_output=final_output)

    def to_wire(self) -> JsonObject:
        if self.action == "accept":
            return {
                "point": HookPoint.AFTER_MODEL_STEP.value,
                "action": "accept",
            }
        if self.action == "continue_with_input":
            return {
                "point": HookPoint.AFTER_MODEL_STEP.value,
                "action": "continue_with_input",
                "text": self.text,
            }
        return {
            "point": HookPoint.AFTER_MODEL_STEP.value,
            "action": "stop_turn",
            "reason": self.reason,
            "final_output": self.final_output,
        }


@dataclass(frozen=True, slots=True)
class BeforeToolCallContext:
    """Read-only context passed to `before_tool_call` hooks."""

    session_id: str
    model_turn: int
    call_id: str
    tool_name: str
    arguments: JsonObject
    is_bridge_tool: bool


@dataclass(frozen=True, slots=True)
class BeforeToolDecision:
    """Decision returned by a `before_tool_call` hook."""

    action: Literal["allow", "modify_input", "deny", "short_circuit", "stop_turn"]
    arguments: JsonObject | None = None
    message: str | None = None
    output: ToolContent | None = None
    reason: str | None = None
    final_output: str | None = None

    def __post_init__(self) -> None:
        if self.action == "allow":
            return
        if self.action == "modify_input":
            if self.arguments is None:
                raise ValueError("modify_input requires a JSON object arguments dict")
            object.__setattr__(
                self,
                "arguments",
                require_json_object(self.arguments, "modified tool arguments"),
            )
            return
        if self.action == "deny":
            if self.message is None:
                raise ValueError("deny requires a non-blank message")
            object.__setattr__(
                self, "message", _require_nonblank("message", self.message)
            )
            return
        if self.action == "short_circuit":
            if not isinstance(self.output, (TextContent, JsonContent)):
                raise TypeError("short_circuit requires TextContent or JsonContent")
            return
        if self.action == "stop_turn":
            if self.reason is None:
                raise ValueError("stop_turn requires a non-blank reason")
            object.__setattr__(self, "reason", _require_nonblank("reason", self.reason))
            object.__setattr__(
                self,
                "final_output",
                _sanitize_optional_text("final_output", self.final_output),
            )
            return
        raise ValueError(f"unsupported BeforeToolDecision action: {self.action!r}")

    @classmethod
    def allow(cls) -> BeforeToolDecision:
        """Allow the tool call to execute unchanged."""

        return cls(action="allow")

    @classmethod
    def modify_input(cls, arguments: JsonObject) -> BeforeToolDecision:
        """Replace the tool call arguments before execution."""

        return cls(action="modify_input", arguments=arguments)

    @classmethod
    def deny(cls, message: str) -> BeforeToolDecision:
        """Deny execution and return `message` to the model as a failed tool result."""

        return cls(action="deny", message=message)

    @classmethod
    def short_circuit(cls, output: ToolContent | str) -> BeforeToolDecision:
        """Skip execution and record `output` as a succeeded tool result."""

        normalized: ToolContent = (
            TextContent(output) if isinstance(output, str) else output
        )
        return cls(action="short_circuit", output=normalized)

    @classmethod
    def stop_turn(
        cls,
        reason: str,
        *,
        final_output: str | None = None,
    ) -> BeforeToolDecision:
        """Skip remaining tools in the wave, settle pending calls, and stop the turn."""

        return cls(action="stop_turn", reason=reason, final_output=final_output)

    def to_wire(self) -> JsonObject:
        if self.action == "allow":
            return {"point": HookPoint.BEFORE_TOOL_CALL.value, "action": "allow"}
        if self.action == "modify_input":
            return {
                "point": HookPoint.BEFORE_TOOL_CALL.value,
                "action": "modify_input",
                "arguments": self.arguments,
            }
        if self.action == "deny":
            return {
                "point": HookPoint.BEFORE_TOOL_CALL.value,
                "action": "deny",
                "message": self.message,
            }
        if self.action == "short_circuit":
            if self.output is None:
                raise ValueError("short_circuit decision is missing output")
            return {
                "point": HookPoint.BEFORE_TOOL_CALL.value,
                "action": "short_circuit",
                "output": _tool_output_to_wire(self.output),
            }
        return {
            "point": HookPoint.BEFORE_TOOL_CALL.value,
            "action": "stop_turn",
            "reason": self.reason,
            "final_output": self.final_output,
        }


@dataclass(frozen=True, slots=True)
class AfterToolCallContext:
    """Read-only context passed to `after_tool_call` hooks."""

    session_id: str
    model_turn: int
    call_id: str
    tool_name: str
    arguments: JsonObject
    status: HookToolStatus
    output: ToolContent
    diagnostic: EventDiagnostic | None
    is_bridge_tool: bool


@dataclass(frozen=True, slots=True)
class AfterToolDecision:
    """Decision returned by an `after_tool_call` hook."""

    action: Literal["continue", "modify_output", "stop_turn"]
    output: ToolContent | None = None
    reason: str | None = None
    final_output: str | None = None

    def __post_init__(self) -> None:
        if self.action == "continue":
            return
        if self.action == "modify_output":
            if not isinstance(self.output, (TextContent, JsonContent)):
                raise TypeError("modify_output requires TextContent or JsonContent")
            return
        if self.action == "stop_turn":
            if self.reason is None:
                raise ValueError("stop_turn requires a non-blank reason")
            object.__setattr__(self, "reason", _require_nonblank("reason", self.reason))
            object.__setattr__(
                self,
                "final_output",
                _sanitize_optional_text("final_output", self.final_output),
            )
            return
        raise ValueError(f"unsupported AfterToolDecision action: {self.action!r}")

    @classmethod
    def continue_normally(cls) -> AfterToolDecision:
        """Keep the tool result unchanged and continue the turn."""

        return cls(action="continue")

    @classmethod
    def modify_output(cls, output: ToolContent | str) -> AfterToolDecision:
        """Replace the tool output before durable commit."""

        normalized: ToolContent = (
            TextContent(output) if isinstance(output, str) else output
        )
        return cls(action="modify_output", output=normalized)

    @classmethod
    def stop_turn(
        cls,
        reason: str,
        *,
        final_output: str | None = None,
    ) -> AfterToolDecision:
        """Commit this tool result, settle remaining calls, and stop the turn."""

        return cls(action="stop_turn", reason=reason, final_output=final_output)

    def to_wire(self) -> JsonObject:
        if self.action == "continue":
            return {"point": HookPoint.AFTER_TOOL_CALL.value, "action": "continue"}
        if self.action == "modify_output":
            if self.output is None:
                raise ValueError("modify_output decision is missing output")
            return {
                "point": HookPoint.AFTER_TOOL_CALL.value,
                "action": "modify_output",
                "output": _tool_output_to_wire(self.output),
            }
        return {
            "point": HookPoint.AFTER_TOOL_CALL.value,
            "action": "stop_turn",
            "reason": self.reason,
            "final_output": self.final_output,
        }


HookContext: TypeAlias = (
    BeforeModelStepContext
    | AfterModelStepContext
    | BeforeToolCallContext
    | AfterToolCallContext
)
HookDecision: TypeAlias = (
    BeforeModelDecision | AfterModelDecision | BeforeToolDecision | AfterToolDecision
)


class _HookSubmitter(Protocol):
    async def _submit_hook_decision(
        self,
        invocation_id: str,
        decision: HookDecision,
    ) -> None: ...

    async def _fail_hook_invocation(
        self,
        invocation_id: str,
        message: str,
    ) -> None: ...


@dataclass(frozen=True, slots=True)
class HookInvocation:
    """Pending host-bridged hook invocation awaiting a decision."""

    id: str
    point: HookPoint
    context: HookContext
    _submitter: _HookSubmitter

    async def submit(self, decision: HookDecision) -> None:
        """Submit a typed decision for this hook invocation."""

        await self._submitter._submit_hook_decision(self.id, decision)

    async def fail(self, message: str) -> None:
        """Report a fail-closed hook error to the Rust runtime."""

        await self._submitter._fail_hook_invocation(self.id, message)


BeforeModelCallback: TypeAlias = Callable[
    [BeforeModelStepContext],
    BeforeModelDecision | Awaitable[BeforeModelDecision],
]
AfterModelCallback: TypeAlias = Callable[
    [AfterModelStepContext],
    AfterModelDecision | Awaitable[AfterModelDecision],
]
BeforeToolCallback: TypeAlias = Callable[
    [BeforeToolCallContext],
    BeforeToolDecision | Awaitable[BeforeToolDecision],
]
AfterToolCallback: TypeAlias = Callable[
    [AfterToolCallContext],
    AfterToolDecision | Awaitable[AfterToolDecision],
]


@dataclass(frozen=True, slots=True)
class _ToolHookEntry:
    tools: frozenset[str] | None
    callback: BeforeToolCallback | AfterToolCallback

    def matches(self, tool_name: str) -> bool:
        return self.tools is None or tool_name in self.tools


class Hooks:
    """Composable Python hook registry with middleware chaining and zero-cost filtering."""

    def __init__(self) -> None:
        self._before_model: list[BeforeModelCallback] = []
        self._after_model: list[AfterModelCallback] = []
        self._before_tool: list[tuple[frozenset[str] | None, BeforeToolCallback]] = []
        self._after_tool: list[tuple[frozenset[str] | None, AfterToolCallback]] = []

    def is_empty(self) -> bool:
        """Whether no hooks have been registered on this container."""

        return (
            not self._before_model
            and not self._after_model
            and not self._before_tool
            and not self._after_tool
        )

    def before_model_step(self, callback: BeforeModelCallback) -> BeforeModelCallback:
        """Register a `before_model_step` hook callback."""

        self._before_model.append(callback)
        return callback

    def after_model_step(self, callback: AfterModelCallback) -> AfterModelCallback:
        """Register an `after_model_step` hook callback."""

        self._after_model.append(callback)
        return callback

    @overload
    def before_tool_call(self, callback: BeforeToolCallback) -> BeforeToolCallback: ...

    @overload
    def before_tool_call(
        self, *, tools: Sequence[str] | None = None
    ) -> Callable[[BeforeToolCallback], BeforeToolCallback]: ...

    def before_tool_call(
        self,
        callback: BeforeToolCallback | None = None,
        *,
        tools: Sequence[str] | None = None,
    ) -> BeforeToolCallback | Callable[[BeforeToolCallback], BeforeToolCallback]:
        """Register a `before_tool_call` hook, optionally scoped to specific tool names."""

        filter_set = _normalize_tool_filter(tools)

        def register(fn: BeforeToolCallback) -> BeforeToolCallback:
            self._before_tool.append((filter_set, fn))
            return fn

        if callback is None:
            return register
        return register(callback)

    @overload
    def after_tool_call(self, callback: AfterToolCallback) -> AfterToolCallback: ...

    @overload
    def after_tool_call(
        self, *, tools: Sequence[str] | None = None
    ) -> Callable[[AfterToolCallback], AfterToolCallback]: ...

    def after_tool_call(
        self,
        callback: AfterToolCallback | None = None,
        *,
        tools: Sequence[str] | None = None,
    ) -> AfterToolCallback | Callable[[AfterToolCallback], AfterToolCallback]:
        """Register an `after_tool_call` hook, optionally scoped to specific tool names."""

        filter_set = _normalize_tool_filter(tools)

        def register(fn: AfterToolCallback) -> AfterToolCallback:
            self._after_tool.append((filter_set, fn))
            return fn

        if callback is None:
            return register
        return register(callback)

    def copy(self) -> Hooks:
        """Return a shallow snapshot copy so later mutations do not affect built agents."""

        cloned = Hooks()
        cloned._before_model = list(self._before_model)
        cloned._after_model = list(self._after_model)
        cloned._before_tool = list(self._before_tool)
        cloned._after_tool = list(self._after_tool)
        return cloned

    def subscriptions_wire(self) -> JsonObject:
        """Serialize only the subscribed hook points and tool matchers for Rust."""

        return {
            "before_model_step": bool(self._before_model),
            "after_model_step": bool(self._after_model),
            "before_tool_call": _tool_filter_to_wire(
                [flt for flt, _ in self._before_tool]
            ),
            "after_tool_call": _tool_filter_to_wire(
                [flt for flt, _ in self._after_tool]
            ),
        }

    async def evaluate(self, invocation: HookInvocation) -> HookDecision:
        """Run all matching callbacks in order and return the merged or short-circuited decision."""

        ctx = invocation.context
        if isinstance(ctx, BeforeModelStepContext):
            return await self._eval_before_model(ctx)
        if isinstance(ctx, AfterModelStepContext):
            return await self._eval_after_model(ctx)
        if isinstance(ctx, BeforeToolCallContext):
            return await self._eval_before_tool(ctx)
        return await self._eval_after_tool(ctx)

    async def _eval_before_model(
        self, ctx: BeforeModelStepContext
    ) -> BeforeModelDecision:
        current_ctx = ctx
        merged_generation: GenerationConfig | None = None
        notes: list[str] = []
        force_compact = False
        for callback in self._before_model:
            decision = await _resolve_before_model(callback(current_ctx))
            if decision.action == "stop_turn":
                return decision
            if decision.generation_override is not None:
                merged_generation = decision.generation_override
                current_ctx = BeforeModelStepContext(
                    session_id=current_ctx.session_id,
                    model_turn=current_ctx.model_turn,
                    max_model_turns=current_ctx.max_model_turns,
                    session_usage=current_ctx.session_usage,
                    generation=merged_generation,
                )
            if decision.dynamic_steering_note is not None:
                notes.append(decision.dynamic_steering_note)
            force_compact = force_compact or decision.force_compact
        return BeforeModelDecision.continue_normally(
            generation_override=merged_generation,
            dynamic_steering_note="\n\n".join(notes) if notes else None,
            force_compact=force_compact,
        )

    async def _eval_after_model(self, ctx: AfterModelStepContext) -> AfterModelDecision:
        for callback in self._after_model:
            decision = await _resolve_after_model(callback(ctx))
            if decision.action != "accept":
                return decision
        return AfterModelDecision.accept()

    async def _eval_before_tool(self, ctx: BeforeToolCallContext) -> BeforeToolDecision:
        current_ctx = ctx
        modified = False
        for tool_filter, callback in self._before_tool:
            if tool_filter is not None and current_ctx.tool_name not in tool_filter:
                continue
            decision = await _resolve_before_tool(callback(current_ctx))
            if decision.action == "allow":
                continue
            if decision.action == "modify_input":
                if decision.arguments is None:
                    raise ValueError("modify_input decision is missing arguments")
                current_ctx = BeforeToolCallContext(
                    session_id=current_ctx.session_id,
                    model_turn=current_ctx.model_turn,
                    call_id=current_ctx.call_id,
                    tool_name=current_ctx.tool_name,
                    arguments=decision.arguments,
                    is_bridge_tool=current_ctx.is_bridge_tool,
                )
                modified = True
                continue
            return decision
        if modified:
            return BeforeToolDecision.modify_input(current_ctx.arguments)
        return BeforeToolDecision.allow()

    async def _eval_after_tool(self, ctx: AfterToolCallContext) -> AfterToolDecision:
        current_ctx = ctx
        modified = False
        for tool_filter, callback in self._after_tool:
            if tool_filter is not None and current_ctx.tool_name not in tool_filter:
                continue
            decision = await _resolve_after_tool(callback(current_ctx))
            if decision.action == "continue":
                continue
            if decision.action == "modify_output":
                if decision.output is None:
                    raise ValueError("modify_output decision is missing output")
                current_ctx = AfterToolCallContext(
                    session_id=current_ctx.session_id,
                    model_turn=current_ctx.model_turn,
                    call_id=current_ctx.call_id,
                    tool_name=current_ctx.tool_name,
                    arguments=current_ctx.arguments,
                    status=current_ctx.status,
                    output=decision.output,
                    diagnostic=current_ctx.diagnostic,
                    is_bridge_tool=current_ctx.is_bridge_tool,
                )
                modified = True
                continue
            return decision
        if modified:
            return AfterToolDecision.modify_output(current_ctx.output)
        return AfterToolDecision.continue_normally()


def _normalize_tool_filter(tools: Sequence[str] | None) -> frozenset[str] | None:
    if tools is None:
        return None
    if isinstance(tools, (str, bytes)):
        raise TypeError("tools must be a sequence of tool names, not a string")
    normalized: set[str] = set()
    for name in tools:
        _require_nonblank("tool name", name, allow_newline_tab=False)
        normalized.add(name)
    return frozenset(normalized)


def _tool_filter_to_wire(filters: Sequence[frozenset[str] | None]) -> JsonObject:
    if not filters:
        return {"mode": "none"}
    merged: set[str] = set()
    for flt in filters:
        if flt is None:
            return {"mode": "all"}
        merged.update(flt)
    if not merged:
        return {"mode": "none"}
    tools_list: list[JsonValue] = [*sorted(merged)]
    return {"mode": "only", "tools": tools_list}


async def _resolve_before_model(
    result: BeforeModelDecision | Awaitable[BeforeModelDecision],
) -> BeforeModelDecision:
    resolved = await result if inspect.isawaitable(result) else result
    if not isinstance(resolved, BeforeModelDecision):
        raise TypeError("before_model_step hook must return BeforeModelDecision")
    return resolved


async def _resolve_after_model(
    result: AfterModelDecision | Awaitable[AfterModelDecision],
) -> AfterModelDecision:
    resolved = await result if inspect.isawaitable(result) else result
    if not isinstance(resolved, AfterModelDecision):
        raise TypeError("after_model_step hook must return AfterModelDecision")
    return resolved


async def _resolve_before_tool(
    result: BeforeToolDecision | Awaitable[BeforeToolDecision],
) -> BeforeToolDecision:
    resolved = await result if inspect.isawaitable(result) else result
    if not isinstance(resolved, BeforeToolDecision):
        raise TypeError("before_tool_call hook must return BeforeToolDecision")
    return resolved


async def _resolve_after_tool(
    result: AfterToolDecision | Awaitable[AfterToolDecision],
) -> AfterToolDecision:
    resolved = await result if inspect.isawaitable(result) else result
    if not isinstance(resolved, AfterToolDecision):
        raise TypeError("after_tool_call hook must return AfterToolDecision")
    return resolved
