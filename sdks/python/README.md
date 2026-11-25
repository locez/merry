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
`merry.errors`, `merry.events`, `merry.tools`) instead of constructing native
objects directly.

## Examples

The examples use the live OpenAI-compatible provider configured through
`MERRY_OPENAI_API_KEY`, `MERRY_OPENAI_MODEL`, and the optional
`MERRY_OPENAI_BASE_URL`:

```bash
uv run examples/basic_agent.py                # streaming with async with
uv run examples/tool_decorator.py             # @merry.tool host execution
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
    provider=merry.Anthropic(api_key="...", model="claude-sonnet-4-5"),
)
```

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
and mid-turn `session.interrupt()`:

```python
import merry
from merry import events

async with agent.interactive() as session:
    await session.send("Hello!")
    async for message in session:
        if isinstance(message, merry.ToolCallBatch):
            await agent.execute_tool_batch(message)
        elif isinstance(message.payload, events.AssistantMessagePayload):
            print(message.payload.text)
            break
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
session journal is persisted under `<store_root>/<session_id>/journal.jsonl`:

```python
from pathlib import Path
import merry

store_root = Path("sessions")
await agent.save_session()

resumed = await merry.Agent.resume(
    "demo",
    provider=merry.OpenAICompatible(api_key="...", model="gpt-4.1-mini"),
    session_store=store_root,
)
```

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
