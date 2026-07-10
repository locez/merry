# OpenAI Chat And Anthropic Providers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver selectable OpenAI Responses, OpenAI Chat Completions, and Anthropic Messages providers through CLI, Rust, PyO3, and Python APIs.

**Architecture:** Provider crates privately adapt Merry-owned request and event types. OpenAI shares transport/SSE infrastructure across two protocol adapters; Anthropic is a separate direct-`reqwest` crate. CLI parses a tagged named-provider registry and constructs role-specific `Arc<dyn ModelProvider>` instances.

**Tech Stack:** Rust 2024, Reqwest/Rustls, Tokio, Serde, TOML, PyO3, Python dataclasses, local TCP mock servers.

---

### Task 1: Add Provider-Neutral Capability And Finish Controls

**Files:**
- Modify: `crates/merry-llm/src/capability.rs`
- Modify: `crates/merry-llm/src/request.rs`
- Modify: `crates/merry-llm/src/response.rs`
- Modify: `crates/merry-llm/tests/protocol.rs`
- Modify: `crates/merry-runtime/src/runtime/provider_step.rs`

- [ ] **Step 1: Write failing serialization and runtime tests**

Cover `ParallelToolCalls::{Auto, Enabled, Disabled}`, default `Auto`, provider
capability resolution, and `FinishReason::Blocked` projecting a stable blocked
diagnostic rather than successful completion.

- [ ] **Step 2: Replace the boolean generation field**

Add the enum and store it in `GenerationConfig`. Keep a compatibility
constructor that maps `bool` to enabled/disabled, but expose
`parallel_tool_calls()` as the authoritative API.

- [ ] **Step 3: Resolve effective capability before rendering**

Runtime resolves `Auto` from `ModelCapabilities::supports_parallel_tool_calls`.
Explicit `Enabled` against an unsupported provider fails before network IO.
Pass the effective enabled/disabled value in the request generation snapshot.

- [ ] **Step 4: Add blocked finish handling**

Add `FinishReason::Blocked`; runtime emits diagnostic code `model_blocked`
without recording a successful assistant output.

- [ ] **Step 5: Run focused tests**

```bash
cargo test -p merry-llm --test protocol generation -- --nocapture
cargo test -p merry-runtime provider_step_flow blocked -- --nocapture
```

Expected: PASS.

### Task 2: Split OpenAI By Protocol Without Changing Responses Behavior

**Files:**
- Create: `crates/merry-provider-openai/src/responses/mod.rs`
- Move/split: existing `render.rs`, `parse.rs`, and `wire.rs` into `responses/`
- Create: `crates/merry-provider-openai/src/transport.rs`
- Modify: `crates/merry-provider-openai/src/config.rs`
- Modify: `crates/merry-provider-openai/src/provider.rs`
- Modify: `crates/merry-provider-openai/src/lib.rs`
- Modify: existing OpenAI tests

- [ ] **Step 1: Add `OpenAiProtocol` config tests**

Assert direct construction defaults to `Responses`, `.with_protocol` selects
Chat Completions, Debug output includes protocol but not the key, and endpoint
joining rejects query/fragment base URLs.

- [ ] **Step 2: Add protocol enum and route provider setup**

```rust
pub enum OpenAiProtocol {
    Responses,
    ChatCompletions,
}
```

Move shared headers, request sending, status classification, retry-after,
request IDs, byte/SSE framing, and cancellation into `transport.rs`. Route body
rendering and event parsing by protocol.

- [ ] **Step 3: Preserve Responses fixtures**

Run all existing OpenAI tests after the file move. There must be no wire JSON
or normalized-event change except multi-tool enablement from the prior plan.

```bash
cargo test -p merry-provider-openai responses -- --nocapture
```

Expected: PASS.

### Task 3: Implement OpenAI Chat Completions Rendering

