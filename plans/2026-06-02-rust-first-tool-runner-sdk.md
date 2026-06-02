# Rust-First Tool Runner SDK Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Merry tool registration Rust-first and expose Python SDK tools as thin bridge wrappers without making Python the primary runtime.

**Architecture:** Rust runtime owns tool specs, runner classification, capability policy, artifacts, ledger, and continuation. Tools declare where they run through `ToolRunner::{Runtime, Bridge}`; `RuntimeProfile` describes Merry-managed capabilities such as network and file/process access, not tool provenance. Python SDK registers bridge specs, listens for bridge tool-call events, runs Python handlers, and submits results back to Rust.

**Tech Stack:** Rust 2024, `merry-runtime`, `merry-core`, `merry-py` PyO3/maturin, Python asyncio wrappers under `sdks/python/merry`.

---

## Baseline Decisions

- Use `ToolRunner`, not `ToolBoundary` or `ToolMode`.
- `ToolRunner::Runtime` means Merry/Rust executes the tool executor.
- `ToolRunner::Bridge` means Merry emits a bridge tool-call event and an external SDK executes it.
- Network is controlled by `RuntimeProfile`, not by tool declaration.
- MVP network policy is `deny_network()` / `allow_network()`, no host allowlist yet.
- Bridge tools require an explicit `RuntimeBuilder::allow_bridge_tools()` opt-in because Merry cannot sandbox a handler running inside the host app process.
- Do not model `inspection`, `workspace`, `trusted_app`, or `agent` as runtime profiles in the MVP. They are overloaded names that mix product mode, file access, tool provenance, and trust.
- `RuntimeProfile` is a Merry-managed capability policy, not an OS sandbox guarantee. Actual file/process enforcement comes from bwrap/sandbox/file access runners where they are enabled.
- Runtime construction must allow pure in-memory tasks with no physical workspace directory. Workspace is an optional capability, not a runtime requirement.
- Do not ask tool authors to self-classify risk as read-only/write/network.
- Python SDK is a bridge runner and facade, not the primary owner of tool semantics.
- In-process Rust executors and SDK bridge handlers are host code. Runtime profiles constrain Merry-managed capabilities; they cannot prevent arbitrary host code from doing direct filesystem, process, or network operations outside Merry-provided runners.

## File Structure

- Modify `crates/merry-runtime/src/tool.rs`
  - Add `ToolRunner`.
  - Add ergonomic Rust-first `Tool`/`ToolDefinition` builder only if it stays thin over existing `RegisteredTool`.
- Create/modify `crates/merry-runtime/src/profile.rs`
  - Add `RuntimeProfile` as a capability policy with no profile-kind enum.
- Future file-access policy belongs in a separate type such as `FileAccess`, not in profile naming.
- Modify `crates/merry-runtime/src/runtime.rs`
  - Preserve existing `register_tool(RegisteredTool)` path.
  - Add `profile(RuntimeProfile)` for Merry-managed capabilities.
  - Add `allow_bridge_tools()` as explicit bridge opt-in.
  - Add bridge-aware registration and event/result flow only where runtime already owns state.
- Modify `crates/merry-core/src/event.rs`
  - Add SDK bridge event variant if existing `ToolCallPending` is not explicit enough.
- Modify `crates/merry-py/src/runtime.rs`
  - Expose thin registration for bridge specs.
  - Expose result submission; Rust still records artifact/ledger.
- Modify `sdks/python/merry/_runtime.py`
  - Keep `run()` non-blocking for asyncio.
  - Add Python bridge runner loop as wrapper around Rust events.
- Create `sdks/python/README.md`
  - Build/rebuild commands.
  - Config and live model example.
  - Tool bridge example.
- Create/update examples under `sdks/python/examples/`
  - `basic_runtime.py`: real provider config.
  - `tool_bridge.py`: Python SDK bridge tool.

## Task 1: Rust ToolRunner Contract

**Files:**
- Modify: `crates/merry-runtime/src/tool.rs`
- Test: `crates/merry-runtime/src/tool.rs`

- [x] **Step 1: Write failing Rust tests**

Add tests near existing tool tests:

