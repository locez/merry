# Python-First Runtime SDK Contract

Date: 2026-05-31

## Purpose

Merry should be usable as an embedded agent runtime, not only as a CLI or a
binary protocol wrapper. The Python binding is therefore a product-facing SDK
surface, not a thin shell around `merry-cli`.

This spec defines the first public contract for embedding Merry from Python:
runtime construction, tool registration, event consumption, and stable error
exposure. It intentionally keeps implementation details out of the contract so
Rust runtime internals, provider adapters, and Python ergonomics can evolve
without breaking business callers.

## Current Position

These points reflect the current repository shape:

- Rust runtime owns sessions, tool execution, artifact recording, context
  compilation, and compaction.
- Provider crates adapt external model APIs into Merry-owned provider traits.
- `merry-core` already has provider-neutral `ToolSpec`, `PendingToolCall`,
  `ToolCallResult`, `RuntimeEvent`, and a small serializable `ErrorInfo`
  shape.
- `merry-runtime` already has typed internal `RuntimeError` variants for
  admission, artifact, tool, context, checkpoint, compaction, and model-role
  failures.
- The existing `ErrorInfo` is too small for a Python business SDK. It is a
  useful predecessor, but the public SDK contract needs stable domains,
  retryability, hints, and redacted structured context.
- Codex is a useful reference for separating internal errors from public error
  information, but its public error shape is optimized for a CLI/app-server
  product. Merry should not copy Codex's `message + coarse enum` model as the
  Python SDK contract.

## Non-Goals

- Do not design a CLI-first error rendering system in this spec.
- Do not expose provider wire errors as Python SDK API.
- Do not make Python bindings reimplement runtime behavior.
- Do not make Pydantic, Python callbacks, or PyO3 types leak into `core`,
  `runtime`, `llm`, or provider crates.
- Do not treat traceback, source chain, raw tool arguments, raw provider
  payloads, or raw tool outputs as part of the stable error contract.
- Do not add a general plugin system in this spec.
- Do not solve long-term memory, task anchors, or TUI command design here.

## Product Boundary

The Python binding should embed the Rust runtime:

```text
Python application
  builds a Merry runtime
  registers Python tools
  configures providers and runtime policy
  starts turns
  consumes events or awaits a final turn result
  handles structured Merry errors

Rust runtime
  owns session state and context assembly
  streams provider-neutral model events
  executes host-registered tools through a narrow tool boundary
  records artifacts and checkpoints
  emits provider-neutral runtime events
  maps internal failures to MerryErrorInfo
```

Python is the host for business tools. Rust remains the owner of runtime
state, event ordering, cancellation, artifact durability, and provider
continuation.

## Layering

The public error and SDK layers should follow this shape:

```text
Internal typed errors
  -> MerryErrorInfo
  -> consumer mapping
```

### Internal Typed Errors

Internal errors are Rust control-flow errors such as `RuntimeError`,
`CoreError`, provider `ModelError`, tool execution infrastructure errors,
context errors, and compaction errors.

They may carry rich sources, internal enum variants, and implementation
details. They are not a cross-language compatibility contract.

### MerryErrorInfo

`MerryErrorInfo` is the stable external error information object.

It is serializable, redacted, bounded, and suitable for Python exceptions,
runtime events, CLI output, TUI state, logs, and JSON reports.

Suggested shape:

```text
MerryErrorInfo:
  code: stable dotted string
  domain: stable enum
  message: short user-facing message
  hint: optional actionable next step
  retryability: stable enum
  context: redacted structured context map
```

Example:

```json
{
  "code": "tool.executor_exception",
  "domain": "tool",
  "message": "Tool `lookup_order` raised an unexpected exception.",
  "hint": "Handle expected business failures inside the tool or raise a Merry tool error.",
  "retryability": "not_retryable",
  "context": {
    "tool_name": "lookup_order",
    "call_id": "call_123"
  }
}
```

`MerryErrorInfo` is not a debug dump. Backtraces, Rust source
chains, Python tracebacks, raw tool arguments, raw tool output, provider
responses, and secrets stay out of this structure by default.

### Consumer Mapping

Consumers map `MerryErrorInfo` into their native experience:

- Python maps it to `MerryError` and subclasses.
- CLI renders it as a concise error plus optional hint.
- TUI renders it as state and recoverable actions.
- JSON/event consumers receive it as structured data.

Consumers must not match on internal Rust error enum variants.

## MerryErrorInfo Fields

### Code

`code` is the primary stable programmatic key. It should be a dotted string:

```text
config.invalid
config.missing_provider
provider.setup_failed
provider.stream_failed
tool.input_invalid
tool.executor_exception
tool.domain_failed
context.compile_failed
compaction.invalid_candidate
compaction.model_failed
runtime.step_already_active
runtime.cancelled
policy.denied
sandbox.denied
```

