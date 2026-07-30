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

To inspect concurrent independent multi-turn runtimes, run:

```bash
uv run examples/multi_runtime.py
```

The multi-runtime example runs three runtimes concurrently. Each runtime then
runs three sequential turns and prints the runtime label, round number, runtime
handle session id, the prompt-cache-key hint derived from that session id, the
session id carried by every event, and provider-reported usage. Use it to check
runtime isolation and event/session plumbing. Treat `cached_input_tokens` as a
best-effort provider diagnostic; a zero value for one concurrent runtime is not
by itself a wiring failure.

To inspect one stable session with a deliberately long repeated prefix, run:

```bash
uv run examples/single_runtime_cache_probe.py
```

The cache probe runs one runtime for several sequential turns and prints the
session id Merry uses as the OpenAI `prompt_cache_key` hint, the last-turn
`cached_input_tokens` value when the provider reports it, and the full usage
snapshot. It is a live observation probe, not a deterministic test: OpenAI
prompt caching requires eligible long prompts, exact matching prefixes, and
provider-side cache routing, so cache hits remain best-effort.

`MERRY_OPENAI_BASE_URL` is optional when using the default OpenAI-compatible
endpoint.

```python
stream = runtime.stream("...")

async for event in stream:
    print(event["type"])

result = await stream.result()
```

To inspect a long-lived interactive run with separate event, input, and control
handles, run:

```bash
uv run examples/interactive_agent.py
```

For a long-lived interactive agent run, split event consumption from input and
control:

```python
run = runtime.start_interactive()

asyncio.create_task(render(run.stream))

await run.input.submit_next("Inspect the current failure.")
backlog = await run.input.enqueue("After that, summarize the next step.")
await backlog.update(backlog.text + " Keep it brief.")
await run.control.interrupt()
```

`submit_next()` preempts backlog at the next boundary. `enqueue()` adds normal
backlog input that the run consumes automatically and returns a pending input
handle with `lane`, `text`, and `update()`/`remove()` methods. Only
suspended input created by an interrupt requires explicit resume or discard.
To reorder pending input, mutate a snapshot list and submit the whole order with
`replace_pending_order(lane, items)`.

Interactive input/control handles do not replace the existing `RuntimeStream`
bridge-tool path; Python bridge tools continue to be resolved by consuming
`runtime.stream(...)`.

The same configuration can be passed directly:

```python
runtime = merry.Runtime(
    config=merry.RuntimeConfig(
        provider=merry.OpenAICompatibleProvider(
            api_key="...",
            model="...",
            base_url="https://api.example.test/v1",
            protocol="chat_completions",
            retry=merry.ProviderRetryConfig(
                max_attempts=6,
                initial_delay_ms=1000,
                max_delay_ms=120000,
                max_elapsed_ms=300000,
                jitter=True,
            ),
        )
    )
)
```

Python SDK retry is opt-in. When enabled, Merry retries transient provider
setup or stream failures only before the first observable output delta or tool
call. Once output is visible, errors are forwarded without replay, preventing
duplicate text and duplicate tool calls.

Anthropic Messages uses the same runtime surface:

```python
runtime = merry.Runtime.with_anthropic(
    api_key="...",
    model="claude-sonnet-4-5",
    default_max_output_tokens=4096,
)
```

## Session Identity

Python SDK runtimes are in-memory and ephemeral by default. Each `Runtime()`,
`Runtime.from_env()`, or `Runtime.with_openai_compatible(...)` call creates a
fresh random session id unless `session_id` is passed explicitly.

Use an explicit session id when you want stable logs or future store/resume
debugging:

```python
runtime = merry.Runtime.from_env(session_id="tenant-a.debug_1")
```

Session ids are filesystem-safe strings. They may contain ASCII letters,
digits, `.`, `_`, and `-`.

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

The native stream driver sends an internal bridge tool request to the Python
wrapper, Python executes the registered handler, then Python submits the result
back to the same Rust runtime session. Public events still show ordinary
`tool_call_started` and `tool_call_finished` records. Bridge handlers run in the
host Python process; Merry profiles do not sandbox arbitrary host code.

## Structured Final Output

`final_output_model` asks the Rust runtime to expose a reserved terminal tool
for the model to call when the task is complete. The model can still call normal
runtime or bridge tools first; the run completes only when the reserved final
output tool is called. Plain text completion is reported as `blocked` while this
contract is active.

```python
class OrderStatusFinalOutput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    order_id: str = Field(description="Stable order identifier in the final answer.")
    status: str = Field(description="Final fulfillment status for the order.")

stream = runtime.stream(
    "Use lookup_order with order_id A123, then submit the final structured order status.",
    final_output_model=OrderStatusFinalOutput,
)

async for event in stream:
    print(event["type"])

result = await stream.result()
print(result.final_output.status)
print(result.final_output_json)
```

`result.final_output` is an instance of the Pydantic model when
`final_output_model` is provided. `result.final_output_json` keeps the exact JSON
payload recorded by the Rust runtime.

Run the live example:

```bash
uv run examples/final_output_model.py
```

## Agent Loop Budget

SDK runs use the Rust runtime's bounded agent loop. `max_model_turns` limits the
number of model turns started by one `run(...)` or `stream(...)` call. The
generic SDK default is 128 model turns. Runtime context compaction may happen
within a run, but it does not reset this control-flow and cost budget.

```python
stream = runtime.stream(
    "Run a task that may need many tool continuations.",
    max_model_turns=32,
)
```

Coding-agent product entry points should pass the coding profile default of
1024 model turns for one top-level user task.

## Tests

```bash
uv run --with pytest python -m pytest tests -q
```
