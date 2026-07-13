# Merry Roadmap

**Updated:** 2026-07-13

## Product Goal

Merry is a practical Rust-first agent runtime: provider-neutral, stream-first,
tool-capable, durable at stable boundaries, and usable from a terminal, Rust,
or Python without rebuilding runtime control flow in each frontend.

## Current Release Target

Stabilize the first multi-provider runtime release around this observable
workflow:

```text
user input
-> live model deltas
-> zero or more ordered tool calls
-> bounded safe execution and durable artifacts
-> ordered tool results
-> continued model stream
-> final output or typed terminal failure
```

Acceptance is offline by default and must include:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
(cd sdks/python && uv run --with pytest python -m pytest tests -q)
```

## Delivered

### Runtime

- Stream-first provider and runtime event contracts.
- Retry without buffering successful streams; retries stop after observable
  output commits.
- Session state, ledger, artifact references, context compilation, memory
  activation, checkpoints, cancellation, permission review, and resume storage.
- Structured final-output tools and typed completed, blocked, failed, and
  cancelled outcomes.
- Ordered non-empty tool-call batches admitted atomically.
- Explicit `ParallelSafe` and `Exclusive` tool execution contracts.
- Bounded parallel-safe waves, exclusive barriers, ordered continuation
  results, bridge-call matching, and cancellation handling.
- Durable recursive plans with runtime-owned revisions, approvals, attempts,
  leases, directives, retry/recovery policy, scoped concurrent subagents, lazy
  subtree expansion, exact result artifacts, and crash-safe resume.

### Providers

- OpenAI-compatible Responses streaming.
- OpenAI-compatible Chat Completions streaming, indexed tool-call assembly,
  usage-only terminal chunks, structured output, and content-filter mapping.
- Anthropic Messages streaming, content blocks, partial tool JSON, usage,
  structured output, and stop-reason mapping.
- Capability-aware `parallel_tool_calls = auto | enabled | disabled`.
- Named provider aliases and provider/model overrides for runtime roles.
- Sanitized bounded provider HTTP diagnostics.

### Product Surfaces

- Headless `merry run`, event JSONL, command planning, debug/smoke commands,
  and a full-screen interactive TUI.
- Automatic outer bubblewrap bootstrap for TUI, resume, and headless run, with
  an explicit `--no-sandbox` host-mode choice that keeps tool commands on the
  inner bubblewrap process runner.
- One chronological TUI timeline, real streaming deltas, compact tool rows,
  on-demand detail, responsive layouts, queues, completion, and resume picker.
- A responsive Plan cockpit with recursive navigation, node inspection,
  approval preview, steering and scheduler controls, and preserved selection
  across live updates at 50x20, 80x24, and 140x40.
- Magenta identity balanced with cyan tool information, green success, yellow
  warning/permission, red failure, white primary text, and gray metadata.
- Rust facade builders for OpenAI-compatible and Anthropic providers.
- Python async streams, interactive handles, bridge tools, structured final
  output, and both provider families.

## Next Active

### 1. Provider Conformance

Add deterministic local HTTP fixtures for the new protocols, covering request
headers, endpoint joining, SSE chunk boundaries, disconnects, cancellation,
rate-limit metadata, and secret redaction. Add explicit opt-in live Anthropic
and Chat Completions smoke commands after fixture coverage is complete.

Observable result: both providers pass the same conformance harness without
network access; live smokes remain excluded from default tests.

### 2. Configuration Hardening

Reject provider-specific keys on the wrong provider type, validate numeric
limits during config loading, improve alias diagnostics, and add a redacted
configuration inspection command.

Observable result: invalid TOML fails before runtime construction and the
tracked example remains a deterministic schema test.

### 3. Session And Batch Durability Audit

Verify savepoints after completed model turns and fully resolved batches,
interrupted batch cleanup, receiver closure, resume projection, and corrupted
session isolation.

Observable result: crash/restart tests prove no persisted session resumes with
a half-accepted or half-resolved tool batch.

### 4. TUI Interaction Cleanup

Replace the remaining artifact-specific navigation state with one explicit
input/review/detail mode, add paused-time redraw tests, and split the oversized
renderer/test modules along real ownership boundaries.

Observable result: keyboard and mouse transitions have one tested state table;
50x20, 80x24, and 140x40 buffers remain overlap-free.

### 5. Public API Stabilization

Reduce oversized protocol/provider modules, finish rustdoc examples, define
compatibility guarantees for serialized events/config, and prepare crate and
Python package release metadata.

Observable result: a versioned public surface can be consumed without importing
internal crates or provider wire types.

## Deferred

- OpenAI Realtime, Assistants, and provider-hosted conversation state.
- A universal abstraction for vendor-specific extensions.
- Automatic semantic inspection of shell arguments for concurrency safety.
- Unbounded parallel tool execution.
- Runtime-agnostic async executors.
- Deep Python callbacks from Rust runtime internals.
- A web or graphical UI.

## Known Limits

- The production CLI sandbox currently targets Linux `bubblewrap`.
- Live provider behavior depends on vendor compatibility and is intentionally
  outside the default deterministic suite.
- Anthropic and Chat Completions have parser/renderer coverage but still need
  the shared local HTTP conformance harness.
- TUI detail navigation still uses the existing artifact review controls while
  the unified interaction-mode cleanup remains next active work.
- Several mature source/test files exceed the preferred 1000-line guideline
  and should be split during public API stabilization, not mechanically.
