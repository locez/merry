# Merry Python SDK

The Python package is an async-first ergonomic wrapper over the Rust-owned
Merry facade. Python owns typed conversion and host-side callable execution;
Rust owns sessions, events, tool outputs, ledger state, permissions, retries,
tool admission, and run lifecycle.

## Install

From PyPI (currently published as a source distribution; requires **Rust 1.95+**):

```bash
pip install merry-agent
# or
uv add merry-agent
```

Or build from the repository root:

```bash
cd sdks/python
uv sync
uv build
```

The package exposes the native extension as `merry._merry`. Application code
should use the typed `merry` package and its focused submodules (`merry.config`,
`merry.errors`, `merry.events`, `merry.hooks`, `merry.tools`) instead of
constructing native objects directly.

## Examples

The examples use the live OpenAI-compatible provider configured through
`MERRY_OPENAI_API_KEY`, `MERRY_OPENAI_MODEL`, and the optional
`MERRY_OPENAI_BASE_URL`:

```bash
uv run examples/basic_agent.py                # streaming with async with
uv run examples/tool_decorator.py             # @merry.tool host execution
uv run examples/lifecycle_hooks.py            # context window + lifecycle hooks
uv run examples/structured_output.py          # two-field typed JSON output
uv run examples/interactive_session.py        # long-lived interactive loop
uv run examples/multi_runtime_orchestration.py  # compose independent runtimes
```

## Build An Agent

Construct an `Agent` directly with keyword arguments (or use `Agent.builder()`
for fluent step-by-step configuration). Provider, prompt, generation, and
workspace validation remain owned by the Rust facade:

```python
from pathlib import Path
import merry

agent = merry.Agent(
    session_id="demo",
    provider=merry.OpenAICompatible(
        api_key="...",
        model="gpt-4.1-mini",
        base_url="https://api.example.test/v1",
    ),
    system_prompt=merry.PromptProfile(
        base_instructions="You are a concise engineering assistant.",
        stable_blocks=[
            merry.PromptBlock("style", "Prefer direct answers with exact paths."),
        ],
    ),
    generation=merry.GenerationConfig(
        max_output_tokens=2048,
        parallel_tool_calls=merry.ParallelToolCalls.AUTO,
    ),
    workspace=merry.WorkspaceConfig(
        root=Path("."),
        patch=merry.PatchConfig(write_scope=["src"]),
    ),
    session_store=Path("sessions"),
    max_model_turns=32,
)
```

Passing a plain string to `system_prompt="..."` is shorthand for
`PromptProfile(base_instructions="...")`.

`WorkspaceConfig` maps to the Rust coding profile. `WorkspaceLimits` fields
default to `None`, inheriting Rust defaults; set only the bounds you want to
override.

Anthropic Messages uses the same constructor:

```python
agent = merry.Agent(
    session_id="anthropic-session",
    provider=merry.Anthropic(
        api_key="...",
        model="claude-sonnet-4-5",
        context_window_tokens=200_000,
    ),
)
```

## Context Window And Compaction

The Rust runtime resolves the effective context window in deterministic order:

1. **Session-level explicit override** (`Agent(..., context_window_tokens=...)` or
   `await session.update_settings(context_window_tokens=...)`) -> `"explicit_config"`
2. **Provider capability** (`OpenAICompatible(..., context_window_tokens=...)` or
   `Anthropic(..., context_window_tokens=...)`) -> `"provider_capabilities"`
3. **Bundled catalog** (conservative built-in windows for common official models) -> `"bundled_catalog"`
4. **Fallback** (`272_000` tokens when no metadata is supplied) -> `"fallback"`

You can inspect the active window and source on `result.session_usage.context`
(`resolved_model_window_tokens`, `effective_window_tokens`, `source`), and
configure automatic compaction or structured-output retries on `Agent` /
`AgentBuilder`:

```python
agent = merry.Agent(
    provider=merry.OpenAICompatible(
        api_key="...",
        model="deepseek-chat",
        context_window_tokens=128_000,
    ),
    # Optional session-level override (takes precedence over provider):
    # context_window_tokens=96_000,
    compaction=merry.CompactionConfig(),  # or CompactionConfig.disabled()
    structured_output_retry_policy=merry.StructuredOutputRetryPolicy(max_retries=2),
)
```

