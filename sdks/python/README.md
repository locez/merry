# Merry Python SDK

The Python package is an async-first ergonomic wrapper over the Rust-owned
Merry facade. Python owns typed conversion and host-side callable execution;
Rust owns sessions, events, artifacts, ledger state, permissions, retries,
tool admission, and run lifecycle.

## Install

From the repository root:

```bash
cd sdks/python
uv sync
uv build
```

The package exposes the native extension as `merry._merry`. Application code
should use the typed `merry` module instead of constructing native objects.

## Examples

The examples use the live OpenAI-compatible provider configured through
`MERRY_OPENAI_API_KEY`, `MERRY_OPENAI_MODEL`, and the optional
`MERRY_OPENAI_BASE_URL`:

```bash
uv run examples/basic_agent.py       # event handling and terminal result
uv run examples/tool_decorator.py    # @builder.tool host execution
uv run examples/structured_output.py # two-field typed JSON output
uv run examples/multi_runtime_orchestration.py  # compose independent runtimes
```

Each agent has its own session id and Rust-owned lifecycle. Interactive and
process-specific examples are intentionally omitted until those adapters are
part of the Python capability matrix.

## Build An Agent

Provider construction is performed by the Rust facade. The Python dataclasses
only carry validated application configuration:

```python
from pathlib import Path

import merry

store_root = Path("sessions")

agent = (
    merry.Agent.builder(session_id="demo")
    .provider(
        merry.OpenAICompatible(
            api_key="...",
            model="gpt-4.1-mini",
            base_url="https://api.example.test/v1",
        )
    )
    .workspace(
        merry.WorkspaceConfig(
            root=Path("."),
            patch=merry.PatchConfig(write_scope=["src"]),
        )
    )
    .session_store(store_root)
    .max_model_turns(32)
    .build()
)
```

`WorkspaceConfig` maps to the Rust coding profile. Patch and forbidden paths
are workspace-relative normalized paths; workspace roots themselves may be
absolute. Every workspace limit is positive and is enforced again by Rust.

Anthropic Messages uses the same builder:

```python
agent = (
    merry.Agent.builder("anthropic-session")
    .provider(merry.Anthropic(api_key="...", model="claude-sonnet-4-5"))
    .build()
)
```

## Tool Decorator

Tools use one Pydantic input model, one Pydantic output model, and an async
handler. Every field must have a description so the generated provider-neutral
schema is explicit:

```python
from pydantic import BaseModel, ConfigDict, Field


class LookupInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier.")


class LookupOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    status: str = Field(description="Current fulfillment status.")


builder = merry.Agent.builder("tool-session")


@builder.tool
async def lookup_order(args: LookupInput) -> LookupOutput:
    """Look up an order by id."""
    return LookupOutput(status="shipped")


agent = builder.provider(
    merry.OpenAICompatible(api_key="...", model="gpt-4.1-mini")
).build()
```

`@agent.tool` is also available after `build()` and before the first run. The
decorator returns a typed `Tool[InputT, OutputT]`, which can be passed to
`ToolRegistry` or called directly in tests. Duplicate names, reserved names,
schema validation, admission, ordering, artifacts, and result persistence
remain Rust responsibilities.

Expected business failures should use `ToolDomainError`; they become a failed
tool result and let the model continue. Unexpected handler exceptions become
`MerryToolError` and cancel the run. Cancellation is propagated to the handler.

## Streaming And Running

`Agent.run()` is the convenient path: it executes registered Python tools and
returns the Rust-owned terminal result. `Agent.stream()` exposes the explicit
message protocol for hosts that need to render events or control tool calls:

```python
import merry

run = agent.stream("Inspect the repository and summarize the result.")
registry = merry.ToolRegistry([lookup_order])

async for message in run:
    if isinstance(message, merry.Event):
        print(message.type.value)
    elif isinstance(message, merry.ToolCallBatch):
        await registry.execute(message)

result = await run.result()
assert result.status is merry.RunStatus.COMPLETED
```

`ToolCallBatch` is an exclusive Rust-owned lease. The complete result set must
be submitted before reading the next message. `result()` is valid after EOF;
`run.cancel()` and `close()` wait for a durable cancelled terminal result. The
Python async task may be cancelled; the SDK requests Rust cancellation and
re-raises `asyncio.CancelledError`.

`AgentBuilder` is single-use for `build()` and `resume()`. A native operation
that has consumed the builder also makes the Python builder terminal, including
when that operation later fails. Start a new builder to retry that operation.

## Structured Final Output

Pass a Pydantic model to `run()` or `stream()` to install the Rust-owned final
output contract:

```python
class Answer(BaseModel):
    model_config = ConfigDict(extra="forbid")

    summary: str = Field(description="Concise answer summary.")
    next_step: str = Field(description="One practical next step for the reader.")


result = await agent.run(
    "Return JSON with exactly two fields: summary and next_step. "
    "Summarize the repository and give one practical next step.",
    final_output_model=Answer,
)
assert result.structured_output is not None
assert result.structured_output.summary
assert result.structured_output.next_step
```

`final_output_json` is the exact JSON value recorded by Rust. Invalid Python
model declarations fail before a run starts, a native final-output contract
failure is `MerryConfigError`, and a recorded JSON value that cannot be decoded
by the requested Pydantic model is `MerryOutputError`. Output state remains
Rust-owned in every case.

## Save And Resume

Session persistence is explicit and file-backed in the current SDK:

```python
from pathlib import Path

store_root = Path("sessions")
await agent.save_session()

resumed = await (
    merry.Agent.builder("demo")
    .provider(merry.OpenAICompatible(api_key="...", model="gpt-4.1-mini"))
    .session_store(store_root)
    .resume()
)
```

The configured path is a store root directory. Each session is persisted under
`<store_root>/<session_id>/state.json`. An explicit root may be passed to
`resume(path)` when it differs from the configured store root.

The session document, ledger, artifact references, and resume validation are
owned by Rust. Python only supplies the provider and store configuration.

## Errors And Tests

All native failures are mapped to `MerryError` subclasses carrying
`MerryErrorInfo` (`code`, `domain`, `message`, `hint`, `retryability`, and
bounded `context`). Native diagnostic messages are operational details and may
include values derived from the configured environment, such as filesystem
paths. Treat exception messages as sensitive before forwarding or persisting
them.

Deterministic tests use a feature-gated Rust fake provider:

```bash
cd sdks/python
maturin develop --uv --features test-utils
uv run --with ruff --with ty ruff check .
uv run --with ruff --with ty ty check
uv run --with pytest python -m pytest tests -q
uv build
```

The fake provider is not part of the default wheel feature set. See
[`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md) for the Rust owner and test
evidence for each public capability.
