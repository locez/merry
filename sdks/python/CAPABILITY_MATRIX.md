# Python SDK Capability Matrix

The Python package is a typed binding surface. The Rust facade/runtime remains
the source of truth for every durable or policy-sensitive capability.

| Capability | Python surface | Rust owner | Evidence |
| --- | --- | --- | --- |
| Provider construction | `OpenAICompatible`, `Anthropic`, `Agent(..., provider=...)`, `AgentBuilder.provider` | `merry::providers`, `merry::AgentBuilder` | `tests/test_builder.py` |
| Direct & fluent construction | `Agent(...)`, `await Agent.resume(...)`, `Agent.builder()` | `merry::AgentBuilder`, `merry::Agent` | `tests/test_builder.py`, `tests/test_run.py` |
| Prompt composition | `system_prompt`, `PromptProfile`, `PromptBlock` | `merry::PromptProfile`, `merry::PromptBlock` | `tests/test_builder.py` |
| Generation controls | `GenerationConfig`, `ParallelToolCalls`, `ServiceTier` | `merry::GenerationConfig` | `tests/test_builder.py` |
| Session identity | `Agent.session_id` | `merry::Agent` / `SessionId` | `tests/test_builder.py`, `tests/test_run.py` |
| Runtime events | immutable `Event`, `merry.events.*` typed payload union; unknown events retained as `UnknownEventPayload` | `merry-core::RuntimeEvent`, runtime journal | `merry/_event_parser.py`, `tests/test_events.py` |
| Streaming lifecycle | `async with agent.stream(...) as run:`, `AgentRun.next`, async iteration | `merry::binding::OwnedAgentRun` | `tests/test_run.py` |
| Interactive sessions | `async with agent.interactive() as session:`, `InteractiveSession` (`send`, `interrupt`, `save_session`, `close`), `session.queue` (`pause`, `resume`, `flush`, `snapshot`, `update`, `remove`) | `merry::binding::OwnedInteractiveRun`, `merry::InteractiveInput`, `merry::InteractiveControl` | `tests/test_interactive.py` |
| Python tool decorator | `@merry.tool`, `@builder.tool`, `Tool` (multi-parameter signatures or single `BaseModel` input; `BaseModel`/`str`/JSON outputs) | Rust bridge tool registration; Python host registry for callables | `tests/test_tools.py` |
| Parallel & barrier tool execution | `Tool(parallel_safe=...)`, `ToolRegistry.execute`, `Agent.execute_tool_batch` | Rust batch lease, ordering, admission, tool outputs; Python wave execution | `tests/test_tools.py`, `tests/test_run.py` |
| Tool domain failure | `ToolDomainError` -> `ToolResult` | Rust tool result/output continuation | `tests/test_tools.py` |
| Cancellation | `AgentRun.cancel`, `AgentRun.close`, `async with`, task cancellation | Rust run cancellation and durable terminal result | `tests/test_run.py`, `crates/merry/tests/agent.rs` |
| Final report | `RunResult` | Rust `merry::RunResult` and runtime status/usage | `tests/test_run.py` |
| Structured output | `final_output_model`, `structured_output` | Rust `FinalOutputContract` and recorded output | `tests/test_run.py`, `crates/merry/tests/agent.rs` |
| Structured-output retry policy | Not exposed yet | Rust `StructuredOutputRetryPolicy` | Deferred; Python does not duplicate retry state |
| Save/resume | `save_session`, `session_store`, `await Agent.resume(...)` | Rust `FileSessionStore` (`journal.jsonl`) and runtime session state | `tests/test_run.py`, `tests/test_interactive.py` |
| Multi-runtime orchestration | Compose independent `Agent` instances in host code | One Rust facade/runtime per session | `examples/multi_runtime_orchestration.py` |
| Workspace config | `WorkspaceConfig`, `WorkspaceLimits`, `PatchConfig` | `merry-coding`, `merry-tools` | `tests/test_builder.py`, `tests/test_workspace_limits.py` |
| Error parity | `MerryErrorInfo` and typed subclasses in `merry.errors` | `merry-core::MerryErrorInfo`, facade errors | `tests/test_errors.py`, `crates/merry-py/tests/bindings.rs` |
| Process/session adapters | Not exposed yet | `merry-process`, coding profile | Deferred follow-up; no unsupported Python config |

The deterministic Python suite must be run with the `test-utils` native feature
because its fake provider is intentionally excluded from production builds.
