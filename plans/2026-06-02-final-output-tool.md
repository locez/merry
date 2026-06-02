# Final Output Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Do not dispatch subagents unless the parent can guarantee the user-required model/effort. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a runtime-owned synthetic final-output tool so agent loops can terminate with structured JSON/Pydantic data without forcing strict structured response format on every model step.

**Architecture:** Merry exposes a reserved provider-visible tool named `merry_final_output` when a final output contract is active. The model may call ordinary tools first; when it calls `merry_final_output`, runtime records the arguments as a JSON artifact and completes the agent loop instead of executing a normal tool. Python SDK validates that final JSON into the caller's Pydantic model.

**Tech Stack:** Rust 2024, `merry-core`, `merry-runtime`, `merry-llm`, `merry-py` PyO3, Python Pydantic v2.

---

## Scope

Implement only final-output tool mode:

```python
result = await runtime.run(task, final_output_model=FinalAnswer)
assert isinstance(result.final_output, FinalAnswer)
```

Do not implement strict response-format mode in this plan:

```python
await runtime.run_structured_once(task, output_model=FinalAnswer)
```

That mode is intentionally separate because it changes model tool-use behavior.

## File Structure

- Modify `crates/merry-core/src/event.rs`
  - Add an observable event for final output artifact recording.
- Modify `crates/merry-runtime/src/final_output.rs`
  - New focused module for reserved final-output tool name, contract construction, and final-output result type.
- Modify `crates/merry-runtime/src/lib.rs`
  - Export final-output contract/result types.
- Modify `crates/merry-runtime/src/agent_loop.rs`
  - Carry final-output config in `AgentLoopConfig`.
  - Complete loop when pending call is the final-output tool.
  - Block if final-output contract is active but model completes with text.
- Modify `crates/merry-runtime/src/step.rs`
  - Carry final-output tool spec through `StepContext`.
- Modify `crates/merry-runtime/src/runtime.rs`
  - Include final-output tool spec in provider request tools.
  - Add durable final-output artifact/event recording.
- Modify `crates/merry-runtime/tests/agent_loop.rs`
  - Add deterministic final-output tool tests.
- Modify `crates/merry-core/tests/protocol.rs`
  - Add event protocol serialization test.
- Modify `crates/merry-py/src/runtime.rs`
  - Accept final-output schema JSON for `run_blocking`.
  - Return final-output JSON to Python.
- Modify `sdks/python/merry/_runtime.py`
  - Add `Runtime.run(..., final_output_model=...)`.
  - Validate final-output JSON into the Pydantic model.
- Modify `sdks/python/tests/test_production_sdk.py`
  - Add Python final-output model tests.
- Modify `sdks/python/README.md`
  - Document final-output tool mode and side effects.

## Task 1: Core Event Contract

**Files:**
- Modify: `crates/merry-core/src/event.rs`
- Modify: `crates/merry-core/tests/protocol.rs`

- [ ] **Step 1: Write failing protocol test**

Add a test in `crates/merry-core/tests/protocol.rs`:

```rust
#[test]
fn final_output_recorded_event_uses_artifact_ref_without_payload() {
    let event = RuntimeEvent::new(
        SessionId::new("final-output-session").unwrap(),
        3,
        RuntimeEventKind::FinalOutputRecorded {
            call_id: ToolCallId::new("call-final").unwrap(),
            artifact: ArtifactRef::new(
                ArtifactId::new("final-output-3").unwrap(),
                ArtifactKind::Json,
            ),
        },
    );

    let value = serde_json::to_value(&event).unwrap();

    assert_eq!(value["kind"]["type"], json!("final_output_recorded"));
    assert_eq!(value["kind"]["call_id"], json!("call-final"));
    assert_eq!(value["kind"]["artifact"]["id"], json!("final-output-3"));
    assert_eq!(value["kind"]["artifact"]["kind"], json!("json"));
    assert!(value["kind"].get("content").is_none());

    let decoded: RuntimeEvent = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, event);
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p merry-core final_output_recorded_event_uses_artifact_ref_without_payload
```

Expected: fail because `RuntimeEventKind::FinalOutputRecorded` does not exist.

- [ ] **Step 3: Implement event variant**

Add to `RuntimeEventKind` in `crates/merry-core/src/event.rs`:

```rust
/// A runtime-owned final-output tool call recorded structured terminal output.
FinalOutputRecorded {
    /// Provider-originated call id for the final-output tool call.
    call_id: ToolCallId,
    /// JSON artifact containing the final structured output.
    artifact: ArtifactRef,
},
```

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p merry-core final_output_recorded_event_uses_artifact_ref_without_payload
```

Expected: pass.

## Task 2: Runtime Final Output Contract

**Files:**
- Create: `crates/merry-runtime/src/final_output.rs`
- Modify: `crates/merry-runtime/src/lib.rs`
- Test: `crates/merry-runtime/src/final_output.rs`

- [ ] **Step 1: Write failing unit tests**

Create `crates/merry-runtime/src/final_output.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::ToolInputSchema;
    use schemars::Schema;
    use serde_json::json;

    fn schema(value: serde_json::Value) -> Schema {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn final_output_contract_uses_reserved_provider_portable_tool_name() {
        let contract = FinalOutputContract::new(
            ToolInputSchema::new(schema(json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Short final summary."
                    }
                },
                "required": ["summary"],
                "additionalProperties": false
            }))).unwrap(),
        ).unwrap();

        assert_eq!(contract.tool_name().as_str(), "merry_final_output");
        assert_eq!(contract.tool_spec().name().as_str(), "merry_final_output");
        assert!(contract.tool_spec().description().contains("final structured output"));
    }

    #[test]
    fn final_output_contract_rejects_schema_without_field_descriptions() {
        let error = FinalOutputContract::new(
            ToolInputSchema::new(schema(json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" }
                },
                "required": ["summary"],
                "additionalProperties": false
            }))).unwrap(),
        ).unwrap_err();

        assert_eq!(error.to_string(), "final output schema field summary must include a description");
    }
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p merry-runtime final_output_contract_
```

Expected: fail because `FinalOutputContract` does not exist.

- [ ] **Step 3: Implement minimal contract**

Implement in `crates/merry-runtime/src/final_output.rs`:

```rust
use merry_core::{CoreError, ToolInputSchema, ToolName, ToolSpec};
use serde_json::Value;
use thiserror::Error;

pub const FINAL_OUTPUT_TOOL_NAME: &str = "merry_final_output";

#[derive(Debug, Clone, PartialEq)]
pub struct FinalOutputContract {
    tool_spec: ToolSpec,
}

impl FinalOutputContract {
    pub fn new(schema: ToolInputSchema) -> Result<Self, FinalOutputContractError> {
        validate_schema_field_descriptions(schema.as_schema().as_value())?;
        let tool_spec = ToolSpec::new(
            ToolName::new(FINAL_OUTPUT_TOOL_NAME).map_err(FinalOutputContractError::Core)?,
            "Submit the final structured output when the task is complete.",
            schema,
        )
        .map_err(FinalOutputContractError::Core)?;
        Ok(Self { tool_spec })
    }

    pub fn tool_name(&self) -> &ToolName {
        self.tool_spec.name()
    }

    pub fn tool_spec(&self) -> &ToolSpec {
        &self.tool_spec
    }
}

#[derive(Debug, Error)]
pub enum FinalOutputContractError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("final output schema field {field} must include a description")]
    MissingFieldDescription { field: String },
}

fn validate_schema_field_descriptions(value: &Value) -> Result<(), FinalOutputContractError> {
    let Some(properties) = value.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    for (field, schema) in properties {
        let has_description = schema
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| !description.trim().is_empty());
        if !has_description {
            return Err(FinalOutputContractError::MissingFieldDescription {
                field: field.clone(),
            });
        }
    }
    Ok(())
}
```

Export from `crates/merry-runtime/src/lib.rs`:

```rust
pub mod final_output;
pub use final_output::{FinalOutputContract, FinalOutputContractError, FINAL_OUTPUT_TOOL_NAME};
```

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p merry-runtime final_output_contract_
```

Expected: pass.

## Task 3: Expose Final Output Tool To Provider Requests

