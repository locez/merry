# Merry Roadmap

**Updated:** 2026-07-15

## Product Goal

Merry should become a coding assistant that its maintainer is willing to use
for daily coding work. The runtime remains Rust-first and provider-neutral, but
the product goal is now measured by the quality of one continuous coding loop:

```text
open or resume a session
-> enter a task with low-friction controls
-> understand the current workspace and runtime state
-> inspect, plan, request permission, and use tools
-> review meaningful tool output and changes
-> run checks and continue the task
-> recover from interruption without losing trusted progress
```

The project is not complete when the individual runtime capabilities exist.
They must be discoverable, understandable, recoverable, and comfortable enough
to replace the maintainer's usual coding workflow.

## Priority Reset

On 2026-07-15, the user explicitly reset the active product priority after
evaluating the current project as a usable toy that is not yet preferred for
coding. This supersedes the previous ordering that led with provider
conformance, configuration hardening, and public API release polish.

Those tracks remain useful supporting work, but they must not displace the
daily-coding acceptance target unless they directly unblock it.

## Current Release Target

Make one real multi-turn coding workflow trustworthy in the TUI, Rust API, and
Python SDK:

```text
user input
-> current-state-aware model request
-> meaningful tool and permission feedback
-> bounded workspace changes
-> test and review loop
-> continued or interrupted session
-> durable recovery at a known boundary
```

Acceptance is offline by default and must include both product scenarios and
repository checks:

```text
1. Start a new TUI session and complete a multi-turn coding task.
2. Use input history, slash commands, tool feedback, and permission controls
   without memorizing internal runtime details.
3. Interrupt or terminate the session at a stable boundary and resume it with
   the completed work intact.
4. Run the same deterministic coding scenarios through Rust and Python
   surfaces and compare their normalized outcomes.
```

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
- Idle-boundary interactive session save through `AgentLoopControl::save_session_to`
  with `SessionSaveRequiresIdle` rejection while a model, tool, or interrupt phase
  is active.
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
  on-demand detail, responsive layouts, queues, completion, resume picker,
  slash commands (`/help`, `/status`, `/save`, `/stop`) shared with the command
  palette, and workspace-scoped persisted text input history.
- A responsive Plan cockpit with recursive navigation, node inspection,
  approval preview, steering and scheduler controls, and preserved selection
  across live updates at 50x20, 80x24, and 140x40.
- Magenta identity balanced with cyan tool information, green success, yellow
  warning/permission, red failure, white primary text, and gray metadata.
- Rust facade builders for OpenAI-compatible and Anthropic providers.
- Python async streams, interactive handles, bridge tools, structured final
  output, and both provider families.

## Next Active

### 1. TUI Daily Coding Flow

Make the TUI comfortable for repeated coding sessions rather than merely
feature-complete:

- ~~Add shared text-only input history across sessions, including resumed and
  newly created sessions. Do not persist image attachments in this history.~~
  Delivered: workspace-scoped JSONL history with SHA256-hashed filenames,
  atomic temp+rename writes, in-memory normalization, and cross-session reuse.
- ~~Add slash commands with one command registry shared by slash completion and
  the existing command palette.~~ Delivered: unified `CommandSpec` registry
  drives both the command palette and `/`-triggered completion; `/help`,
  `/status`, `/save`, `/stop` execute locally without sending model messages.
- ~~Make common tools render meaningful summaries: operation, target, status,
  result, and failure reason. Generic fallback rendering must not reduce useful
  information to output such as `args=3`.~~ Delivered: failed tool rows replace
  their pending timeline row with a `-> failed` status and bounded diagnostic
  body; process, patch, read, list, and permission tools render dedicated
  previews.
- Keep plan, permission, save, interrupt, resume, retry, and discard states
  visible and actionable from the same control surface.

Observable result: a user can open a new or resumed session, recall previous
text input, invoke `/help`, `/status`, `/save`, `/stop`, and related commands,
and understand a tool operation without opening raw argument details.

### 2. Coding Harness, Prompt, And Runtime Context