```rust
#[test]
fn registered_tool_defaults_to_runtime_runner() {
    let tool = RegisteredTool::read_only(
        ToolSpec::new(
            ToolName::new("lookup_order").unwrap(),
            "Look up an order.",
            ToolInputSchema::new(object_input_schema()).unwrap(),
        ).unwrap(),
        Arc::new(OkExecutor),
    );

    assert_eq!(tool.runner(), ToolRunner::Runtime);
}

#[test]
fn bridge_tool_carries_spec_without_runtime_executor() {
    let tool = RegisteredTool::bridge(
        ToolSpec::new(
            ToolName::new("lookup_order").unwrap(),
            "Look up an order.",
            ToolInputSchema::new(object_input_schema()).unwrap(),
        ).unwrap(),
    );

    assert_eq!(tool.runner(), ToolRunner::Bridge);
    assert_eq!(tool.spec().name().as_str(), "lookup_order");
}
```

Run:

```bash
cargo test -p merry-runtime registered_tool_defaults_to_runtime_runner bridge_tool_carries_spec_without_runtime_executor
```

Expected: fail because `ToolRunner`, `runner()`, and `bridge()` do not exist.

- [x] **Step 2: Implement minimal ToolRunner**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRunner {
    Runtime,
    Bridge,
}
```

Store it in `RegisteredTool`. Existing constructors set `Runtime`; `bridge(spec)` sets `Bridge` and uses a no-op executor that cannot be executed directly.

- [x] **Step 3: Verify**

Run:

```bash
cargo test -p merry-runtime registered_tool_defaults_to_runtime_runner bridge_tool_carries_spec_without_runtime_executor
```

Expected: pass.

## Task 2: Runtime Profile Capability Policy

**Files:**
- Create: `crates/merry-runtime/src/profile.rs`
- Modify: `crates/merry-runtime/src/lib.rs`
- Test: `crates/merry-runtime/src/profile.rs`

- [x] **Step 1: Write failing Rust tests**

Add tests proving profile is capability-level only and has no overloaded profile-kind names:

```rust
#[test]
fn runtime_profile_controls_network_without_tool_network_field() {
    let profile = RuntimeProfile::default().allow_network();

    assert!(profile.network_allowed());
}

#[test]
fn runtime_profile_denies_network_by_default() {
    let profile = RuntimeProfile::default();

    assert!(!profile.network_allowed());
}

#[test]
fn runtime_profile_does_not_authorize_bridge_tools() {
    let profile = RuntimeProfile::default().allow_network();

    assert!(!profile.bridge_tools_allowed());
}
```

Run:

```bash
cargo test -p merry-runtime runtime_profile
```

Expected: fail because profile API does not exist.

- [x] **Step 2: Implement minimal profile**

Use a small public API:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeProfile {
    network_allowed: bool,
}
```

Methods:

```rust
RuntimeProfile::new()
RuntimeProfile::allow_network(self) -> Self
RuntimeProfile::deny_network(self) -> Self
RuntimeProfile::network_allowed(&self) -> bool
RuntimeProfile::bridge_tools_allowed(&self) -> bool // always false in MVP
```

- [x] **Step 3: Verify**

Run:

```bash
cargo test -p merry-runtime runtime_profile
```

Expected: pass.

## Task 3: Bridge Tool Event Flow

**Files:**
- Modify: `crates/merry-core/src/event.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Test: `crates/merry-runtime/tests/agent_loop.rs`

- [x] **Step 1: Write failing test**

Test that a model-requested bridge tool produces an observable bridge request instead of runtime executing a missing executor:

```rust
#[tokio::test]
async fn bridge_tool_call_emits_bridge_request_event() {
    let provider = scripted_tool_call_provider("lookup_order", json!({"order_id":"A123"}), "done");
    let runtime = Runtime::builder(session_id("sdk-bridge-tool"))
        .profile(RuntimeProfile::default())
        .allow_bridge_tools()
        .model_provider(Arc::new(provider), model_name("fake"))
        .register_tool(RegisteredTool::bridge(tool_spec("lookup_order")))
        .build()
        .unwrap();

    let events = collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Check order A123.").unwrap(),
        StepContext::default(),
    ).await.unwrap();

    assert!(events.iter().any(|event| matches!(
        event.kind(),
        RuntimeEventKind::SdkToolCallRequested { .. }
    )));
}
```

Run:

```bash
cargo test -p merry-runtime bridge_tool_call_emits_bridge_request_event
```

Expected: fail because event does not exist.

- [x] **Step 2: Implement bridge event**

Add event variant:

```rust
SdkToolCallRequested { call: PendingToolCall }
```

When runtime records pending tool call for a registered bridge tool, emit this event. Preserve existing `ToolCallPending` if needed for compatibility, but Python should key off the bridge event.

Also reject bridge tool registration unless `RuntimeBuilder::allow_bridge_tools()` was explicitly called, with a typed runtime build error.

- [x] **Step 3: Verify**

Run bridge event test. Expected: pass.

## Task 4: Python Thin Bridge Wrapper

**Files:**
- Modify: `crates/merry-py/src/runtime.rs`
- Modify: `sdks/python/merry/_runtime.py`
- Modify: `sdks/python/merry/__init__.py`
- Test: `sdks/python/tests/test_production_sdk.py`

- [x] **Step 1: Write failing Python tests**

Use:

```python
def test_runtime_config_constructs_openai_runtime():
    config = merry.RuntimeConfig(
        provider=merry.OpenAICompatibleProvider(
            api_key="sk-test",
            model="gpt-test",
            base_url="https://api.example.test/v1",
        ),
    )
    runtime = merry.Runtime(config=config)
    assert isinstance(runtime, merry.Runtime)