Codes should be specific enough for business callers to branch on, but not so
specific that every internal enum variant becomes public API.

### Domain

Suggested first domains:

```text
config
provider
runtime
tool
policy
context
compaction
artifact
sandbox
internal
```

The domain is for coarse grouping, metrics, and exception subclasses. Business
logic should prefer `code` for precise branching.

### Message

`message` is short, human-readable, and safe to show. It should not require
parsing. It should avoid leaking secrets or unbounded payloads.

### Hint

`hint` is optional. It should tell the caller what to change when there is a
reasonable next action, such as:

```text
Set [models.context_compaction].model or configure the default provider.
Register tool `lookup_order` before starting the turn.
Handle this expected tool failure by returning ToolResult.failed(...).
```

### Retryability

Suggested enum:

```text
retryable
not_retryable
user_action_required
cancelled
unknown
```

This is host-facing behavior guidance. It does not force runtime retry policy.

### Context

`context` is a bounded map of safe structured fields. First allowed fields
should be small IDs and configuration locators:

```text
session_id
turn_id
call_id
tool_name
provider_name
model_role
config_path
field_path
artifact_id
checkpoint_id
http_status
exit_code
```

Context values should be scalars or short lists of scalars. Do not place raw
tool input, raw tool output, provider JSON, stdout/stderr bodies, file content,
or tracebacks in `context`.

## Python Error API

Python should expose one stable base exception:

```python
class MerryError(Exception):
    info: MerryErrorInfo

    @property
    def code(self) -> str: ...

    @property
    def domain(self) -> str: ...

    @property
    def retryability(self) -> str: ...
```

Convenience subclasses may exist:

```text
MerryConfigError
MerryProviderError
MerryRuntimeError
MerryToolError
MerryPolicyError
MerryContextError
MerryCompactionError
MerryInternalError
MerryTurnError
```

The stable compatibility contract is still `MerryError.info`, especially
`info.code` and `info.domain`. Subclasses are ergonomic helpers and may group
multiple codes.

## Runtime Construction API

The Python SDK should make runtime construction explicit:

```python
runtime = merry.Runtime.builder() \
    .with_config_file("merry.toml") \
    .with_tool(lookup_order) \
    .build()
```

Equivalent direct construction should be possible for applications that do not
want a config file:

```python
runtime = merry.Runtime(
    providers={...},
    models={...},
    tools=[lookup_order],
    compaction={...},
)
```

Construction failures should raise `MerryError` before a session starts:

```text
config.invalid
config.missing_provider
provider.setup_failed
tool.registration_invalid
tool.duplicate_name
```

Runtime construction must not require a clean project folder unless the caller
uses workspace-specific tools or policies that require one.

## Tool Definition Contract

Rust runtime should distinguish provider-visible tool specification from
runtime-owned execution registration.

Provider-visible tool spec:

```text
ToolSpec:
  name
  description
  input_schema
```

Runtime execution registration:

```text
ToolRegistration:
  spec
  executor
  side_effect_class
  timeout
  cancellation_policy
  error_policy
```

Python should provide ergonomic helpers that compile down to the runtime-owned
registration:

```python
@merry.tool(
    name="lookup_order",
    description="Look up an order by id.",
    input_model=LookupOrderInput,
)
async def lookup_order(ctx: merry.ToolContext, input: LookupOrderInput) -> OrderResult:
    ...
```

The SDK should also allow explicit JSON schema for callers that do not use
Pydantic:

```python
runtime.register_tool(
    name="lookup_order",
    description="Look up an order by id.",
    input_schema={...},
    handler=lookup_order,
)
```

Pydantic is a Python binding convenience only. The Rust runtime should receive
normal JSON schema and JSON arguments.

## Tool Execution Outcomes

Tool execution has three different failure classes. They should not be
collapsed into one string.

### Tool Input Invalid

The model supplied arguments that do not match the tool schema or Python input
model.

Default behavior:

```text
resolve the tool call as failed
return a safe model-visible tool error
emit a tool-result event carrying MerryErrorInfo
continue the turn if provider protocol allows continuation
```

Suggested code:

```text
tool.input_invalid
```

### Tool Domain Failure

The tool ran and the business domain produced an expected failure, such as
"order not found".

Tool authors should return an explicit failed tool result or raise a Merry
tool-domain error intended to be visible to the model.

Default behavior:

```text
record artifact-backed failed tool result
include small model-visible failure content
include MerryErrorInfo for host/runtime observers
continue the turn
```

Suggested code:

```text
tool.domain_failed
```

### Tool Executor Exception

The Python callable raised an unexpected exception or the host execution
boundary failed before a durable tool result was produced.

Default behavior:

```text
fail the turn with MerryErrorInfo
do not expose traceback to the model by default
emit/raise a host-visible MerryToolError
```

Suggested code:

```text
tool.executor_exception
```