Build a deterministic coding harness around the prompt and tool contract, not
only around provider response parsing:

- Audit every coding tool description for purpose, preconditions, side effects,
  input meaning, failure behavior, and when to choose a different tool.
- Define the initial context contract for workspace identity, current task,
  relevant runtime state, available tools, plan/permission state, and resume
  state. Inject dynamic state after stable instructions and tool definitions
  where the context contract allows it.
- Make the current state explicit enough that the model does not have to infer
  initialization state or hidden runtime state from empty history.
- Add deterministic coding scenarios that capture request context, tool calls,
  permission decisions, artifacts, workspace changes, failures, and final
  state. Reuse those scenarios across the Rust and Python surfaces where
  practical.

Observable result: representative coding scenarios select the intended tools,
respect the current workspace and runtime state, and produce inspectable,
replayable traces without a live provider.

### 3. Incremental Session Resume

Replace exit-only full snapshot persistence with a local incremental store:

- SQLite append-only durable events for stable runtime boundaries;
- periodic materialized snapshots and event-tail replay;
- artifact metadata and references without repeating large contents in every
  snapshot;
- automatic save at completed turns, resolved tool boundaries, plan changes,
  and permission decisions, without persisting every streaming delta;
- visible save state, failure feedback, and an explicit recovery choice for
  interrupted model or tool work;
- import and compatibility for existing file-backed sessions.

Observable result: completed coding work survives a process crash, an
interrupted operation resumes as an explicit interrupted state, and the user
can tell which state is durably saved. The store design must preserve tool
side-effect boundaries and must not silently repeat an uncertain external
operation.

### 4. Python SDK Product Surface

Make the Python SDK feel like a coherent public SDK rather than a thin binding
probe:

- provide a small first-run API for run, stream, interactive control, tools,
  cancellation, and errors;
- improve event types and typing so callers do not need to branch on untyped
  `dict[str, Any]` payloads for normal flows;
- keep Rust as the runtime owner while making bridge tools, backpressure,
  stream closure, and error recovery predictable in asyncio;
- align Python event and control semantics with the Rust and TUI scenarios;
- document the supported lifecycle, persistence expectations, and production
  packaging path.

Observable result: a new Python user can build a streaming coding assistant
from the public package, handle tool and error events without inspecting native
details, and use the same deterministic scenarios as the Rust facade.

### 5. Small, Stable Rust Agent API

Extract a pleasant high-level Rust facade above the capable but low-level
`RuntimeBuilder` surface:

- provide a simple agent/coding-agent construction path for provider, profile,
  tools, and session setup;
- make common `run`, `stream`, interactive control, final output, and error
  flows obvious from rustdoc examples;
- keep advanced runtime, journal, plan, permission, and artifact APIs available
  for power users without making them mandatory for the common path;
- preserve provider-neutral types and avoid leaking provider wire formats.

Observable result: the README's Rust example and a focused public API test can
construct and run a coding agent without importing internal crates or manually
assembling unrelated runtime components.

## Supporting Work

The following work remains valid, but is subordinate to the daily coding goal:

- provider conformance fixtures and opt-in live smokes;
- configuration validation and redacted diagnostics;
- source/test module splitting and public API compatibility metadata;
- provider-specific improvements that directly remove a failure in the coding
  scenarios.

Supporting work should be pulled into `Next Active` only when it unblocks a
named coding workflow or acceptance test.

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
- TUI session persistence is still primarily exit-time full snapshot saving;
  incremental SQLite durability is not implemented.
- TUI slash-command control plane, shared cross-session text input history,
  and consistently useful generic tool rendering have been delivered; the
  remaining TUI Daily Coding Flow work is unified state visibility.
- Coding-tool descriptions and initial runtime context still need a deliberate
  harness-driven audit.
- Python sessions are ephemeral by default and Python event surfaces still need
  a more stable typed contract.
- The Rust public facade remains more powerful than ergonomic for common coding
  application setup.
- Several mature source/test files exceed the preferred 1000-line guideline
  and should be split when doing the active product work, not mechanically.
