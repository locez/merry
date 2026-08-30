# Python SDK Capability Matrix

The Python package is a typed binding surface. The Rust facade/runtime remains
the source of truth for every durable or policy-sensitive capability.

| Capability | Python surface | Rust owner | Evidence |
| --- | --- | --- | --- |
| Provider construction | `OpenAICompatible`, `Anthropic`, `AgentBuilder.provider` | `merry::providers`, `merry::AgentBuilder` | `tests/test_builder.py` |
| Session identity | `Agent.session_id` | `merry::Agent` / `SessionId` | `tests/test_builder.py`, `tests/test_run.py` |
| Runtime events | immutable `Event` | `merry-core::RuntimeEvent`, runtime journal | `tests/test_run.py` |
| Event payload contract | `EventType` plus typed payload union; unknown events are retained as `UnknownEventPayload` | `merry-core::RuntimeEvent` serde contract | `merry/_event_parser.py`, `tests/test_events.py` |
| Streaming lifecycle | `AgentRun.next`, async iteration, `messages` | `merry::binding::OwnedAgentRun` | `tests/test_run.py` |
| Python tool decorator | `@builder.tool`, `@agent.tool`, `Tool` | Rust bridge tool registration; Python host registry for callables | `tests/test_tools.py` |
| Tool batch contract | `ToolCallBatch.submit`, `ToolRegistry.execute` | Rust batch lease, ordering, admission, artifacts | `tests/test_run.py`, `tests/test_tools.py`, `crates/merry/tests/agent.rs` |
| Tool domain failure | `ToolDomainError` -> `ToolResult` | Rust tool result/artifact continuation | `tests/test_tools.py` |
| Cancellation | `AgentRun.cancel`, `AgentRun.close`, task cancellation | Rust run cancellation and durable terminal result | `tests/test_run.py`, `crates/merry/tests/agent.rs` |
| Final report | `RunResult` | Rust `merry::RunResult` and runtime status/usage | `tests/test_run.py` |
| Structured output | `final_output_model`, `structured_output` | Rust `FinalOutputContract` and recorded artifact | `tests/test_run.py`, `crates/merry/tests/agent.rs` |
| Generation controls | Not exposed in this slice | Rust `GenerationConfig` | Deferred until a stable Python model maps the full facade contract |
| Structured-output retry policy | Not exposed in this slice | Rust `StructuredOutputRetryPolicy` | Deferred; Python does not duplicate retry state |
| Save/resume | `save_session`, `session_store`, `resume` | Rust `FileSessionStore` and runtime session state | `tests/test_run.py`, Rust facade agent tests |
| Multi-runtime orchestration | Compose independent `Agent` instances in host code | One Rust facade/runtime per session | `examples/multi_runtime_orchestration.py` |
| Workspace config | `WorkspaceConfig`, `WorkspaceLimits`, `PatchConfig` | `merry-coding`, `merry-tool-workspace` | `tests/test_builder.py` |
| Error parity | `MerryErrorInfo` and typed subclasses | `merry-core::MerryErrorInfo`, facade errors | `tests/test_errors.py`, `crates/merry-py/tests/bindings.rs` |
| Interactive controls | Not exposed in this T8 slice | `merry::InteractiveRun` | Deferred follow-up; no unsupported Python config |
| Process/session adapters | Not exposed in this T8 slice | `merry-process`, coding profile | Deferred follow-up; no unsupported Python config |

The deterministic Python suite must be run with the `test-utils` native feature
because its fake provider is intentionally excluded from production builds.