## Lifecycle Hooks And Turn Control (`merry.hooks`)

Use `merry.Hooks` to intercept model steps and tool executions with typed
decisions while keeping runtime state and journal settlement in Rust:

- `@hooks.before_model_step` -> `BeforeModelDecision.continue_normally(...)` (with
  optional `generation_override`, `dynamic_steering_note` injected after the
  cached prefix, and `force_compact=True`) or `BeforeModelDecision.stop_turn(...)`
- `@hooks.after_model_step` -> `AfterModelDecision.accept()`,
  `AfterModelDecision.continue_with(text)` (force another model turn even when
  the model did not call tools), or `AfterModelDecision.stop_turn(...)`
- `@hooks.before_tool_call(tools=[...])` -> `BeforeToolDecision.allow()`,
  `.modify_input(arguments)` (re-validated against the tool schema),
  `.deny(message)`, `.short_circuit(output)`, or `.stop_turn(...)`
- `@hooks.after_tool_call(tools=[...])` -> `AfterToolDecision.continue_normally()`,
  `.modify_output(output)`, or `.stop_turn(...)`

Unregistered hook points and non-matching `tools=[...]` filters emit **zero**
hook bridge messages from Rust. Built-in strategy helpers are also exported:
`max_token_budget_hook`, `deny_tools_hook`, and
`require_condition_before_stop_hook`.

```python
hooks = merry.Hooks()
hooks.before_model_step(merry.max_token_budget_hook(100_000))


@hooks.before_tool_call(tools=["lookup_order"])
async def rewrite_order(ctx: merry.BeforeToolCallContext) -> merry.BeforeToolDecision:
    if ctx.arguments.get("order_id") == "blocked":
        return merry.BeforeToolDecision.deny("Order lookup blocked by policy.")
    return merry.BeforeToolDecision.allow()


agent = merry.Agent(
    provider=merry.OpenAICompatible(api_key="...", model="gpt-4.1-mini"),
    tools=[lookup_order],
    hooks=hooks,
)
result = await agent.run("Check order A123.")
if result.status is merry.RunStatus.STOPPED_BY_HOOK and result.hook_stop is not None:
    print(result.hook_stop.hook_point, result.hook_stop.reason)
```

- `agent.stream(..., auto_hooks=True, auto_tools=False)` (default): runs
  registered Python hooks automatically on the caller's asyncio loop and yields
  only `Event | ToolCallBatch`. Pass `auto_tools=True` to also execute host
  tools automatically while streaming events.
- `agent.stream(..., auto_hooks=False)`: yields `HookInvocation` messages
  directly so external workflows or approval UIs can resolve them manually via
  `await invocation.submit(decision)` or `await invocation.fail(message)`.
- Hook exceptions are fail-closed: Python reports the failure to Rust so
  pending tool calls settle cleanly and re-raises the original exception.

## Tools (`@merry.tool`)

Define standalone async tools with `@merry.tool` and pass them via `tools=[...]`
(or register them on `AgentBuilder` with `@builder.tool`).

Two input styles are supported:
- **Multi-parameter function signatures** using `Annotated[..., Field(description=...)]`
- **Single Pydantic `BaseModel` input** with `ConfigDict(extra="forbid")` and
  `Field(description=...)` on every field

Outputs may be any Pydantic `BaseModel` (no input-only schema constraints
required), `str`, `TextContent`, `JsonContent`, or bounded JSON values.
Set `parallel_safe=True` to opt a tool into concurrent wave execution when the
model requests multiple calls in one batch:

```python
from typing import Annotated
from pydantic import BaseModel, Field
import merry


class LookupOutput(BaseModel):
    order_id: str
    status: str


@merry.tool(parallel_safe=True)
async def lookup_order(
    order_id: Annotated[str, Field(description="Stable order identifier.")],
) -> LookupOutput:
    """Look up an order by id."""
    return LookupOutput(order_id=order_id, status="shipped")


agent = merry.Agent(
    session_id="tool-session",
    provider=merry.OpenAICompatible(api_key="...", model="gpt-4.1-mini"),
    tools=[lookup_order],
)
```

