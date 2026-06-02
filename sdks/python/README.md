# Merry Python SDK

Python bindings for the Rust-owned Merry runtime.

The Python package is a thin facade over `merry._merry`; runtime state,
events, artifacts, tool continuation, and bridge tool registration stay owned
by Rust.

## Build

From the repository root:

```bash
cd sdks/python
uv sync
uv pip install --python .venv/bin/python --reinstall --editable .
```

After changing Rust code under `crates/merry-py`, `crates/merry-runtime`, or
`crates/merry-core`, rebuild the editable extension:

```bash
uv pip install --python .venv/bin/python --reinstall --editable .
```

## Live Provider Example

`examples/basic_runtime.py` uses an OpenAI-compatible provider configured from
environment variables. It consumes live runtime events first, then reads the
final result from the same run:

```bash
export MERRY_OPENAI_API_KEY=...
export MERRY_OPENAI_MODEL=...
export MERRY_OPENAI_BASE_URL=https://api.example.test/v1
uv run examples/basic_runtime.py
```

`MERRY_OPENAI_BASE_URL` is optional when using the default OpenAI-compatible
endpoint.

```python
stream = runtime.stream("...")

async for event in stream:
    print(event["kind"]["type"])

result = await stream.result()
```

The same configuration can be passed directly:

```python
runtime = merry.Runtime(
    config=merry.RuntimeConfig(
        provider=merry.OpenAICompatibleProvider(
            api_key="...",
            model="...",
            base_url="https://api.example.test/v1",
        )
    )
)
```

## Bridge Tool Example

`examples/tool_bridge.py` uses the same OpenAI-compatible provider config,
registers a Python bridge tool, streams events, then prints the final result:

```bash
uv run examples/tool_bridge.py
```

Tool inputs and outputs are declared with Pydantic models. Every model field
must include a `Field(description=...)`; Merry rejects underspecified tool
contracts before registering them with the Rust runtime.

```python
from pydantic import BaseModel, ConfigDict, Field

class LookupOrderInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier to look up.")

class LookupOrderOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier that was looked up.")
    status: str = Field(description="Current fulfillment status for the order.")

async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
    """Look up an order by id."""
    return LookupOrderOutput(order_id=args.order_id, status="shipped")

runtime.register_tool(lookup_order)
```

`register_tool(func)` derives the tool name from `func.__name__`, the tool
description from the function docstring, the input schema from the single
Pydantic argument annotation, and the output contract from the return
annotation.

The same tool can be registered directly on a runtime:

```python
@runtime.tool
async def lookup_order(args: LookupOrderInput) -> LookupOrderOutput:
    """Look up an order by id."""
    return LookupOrderOutput(order_id=args.order_id, status="shipped")
```

The Rust runtime emits `bridge_tool_call_requested`, Python executes the
registered handler, then Python submits the result back to the same Rust runtime
session. Bridge handlers run in the host Python process; Merry profiles do not
sandbox arbitrary host code.

## Tests

```bash
uv run --with pytest python -m pytest tests -q
```