**Files:**
- Create: `crates/merry-provider-openai/src/chat_completions/mod.rs`
- Create: `crates/merry-provider-openai/src/chat_completions/wire.rs`
- Create: `crates/merry-provider-openai/src/chat_completions/render.rs`
- Test: module unit tests and `crates/merry-provider-openai/tests/provider_stream.rs`

- [ ] **Step 1: Write request-shape tests**

Cover system/user/assistant messages, one assistant message with two ordered
tool calls, two following tool-result messages, function schemas,
`parallel_tool_calls`, `max_completion_tokens`, `response_format`,
`stream=true`, and `stream_options.include_usage=true`.

- [ ] **Step 2: Add private borrowed wire structs**

Use `#[serde(skip_serializing_if = "Option::is_none")]` for optional request
fields. Do not expose Chat types from crate `lib.rs`.

- [ ] **Step 3: Render grouped history**

Walk `ModelRequest::input()`. Coalesce consecutive model tool calls into one
assistant `tool_calls` message and render their following ordered results as
`role = "tool"` messages. Reject incomplete groups before serialization.

- [ ] **Step 4: Run rendering tests**

```bash
cargo test -p merry-provider-openai chat_completions::render -- --nocapture
```

Expected: PASS.

### Task 4: Implement Chat Completions SSE Parsing

**Files:**
- Create: `crates/merry-provider-openai/src/chat_completions/parse.rs`
- Create fixtures under `crates/merry-provider-openai/tests/fixtures/`
- Modify: `crates/merry-provider-openai/tests/provider_stream.rs`

- [ ] **Step 1: Add fixtures and failing parser tests**

Cover fragmented UTF-8/SSE frames, live text, two interleaved tool calls by
index, final usage-only chunk, `[DONE]`, content filter, malformed arguments,
unexpected multiple choices, and EOF before finish.

- [ ] **Step 2: Implement indexed accumulation**

Track choice zero, aggregate text, tool buffers in `BTreeMap<u64, Buffer>`,
usage, and finish state. Emit a tool event at finalization after parsing each
complete JSON object. Emit one completed response before accepting `[DONE]`.

- [ ] **Step 3: Add delayed mock-server streaming test**

The server writes one content chunk, waits on a channel, then writes finish and
`[DONE]`. Assert the provider stream yields the delta before releasing the
server.

- [ ] **Step 4: Run Chat provider tests**

```bash
cargo test -p merry-provider-openai chat_completions -- --nocapture
```

Expected: PASS.

### Task 5: Sanitize OpenAI HTTP Errors

**Files:**
- Modify: `crates/merry-provider-openai/src/error.rs`
- Modify: `crates/merry-provider-openai/src/transport.rs`
- Modify: `crates/merry-provider-openai/tests/provider_stream.rs`

- [ ] **Step 1: Add a secret-bearing error response test**

Return a 400 body containing a fake API key and prompt. Assert the final error
contains status, bounded provider error code, and request ID, but not the body,
key, prompt, or response headers.

- [ ] **Step 2: Parse only bounded metadata**

Read at most the existing response-size bound, extract a validated short
`error.type` or `error.code`, then discard the body. Build the public message
from provider name, status, code, and request ID only.

- [ ] **Step 3: Run all OpenAI tests**

```bash
cargo test -p merry-provider-openai
```

Expected: PASS.

### Task 6: Scaffold The Anthropic Provider Crate

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `crates/merry-provider-anthropic/Cargo.toml`
- Create: `crates/merry-provider-anthropic/src/lib.rs`
- Create: `crates/merry-provider-anthropic/src/config.rs`
- Create: `crates/merry-provider-anthropic/src/error.rs`

- [ ] **Step 1: Add failing config tests**

Cover defaults, base URL, API version, output-token default, provider name,
capabilities, invalid values, and redacted Debug.

- [ ] **Step 2: Add the workspace crate and config**

Implement `AnthropicProviderConfig::new(api_key)` with defaults:

```text
base_url = https://api.anthropic.com
anthropic_version = 2023-06-01
default_max_output_tokens = 4096
provider_name = anthropic
```

