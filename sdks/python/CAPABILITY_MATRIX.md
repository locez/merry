# Python SDK Capability Matrix

The Python package is a typed binding surface. The Rust facade/runtime remains
the source of truth for every durable or policy-sensitive capability.

| Capability | Python surface | Rust owner | Evidence |
| --- | --- | --- | --- |
| Provider construction | `OpenAICompatible`, `Anthropic` (including `context_window_tokens`), `Agent(..., provider=...)`, `AgentBuilder.provider` | `merry::providers`, `merry::AgentBuilder`, `ModelCapabilities` | `tests/test_builder.py`, `tests/test_context_and_hooks.py` |
| Context window & compaction | `context_window_tokens`, `CompactionConfig`, `InteractiveSession.update_settings`, `InteractiveSession.compact`, `CompactionOutcome` | `merry::AgentBuilder`, `RuntimeBuilder`, `InteractiveControl` | `tests/test_context_and_hooks.py` |
| Lifecycle hooks & turn control | `Hooks`, `merry.hooks.*` (`before_model_step`, `after_model_step`, `before_tool_call`, `after_tool_call`), `StopTurn`/`Continue`/`Modify`/`Deny`/`ShortCircuit`, `auto_hooks`/`auto_tools`, built-in strategy helpers | `merry_runtime::hooks`, `HookSubscriptions`, `AgentLoopStatus::StoppedByHook` (zero bridge traffic when unsubscribed) | `tests/test_context_and_hooks.py`, `crates/merry/tests/hooks_and_context.rs` |
| Direct & fluent construction | `Agent(...)`, `await Agent.resume(...)`, `Agent.builder()` | `merry::AgentBuilder`, `merry::Agent` | `tests/test_builder.py`, `tests/test_run.py` |
| Prompt composition | `system_prompt`, `PromptProfile`, `PromptBlock` | `merry::PromptProfile`, `merry::PromptBlock` | `tests/test_builder.py` |
| Generation controls | `GenerationConfig`, `ParallelToolCalls`, `ServiceTier` | `merry::GenerationConfig` | `tests/test_builder.py` |
| Session identity | `Agent.session_id` | `merry::Agent` / `SessionId` | `tests/test_builder.py`, `tests/test_run.py` |
| Runtime events | immutable `Event`, `merry.events.*` typed payload union; unknown events retained as `UnknownEventPayload` | `merry-core::RuntimeEvent`, runtime journal | `merry/_event_parser.py`, `tests/test_events.py` |
| Streaming lifecycle | `async with agent.stream(..., auto_hooks=..., auto_tools=...) as run:`, `AgentRun.next`, async iteration | `merry::binding::OwnedAgentRun` | `tests/test_run.py`, `tests/test_context_and_hooks.py` |
| Interactive sessions | `async with agent.interactive(auto_hooks=..., auto_tools=...) as session:`, `InteractiveSession` (`send`, `interrupt`, `update_settings`, `compact`, `save_session`, `close`), `session.queue` (`pause`, `resume`, `flush`, `snapshot`, `update`, `remove`) | `merry::binding::OwnedInteractiveRun`, `merry::InteractiveInput`, `merry::InteractiveControl` | `tests/test_interactive.py`, `tests/test_context_and_hooks.py` |
| Python tool decorator | `@merry.tool`, `@builder.tool`, `Tool` (multi-parameter signatures or single `BaseModel` input; `BaseModel`/`str`/JSON outputs) | Rust bridge tool registration; Python host registry for callables | `tests/test_tools.py` |
| Parallel & barrier tool execution | `Tool(parallel_safe=...)`, `ToolRegistry.execute`, `Agent.execute_tool_batch` | Rust batch lease, ordering, admission, tool outputs; Python wave execution | `tests/test_tools.py`, `tests/test_run.py` |
| Tool domain failure | `ToolDomainError` -> `ToolResult` | Rust tool result/output continuation | `tests/test_tools.py` |
| Cancellation | `AgentRun.cancel`, `AgentRun.close`, `async with`, task cancellation | Rust run cancellation and durable terminal result | `tests/test_run.py`, `crates/merry/tests/agent.rs` |
| Final report | `RunResult`, `RunStatus.STOPPED_BY_HOOK`, `HookStopInfo` | Rust `merry::RunResult` and runtime status/usage | `tests/test_run.py`, `tests/test_context_and_hooks.py` |
| Structured output | `final_output_model`, `structured_output` | Rust `FinalOutputContract` and recorded output | `tests/test_run.py`, `crates/merry/tests/agent.rs` |
| Structured-output retry policy | `StructuredOutputRetryPolicy`, `Agent(..., structured_output_retry_policy=...)`, `AgentBuilder.structured_output_retry_policy` | Rust `StructuredOutputRetryPolicy` | `tests/test_context_and_hooks.py` |
| Save/resume & session lock | `save_session`, `session_store`, `session_lock`, `async with Agent(...)` / `async with Agent.resume(...)` / `await Agent.resume(...)`, `Agent.close`, `MerrySessionLockedError` | Rust `FileSessionStore` (`journal.jsonl`), `SessionReservation` (`.session.lock`), `Runtime::close` | `tests/test_run.py`, `tests/test_interactive.py`, `tests/test_session_lock.py` |
| Multi-runtime orchestration | Compose independent `Agent` instances in host code | One Rust facade/runtime per session | `examples/multi_runtime_orchestration.py` |
| Workspace config | `WorkspaceConfig`, `WorkspaceLimits`, `PatchConfig` | `merry-coding`, `merry-tools` | `tests/test_builder.py`, `tests/test_workspace_limits.py` |
| Error parity | `MerryErrorInfo` and typed subclasses in `merry.errors` | `merry-core::MerryErrorInfo`, facade errors | `tests/test_errors.py`, `crates/merry-py/tests/bindings.rs` |
| Process/session adapters | Not exposed yet | `merry-process`, coding profile | Deferred follow-up; no unsupported Python config |

The deterministic Python suite must be run with the `test-utils` native feature
because its fake provider is intentionally excluded from production builds.