This default keeps business application bugs visible to the host instead of
silently feeding them back to the model as if they were normal tool output.
A later opt-in policy may convert unexpected tool exceptions into safe failed
tool results, but that should not be the default SDK behavior.

## Event Contract

Python should support both a final-result API and a streaming API.

Final-result API:

```python
result = await runtime.run("Update the order status.")
```

Streaming API:

```python
async for event in runtime.run_stream("Update the order status."):
    ...
```

Proposed stream behavior:

- Runtime construction and setup failures raise `MerryError`.
- `run()` raises `MerryTurnError` when the turn terminal state is failed.
- `run_stream()` yields structured events whenever possible.
- A failed turn should be observable as a terminal event with
  `error_info: MerryErrorInfo`.
- `run_stream()` should raise only when the stream infrastructure itself fails
  before a coherent terminal event can be emitted.

This lets business callers choose between exception-oriented and event-oriented
control flow.

## Relationship To Existing ErrorInfo

The current `merry-core::ErrorInfo` has:

```text
code
message
```

`MerryErrorInfo` should supersede that public role. The implementation may
either rename the type or introduce the new shape and migrate existing event
fields over time. The design requirement is that the stable SDK-facing shape
is `MerryErrorInfo`, not a raw Rust error string and not a provider-specific
wire error.

For tool result errors, the same `MerryErrorInfo` shape can be used if the
fields remain bounded and safe. The host-visible meaning must remain clear:

- failed tool result does not necessarily fail the turn;
- failed turn does fail the turn;
- cancelled runtime is neither a successful result nor an unexpected crash.

## Redaction Rules

`MerryErrorInfo` must be safe by default:

- No API keys or authorization headers.
- No full provider request or response bodies.
- No full tool arguments unless the field is explicitly declared safe.
- No full stdout/stderr bodies.
- No file content.
- No Python traceback in stable fields.
- No local machine paths unless the field is a user-selected config path or a
  workspace-relative path already expected to be visible.

Debug logging may record richer data under a separate opt-in mechanism. That
debug data must not become the SDK compatibility contract.

## Cancellation And Timeouts

Python tool context should expose cancellation:

```python
async def tool(ctx, input):
    await ctx.check_cancelled()
```

Cancellation should map to:

```text
runtime.cancelled
tool.cancelled
provider.cancelled
```

with `retryability = cancelled`.

Timeouts should be explicit policy failures:

```text
tool.timeout
provider.timeout
```

with retryability chosen by the owning layer.

## Minimal Python Example

The first SDK slice should make this shape possible:

```python
import merry
from pydantic import BaseModel

class LookupOrderInput(BaseModel):
    order_id: str

@merry.tool(
    name="lookup_order",
    description="Look up an order by id.",
    input_model=LookupOrderInput,
)
async def lookup_order(ctx: merry.ToolContext, input: LookupOrderInput):
    order = await ctx.app.orders.get(input.order_id)
    if order is None:
        return merry.ToolResult.failed(
            code="order.not_found",
            message="Order was not found.",
            content={"found": False},
        )
    return {"found": True, "status": order.status}

runtime = merry.Runtime.builder() \
    .with_config_file("merry.toml") \
    .with_tool(lookup_order) \
    .build()

try:
    result = await runtime.run("Check order A123 and tell me its status.")
except merry.MerryError as error:
    print(error.code)
    print(error.info.context)
```

This is an API design target, not an implementation promise for the current
Rust-only MVP.

## Acceptance Criteria For The First Implementation Plan

The implementation plan following this spec should produce a small vertical
slice:

- A `MerryErrorInfo` type exists at the provider-neutral core boundary.
- Runtime construction/admission errors can map to `MerryErrorInfo`.
- Runtime failed/cancelled events carry `MerryErrorInfo`.
- Tool failed results can carry bounded `MerryErrorInfo`.
- Provider errors are normalized before they enter runtime events or Python
  exceptions.
- A future `merry-py` binding can expose `MerryError.info` without matching
  internal Rust enum variants.
- Tests cover serialization, redaction, stable code/domain mapping, and at
  least one tool-domain failure versus one tool-executor exception distinction.

## Design Decisions

- The provider-neutral public type should be named `MerryErrorInfo`. The
  existing `ErrorInfo` shape should be migrated or retired rather than becoming
  the long-term SDK-facing contract.
- `MerryErrorInfo` is error-only for the first slice. Warning or notice events
  may get sibling types later, but should not broaden this contract now.
- Unexpected Python tool exceptions should fail the turn by default in the
  first Python SDK slice. A later per-tool opt-in may convert such exceptions
  into safe failed tool output, but that is not the MVP default.
- The first stable code list should be limited to config, provider, runtime,
  tool, context, compaction, policy, sandbox, and internal failures that appear
  in the first implementation slice. Experimental codes should not be exposed
  as stable Python branching points.