```

and:

```python
async def test_bridge_tool_executes_from_event():
    calls = []

    async def lookup_order(order_id: str):
        calls.append(order_id)
        return {"status": "shipped"}

    runtime = runtime_with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="Order shipped.",
    )
    runtime.register_tool(merry.Tool.bridge(
        name="lookup_order",
        description="Look up an order.",
        schema={"type": "object", "properties": {"order_id": {"type": "string"}}},
        handler=lookup_order,
    ))

    result = await runtime.run("Check order A123.")
    assert result.status == "completed"
    assert calls == ["A123"]
```

Run:

```bash
UV_CACHE_DIR=/tmp/merry-uv-cache uv run --project sdks/python --with pytest python -m pytest sdks/python/tests/test_production_sdk.py -q
```

Expected: fail before implementation.

- [x] **Step 2: Implement Python wrapper**

Add:

```python
@dataclass(frozen=True)
class OpenAICompatibleProvider: ...

@dataclass(frozen=True)
class RuntimeConfig: ...

@dataclass(frozen=True)
class Tool:
    @classmethod
    def bridge(...): ...
```

`Runtime.run()` must stay non-blocking by offloading native blocking calls with `asyncio.to_thread`.

- [x] **Step 3: Implement native methods**

Expose:

```rust
register_bridge_tool(name, description, schema_json)
submit_tool_success_json_blocking(call_id, artifact_id, content_json)
```

The native submit method must call `Runtime::submit_tool_result`; Python must not mutate artifact/ledger state itself.

- [x] **Step 4: Verify**

Run Python tests. Expected: pass.

## Task 5: README And Examples

**Files:**
- Create: `sdks/python/README.md`
- Modify: `sdks/python/examples/basic_runtime.py`
- Create: `sdks/python/examples/tool_bridge.py`

- [x] **Step 1: Add README**

Include exact commands:

```bash
cd sdks/python
uv sync
uv pip install --python .venv/bin/python --reinstall --editable .
uv run examples/basic_runtime.py
uv run examples/tool_bridge.py
```

Explain env:

```bash
export MERRY_OPENAI_API_KEY=...
export MERRY_OPENAI_MODEL=...
export MERRY_OPENAI_BASE_URL=...
```

Explain rebuild requirement after Rust changes.

- [x] **Step 2: Add examples**

`basic_runtime.py` uses `RuntimeConfig`.

`tool_bridge.py` uses `Tool.bridge`.

- [x] **Step 3: Verify examples import**

Run without secrets:

```bash
cd sdks/python
uv run examples/basic_runtime.py
```

Expected: clear config error, not import error.

## Task 6: Full Verification

Run:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
UV_CACHE_DIR=/tmp/merry-uv-cache uv run --project sdks/python --with pytest python -m pytest sdks/python/tests -q
```

Expected: all pass.

If Python extension methods changed, rebuild:

```bash
UV_CACHE_DIR=/tmp/merry-uv-cache uv pip install --python sdks/python/.venv/bin/python --reinstall --editable sdks/python
```

Completed verification:

- [x] `cargo fmt --all --check`
- [x] `cargo clippy --all-targets --all-features -- -D warnings`
- [x] `cargo test --all`
- [x] `UV_CACHE_DIR=/tmp/merry-uv-cache uv run --with pytest python -m pytest tests -q` from `sdks/python`
- [x] `UV_CACHE_DIR=/tmp/merry-uv-cache uv run examples/basic_runtime.py` from `sdks/python` returns a clear config error when no API key is configured
- [x] `UV_CACHE_DIR=/tmp/merry-uv-cache uv run examples/tool_bridge.py` from `sdks/python` returns a clear config error when no API key is configured
- [x] Python SDK tests cover deterministic bridge tool execution through Rust runtime events without exposing scripted providers in public examples