Expected business failures should raise `ToolDomainError`; they become failed
tool results so the model can recover. Unexpected handler exceptions are logged
by exception type only (never exposing sensitive messages), mapped to
`MerryToolError`, and cancel the run.

## Streaming And Running

`Agent.run()` executes registered host tools automatically and returns the
durable `RunResult`. `Agent.stream()` returns an async context manager yielding
typed `merry.Event` and `merry.ToolCallBatch` messages:

```python
import merry
from merry import events

async with agent.stream("Inspect the repository and summarize the result.") as run:
    async for message in run:
        if isinstance(message, merry.Event):
            if isinstance(message.payload, events.AssistantMessageDeltaPayload):
                print(message.payload.delta, end="")
        elif isinstance(message, merry.ToolCallBatch):
            await agent.execute_tool_batch(message)
    result = await run.result()

assert result.status is merry.RunStatus.COMPLETED
```

Leaving the `async with` block cancels any unfinished run and awaits durable
Rust shutdown cleanly.

## Interactive Sessions

Use `agent.interactive()` for long-lived multi-turn sessions with `session.send(...)`,
`session.queue` controls (`pause`, `resume`, `flush`, `snapshot`, `update`, `remove`),
mid-turn `session.interrupt()`, hot settings updates (`session.update_settings(...)`),
and manual compaction (`session.compact(...)`):

```python
import merry
from merry import events

async with agent.interactive(auto_tools=True) as session:
    # Apply runtime settings at the next model-request boundary
    await session.update_settings(context_window_tokens=128_000)
    await session.send("Hello!")
    async for message in session:
        if isinstance(message, merry.Event) and isinstance(
            message.payload, events.AssistantMessagePayload
        ):
            print(message.payload.text)
            break

    # Clear explicit override (returns to provider/catalog window) or compact while idle
    await session.update_settings(context_window_tokens=None)
    outcome = await session.compact(focus="Keep key decisions")
```

## Structured Final Output

Pass a strict Pydantic model to `run()` or `stream()` to install the Rust-owned
final output contract:

```python
from pydantic import BaseModel, ConfigDict, Field


class Answer(BaseModel):
    model_config = ConfigDict(extra="forbid")

    summary: str = Field(description="Concise answer summary.")
    next_step: str = Field(description="One practical next step for the reader.")


result = await agent.run(
    "Summarize the repository and give one practical next step.",
    final_output_model=Answer,
)
assert result.structured_output is not None
```

## Save And Resume

Session persistence is file-backed through Rust's `FileSessionStore`. Each
session journal is persisted under `<store_root>/<session_id>/journal.jsonl`,
and exclusive OS-level session locking (`session_lock=True` by default)
prevents concurrent processes from double-writing the same session:

```python
from pathlib import Path
import merry

store_root = Path("sessions")
provider = merry.OpenAICompatible(api_key="...", model="gpt-4.1-mini")

async with merry.Agent(
    session_id="demo",
    provider=provider,
    session_store=store_root,
) as agent:
    await agent.save_session()

async with merry.Agent.resume(
    "demo",
    provider=provider,
    session_store=store_root,
) as resumed:
    result = await resumed.run("Continue from saved state.")
```

`Agent.resume(...)` also remains directly awaitable (`agent = await merry.Agent.resume(...)`)
when managing `await agent.close()` manually. Opening a session already locked by
another handle or process raises `MerrySessionLockedError`
(`code="session_already_reserved"`). Pass `session_lock=False` to opt out.

## Errors And Tests

All native failures map to `MerryError` subclasses (also grouped under
`merry.errors`) carrying `MerryErrorInfo`.

Deterministic tests use a feature-gated Rust fake provider:

```bash
cd sdks/python
maturin develop --uv --features test-utils
uv run --with ruff ruff format --check .
uv run --with ruff ruff check .
uv run --with ty ty check
uv run --with pytest python -m pytest tests -q
uv build
```

See [`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md) for the Rust owner and test
evidence for each public capability.