**Files:**
- Modify: `crates/merry-runtime/src/step.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Test: `crates/merry-runtime/src/runtime.rs`

- [ ] **Step 1: Write failing provider-request test**

Add a runtime unit test near provider request/tool compilation tests:

```rust
#[tokio::test(flavor = "current_thread")]
async fn final_output_contract_adds_reserved_tool_to_provider_request() {
    let provider = RecordingModelProvider::new(vec![ScriptedModelProviderResponse::text("done")]);
    let requests = provider.requests();
    let contract = final_output_contract(json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Short final summary."
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    }));
    let runtime = Runtime::builder(session_id("final-output-request"))
        .model_provider(Arc::new(provider), model_name("fake"))
        .build()
        .unwrap();

    let _events = collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Return a final summary.").unwrap(),
        StepContext::default().with_final_output_contract(contract),
    ).await.unwrap();

    let requests = requests.lock().unwrap();
    assert!(requests[0].tools().iter().any(|tool| tool.name().as_str() == "merry_final_output"));
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p merry-runtime final_output_contract_adds_reserved_tool_to_provider_request
```

Expected: fail because `StepContext::with_final_output_contract` does not exist.

- [ ] **Step 3: Add final-output contract to StepContext**

Modify `crates/merry-runtime/src/step.rs`:

```rust
use crate::FinalOutputContract;

#[derive(Debug, Clone)]
pub struct StepContext {
    cancellation_token: CancellationToken,
    generation_config: GenerationConfig,
    final_output_contract: Option<FinalOutputContract>,
}

impl StepContext {
    pub fn with_final_output_contract(mut self, contract: FinalOutputContract) -> Self {
        self.final_output_contract = Some(contract);
        self
    }

    pub(crate) fn final_output_contract(&self) -> Option<&FinalOutputContract> {
        self.final_output_contract.as_ref()
    }
}
```

- [ ] **Step 4: Include synthetic tool in provider request**

Where runtime compiles provider request tools, append:

```rust
if let Some(contract) = context.final_output_contract() {
    tools.push(contract.tool_spec().clone());
}
```

Reject collisions before request construction:

```rust
if registry.contains(contract.tool_name()) {
    return Err(RuntimeError::ReservedFinalOutputToolName);
}
```

- [ ] **Step 5: Verify GREEN**

Run:

```bash
cargo test -p merry-runtime final_output_contract_adds_reserved_tool_to_provider_request
```

Expected: pass.

## Task 4: Agent Loop Terminal Final Output

**Files:**
- Modify: `crates/merry-runtime/src/agent_loop.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/src/session.rs`
- Modify: `crates/merry-runtime/src/ledger.rs`
- Test: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Write failing test for direct final output**

Add in `crates/merry-runtime/tests/agent_loop.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_completes_when_model_calls_final_output_tool() {
    let provider = scripted_tool_call_provider(
        "merry_final_output",
        json!({"summary": "Order A123 shipped."}),
        "unreachable",
    );
    let contract = final_output_contract(json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Short final summary."
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    }));
    let runtime = Runtime::builder(session_id("final-output-loop"))
        .model_provider(Arc::new(provider), model_name("fake"))
        .build()
        .unwrap();

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured final output.").unwrap(),
            StepContext::default(),
            AgentLoopConfig::default().with_final_output_contract(contract),
        )
        .await
        .unwrap();

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output_json().unwrap()["summary"], json!("Order A123 shipped."));
    assert!(result.events().iter().any(|event| matches!(
        event.kind(),
        RuntimeEventKind::FinalOutputRecorded { .. }
    )));
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p merry-runtime --test agent_loop agent_loop_completes_when_model_calls_final_output_tool
```

Expected: fail because agent loop treats `merry_final_output` as a normal pending tool.

- [ ] **Step 3: Extend AgentLoopConfig**

Modify `AgentLoopConfig`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLoopConfig {
    max_steps: NonZeroUsize,
    final_output_contract: Option<FinalOutputContract>,
}

impl AgentLoopConfig {
    pub fn with_final_output_contract(mut self, contract: FinalOutputContract) -> Self {
        self.final_output_contract = Some(contract);
        self
    }

    pub fn final_output_contract(&self) -> Option<&FinalOutputContract> {
        self.final_output_contract.as_ref()
    }
}
```

Remove `Copy` from `AgentLoopConfig` because it now can own a contract.

- [ ] **Step 4: Carry final-output contract into each step**

When building `step_context` inside `run_agent_loop`, add:

```rust
let mut step_context = StepContext::new(loop_token.clone())
    .with_generation_config(generation_config.clone());
if let Some(contract) = config.final_output_contract().cloned() {
    step_context = step_context.with_final_output_contract(contract);
}
```

- [ ] **Step 5: Record final output artifact and event**

Add a runtime method:

```rust
pub async fn record_final_output(
    &self,
    call: PendingToolCall,
) -> Result<(serde_json::Value, Vec<RuntimeEvent>), RuntimeError>
```

Behavior:

- Serialize `call.arguments()` as a JSON object.
- Record artifact id `final-output-{sequence}` with `ArtifactKind::Json`.
- Emit `ArtifactRecorded`.
- Emit `FinalOutputRecorded { call_id, artifact }`.
- Return the JSON object and emitted events.

- [ ] **Step 6: Complete on final-output pending call**

In `StepOutcome::Pending(call)` branch before normal tool execution:

```rust
if config
    .final_output_contract()
    .is_some_and(|contract| call.name() == contract.tool_name())
{
    let (json, mut final_events) = self.record_final_output(call).await?;
    events.append(&mut final_events);
    trace_loop_finish(self.session_id().as_str(), "completed", steps_run, None);
    return Ok(AgentLoopResult::new(
        AgentLoopStatus::Completed,
        events,
        steps_run,
        FinalOutput::Json(json),
    ));
}
```

Represent final output as:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum FinalOutput {
    Text(String),
    Json(serde_json::Value),
}
```

Keep `final_output()` as text-only compatibility:

```rust
pub fn final_output(&self) -> Option<&str> {
    match &self.final_output {
        Some(FinalOutput::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

pub fn final_output_json(&self) -> Option<&serde_json::Value> {
    match &self.final_output {
        Some(FinalOutput::Json(value)) => Some(value),
        _ => None,
    }
}
```

- [ ] **Step 7: Block text completion when structured final output is required**

Add blocked reason:

```rust
ExpectedFinalOutputToolCall
```

In `StepOutcome::Completed`, when `config.final_output_contract().is_some()` and no final-output tool was called, return blocked with that reason instead of accepting text.

- [ ] **Step 8: Verify direct final output**

Run:

```bash
cargo test -p merry-runtime --test agent_loop agent_loop_completes_when_model_calls_final_output_tool
```

Expected: pass.

## Task 5: Tool-Then-Final Flow

**Files:**
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Write failing integration test**

Add:

```rust
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_can_call_business_tool_before_final_output_tool() {
    let provider = scripted_two_step_tool_provider(vec![
        ("lookup_order", json!({"order_id": "A123"})),
        ("merry_final_output", json!({"summary": "Order A123 shipped."})),
    ]);
    let contract = final_output_contract(json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Short final summary."
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    }));
    let runtime = Runtime::builder(session_id("tool-then-final-output"))
        .model_provider(Arc::new(provider), model_name("fake"))
        .register_tool(RegisteredTool::read_only(
            tool_spec("lookup_order"),
            Arc::new(OkJsonExecutor::new(json!({"status": "shipped"}))),
        ))
        .build()
        .unwrap();

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Look up order A123 and return structured output.").unwrap(),
            StepContext::default(),
            AgentLoopConfig::default().with_final_output_contract(contract),
        )
        .await
        .unwrap();

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output_json().unwrap()["summary"], json!("Order A123 shipped."));
    assert!(event_types(result.events()).contains(&"ToolCallResolved"));
    assert!(event_types(result.events()).contains(&"FinalOutputRecorded"));
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p merry-runtime --test agent_loop agent_loop_can_call_business_tool_before_final_output_tool
```

Expected: fail until helper/provider and final-output branch are complete.

- [ ] **Step 3: Add deterministic scripted provider helper**

Use existing scripted provider helpers in `agent_loop.rs` as the pattern. The helper must:

- Return `lookup_order` tool call on first request.
- Return `merry_final_output` tool call on second request after tool continuation exists.
- Record requests for assertions if useful.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p merry-runtime --test agent_loop agent_loop_can_call_business_tool_before_final_output_tool
```

Expected: pass.

## Task 6: Python Binding Surface

**Files:**
- Modify: `crates/merry-py/src/runtime.rs`
- Modify: `sdks/python/merry/_runtime.py`
- Test: `sdks/python/tests/test_production_sdk.py`

- [ ] **Step 1: Write failing Python test**

Add:

```python
class FinalAnswer(BaseModel):
    model_config = ConfigDict(extra="forbid")

    summary: str = Field(description="Short final summary.")


async def _assert_runtime_run_returns_pydantic_final_output():
    runtime = runtime_with_scripted_final_output(
        arguments={"summary": "Order A123 shipped."},
    )

    result = await runtime.run(
        "Return structured final output.",
        final_output_model=FinalAnswer,
    )

    assert result.status == "completed"
    assert result.final_output == FinalAnswer(summary="Order A123 shipped.")


def test_runtime_run_returns_pydantic_final_output():
    asyncio.run(_assert_runtime_run_returns_pydantic_final_output())
```

- [ ] **Step 2: Run Python test to verify RED**

Run:

```bash
UV_CACHE_DIR=/tmp/merry-uv-cache uv run --with pytest python -m pytest tests/test_production_sdk.py::test_runtime_run_returns_pydantic_final_output -q
```

Expected: fail because `Runtime.run()` has no `final_output_model` parameter.

- [ ] **Step 3: Update native result shape**

In `crates/merry-py/src/runtime.rs`, include:

```rust
dict.set_item("final_output_json", result.final_output_json())?;
```

Keep existing `final_output` for text output.

- [ ] **Step 4: Accept final-output schema JSON**

Change native `run_blocking` signature:

```rust
fn run_blocking(
    &self,
    py: Python<'_>,
    task: String,
    final_output_schema_json: Option<String>,
) -> PyResult<Py<PyAny>>
```

Build `AgentLoopConfig`:

```rust
let mut config = AgentLoopConfig::default();
if let Some(schema_json) = final_output_schema_json {
    let schema = parse_schema(&schema_json)?;
    let contract = FinalOutputContract::new(ToolInputSchema::new(schema)?)?;
    config = config.with_final_output_contract(contract);
}
```

- [ ] **Step 5: Validate final output in Python**

In `sdks/python/merry/_runtime.py`:

```python
@dataclass(frozen=True)
class RunResult:
    status: str
    steps_run: int
    final_output: object | None
    events: list[dict[str, Any]]
```

Add to `Runtime.run`:

```python
async def run(
    self,
    task: str,
    *,
    final_output_model: type[BaseModel] | None = None,
) -> RunResult:
```

When model is provided:

```python
_validate_pydantic_model(final_output_model, "final_output_model")
schema_json = json.dumps(final_output_model.model_json_schema(), sort_keys=True)
raw = self._native.run_blocking(task, schema_json)
final_output = final_output_model.model_validate(raw["final_output_json"])
```

- [ ] **Step 6: Verify Python GREEN**

Run:

```bash
UV_CACHE_DIR=/tmp/merry-uv-cache uv run --with pytest python -m pytest tests/test_production_sdk.py::test_runtime_run_returns_pydantic_final_output -q
```

Expected: pass.

## Task 7: Documentation And Guardrails

**Files:**
- Modify: `sdks/python/README.md`
- Modify: `plans/2026-06-02-final-output-tool.md`

- [ ] **Step 1: Document final-output tool mode**

Add README section:

```md
## Structured Final Output

`final_output_model` uses a runtime-owned final-output tool. It does not use
strict response format on the first model request, so the model can call normal
tools before submitting the final structured result.
```

Show:

```python
class FinalAnswer(BaseModel):
    model_config = ConfigDict(extra="forbid")

    summary: str = Field(description="Short final summary.")

result = await runtime.run(
    "Look up order A123 and return structured data.",
    final_output_model=FinalAnswer,
)
```

- [ ] **Step 2: Document side effects and non-goal**

Add:

```md
This is different from strict structured response format. Strict structured
format can bias a model toward answering immediately instead of calling tools.
Merry's `final_output_model` exposes a reserved final-output tool and treats
that tool call as the terminal structured answer.
```

- [ ] **Step 3: Verify docs examples import**

Run:

```bash
UV_CACHE_DIR=/tmp/merry-uv-cache uv run examples/basic_runtime.py
UV_CACHE_DIR=/tmp/merry-uv-cache uv run examples/tool_bridge.py
```

Expected without secrets: both return clear config errors, not import errors.

## Task 8: Full Verification

- [ ] **Step 1: Run Rust checks**

Run:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: all pass.

- [ ] **Step 2: Run Python checks**

Run:

```bash
UV_CACHE_DIR=/tmp/merry-uv-cache uv run --with pytest python -m pytest tests -q
```

from `sdks/python`.

Expected: all pass.

- [ ] **Step 3: Record verification**

Append the successful command list and any known caveats to this plan under a `Completed verification` section.