Use a non-zero output-token type and the same strict text/base URL validation
style as OpenAI.

- [ ] **Step 3: Run config tests**

```bash
cargo test -p merry-provider-anthropic config -- --nocapture
```

Expected: PASS.

### Task 7: Implement Anthropic Request Rendering

**Files:**
- Create: `crates/merry-provider-anthropic/src/wire.rs`
- Create: `crates/merry-provider-anthropic/src/render.rs`
- Test: module unit tests

- [ ] **Step 1: Write full request-shape tests**

Cover top-level system blocks, user/assistant messages, two `tool_use` blocks,
ordered user `tool_result` blocks, tool schemas, output limit fallback,
explicit output limit, disabled parallel calls, structured output, explicit
effort, and omission of unset optional fields.

- [ ] **Step 2: Add private wire structs and renderer**

Render only standard Messages fields. Group calls and results according to the
validated Merry batch continuation. Reject an unsupported Merry input shape
before HTTP setup.

- [ ] **Step 3: Run render tests**

```bash
cargo test -p merry-provider-anthropic render -- --nocapture
```

Expected: PASS.

### Task 8: Implement Anthropic Streaming And HTTP

**Files:**
- Create: `crates/merry-provider-anthropic/src/parse.rs`
- Create: `crates/merry-provider-anthropic/src/provider.rs`
- Create: `crates/merry-provider-anthropic/tests/provider_stream.rs`

- [ ] **Step 1: Write SSE state-machine tests**

Cover message start, immediate text delta, two tool blocks with interleaved
`input_json_delta`, usage updates, message stop, ping, unknown metadata,
in-stream error, refusal, malformed JSON, unsupported known output blocks, EOF,
and cancellation.

- [ ] **Step 2: Implement per-content-block state**

Track block index and kind. Text emits immediately. Tool JSON accumulates until
`content_block_stop`, then parses to one `ModelToolCall`. Final completion uses
the ordered blocks, normalized usage, and mapped stop reason.

- [ ] **Step 3: Implement direct Reqwest provider**

POST `{base_url}/v1/messages` with `x-api-key`, `anthropic-version`, content
type, and SSE accept headers. Apply the same cancellation, response-size,
status classification, retry-after, and sanitized error rules as OpenAI without
sharing provider-private code across crates.

- [ ] **Step 4: Add delayed and cancellation mock-server tests**

Prove the first Anthropic text delta arrives before `message_stop`, dropping the
stream closes the request, and raw error bodies never appear in errors.

- [ ] **Step 5: Run Anthropic tests**

```bash
cargo test -p merry-provider-anthropic
```

Expected: PASS.

### Task 9: Replace CLI Provider Configuration With A Named Registry

**Files:**
- Modify: `crates/merry-cli/Cargo.toml`
- Rewrite: `crates/merry-cli/src/config/provider.rs`
- Modify: `crates/merry-cli/src/config/runtime.rs`
- Rewrite: `crates/merry-cli/src/provider_config.rs`
- Modify: `crates/merry-cli/src/runtime_config.rs`
- Modify: provider-related CLI tests
- Rewrite: `examples/config.toml`

- [ ] **Step 1: Write tagged-registry parsing tests**

Cover one OpenAI Chat entry, one Anthropic entry, role override, credential from
inline/file/env, ambiguous and missing credentials, unknown alias, invalid
protocol/type field, capability override, and parsing the tracked example.

- [ ] **Step 2: Add strict tagged TOML entries**

Use a flattened named map with a strict internally tagged enum:

```rust
#[serde(tag = "type", rename_all = "snake_case")]
enum ProviderToml {
    Openai(OpenAiProviderToml),
    Anthropic(AnthropicProviderToml),
}
```

Reserve `default` and `retry`; reject a named provider using either reserved
name. Resolve `api_key_file` relative to the loaded config path and
`api_key_env` from the process environment.

- [ ] **Step 3: Build providers by alias and role**

Materialize each referenced provider once as `Arc<dyn ModelProvider>`. Apply the
global retry policy at runtime role registration. Unknown aliases report their
source path, such as `models.context_compaction.provider`.

- [ ] **Step 4: Stop forcing reasoning effort**

`generation_config` returns `GenerationConfig::default()` unless the selected
model binding explicitly sets effort, output limit, or parallel mode. TUI
metadata displays `provider default` when effort is absent.

- [ ] **Step 5: Rewrite the tracked example**

Use `primary` OpenAI Chat and a commented Anthropic provider/role override.
Keep secrets as environment-variable or relative-file names only.

- [ ] **Step 6: Run config and CLI tests**

```bash
cargo test -p merry-cli config -- --nocapture
cargo test -p merry-cli provider_config -- --nocapture
```

Expected: PASS.

### Task 10: Update The Rust Facade And PyO3 Boundary

**Files:**
- Modify: `crates/merry/Cargo.toml`
- Replace: `crates/merry/src/providers/openai_compatible.rs`
- Create: `crates/merry/src/providers/openai.rs`
- Create: `crates/merry/src/providers/anthropic.rs`
- Modify: `crates/merry/src/providers/mod.rs`
- Modify: `crates/merry-py/src/runtime.rs`
- Modify: `crates/merry-py/tests/bindings.rs`

- [ ] **Step 1: Write facade and binding tests**

Assert OpenAI accepts a protocol, Anthropic accepts base URL/version/output
default, retry applies to either, invalid values produce typed config errors,
and Debug/Python exceptions do not reveal keys.

- [ ] **Step 2: Add typed facade builders**

Expose `openai()` and `anthropic()` builders that produce
`ConfiguredModelProvider`. Keep `openai_compatible()` as a deprecated alias to
OpenAI Responses for the 0.1 series.

- [ ] **Step 3: Add native Python constructors**

Add `Runtime.with_openai(..., protocol, ...)` and
`Runtime.with_anthropic(..., anthropic_version, default_max_output_tokens,
...)`. Keep `with_openai_compatible` delegating to the Responses constructor.

- [ ] **Step 4: Run Rust facade/binding tests**

```bash
cargo test -p merry -p merry-py
```

Expected: PASS.

### Task 11: Update The Python SDK

**Files:**
- Modify: `sdks/python/merry/_runtime.py`
- Modify: `sdks/python/merry/__init__.py`
- Modify: `sdks/python/tests/test_openai_runtime.py`
- Modify: `sdks/python/tests/test_production_sdk.py`
- Create: `sdks/python/tests/test_anthropic_runtime.py`
- Modify: SDK examples and `sdks/python/README.md`

- [ ] **Step 1: Write provider union and environment tests**

Cover `OpenAIProvider(protocol="chat_completions")`, `AnthropicProvider`, the
compatibility alias, `RuntimeConfig` dispatch, and
`MERRY_PROVIDER=openai|anthropic` environment selection.

- [ ] **Step 2: Add Python dataclasses and dispatch**

Define `OpenAIProvider`, `AnthropicProvider`, and
`Provider = OpenAIProvider | AnthropicProvider`. Keep
`OpenAICompatibleProvider` as an alias/subclass with Responses protocol.

- [ ] **Step 3: Implement provider-specific `from_env`**

Read `MERRY_OPENAI_*`/standard OpenAI variables or `MERRY_ANTHROPIC_*`/standard
Anthropic variables according to `MERRY_PROVIDER`. Errors identify the missing
provider-specific variable.

- [ ] **Step 4: Run Python tests**

```bash
cd sdks/python && uv run --with pytest python -m pytest tests -q
```

Expected: all tests pass.

### Task 12: Verify The Provider Slice

**Files:**
- Verify only

- [ ] **Step 1: Run workspace checks**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cd sdks/python && uv run --with pytest python -m pytest tests -q
git diff --check
```

Expected: all commands exit zero. Live provider smoke tests remain opt-in.
