# Event-First Interactive CLI Design

Date: 2026-05-23

## Purpose

Merry has proven that deterministic, sandboxed, and live-provider tool calls can
complete the minimal coding loop. The next milestone should make that loop
usable and inspectable by a human operator. The goal is not to build a polished
TUI or a general coding agent. The goal is to expose what the runtime is doing:
steps, tool calls, artifacts, process evidence, patch evidence, verification,
and final answer.

The chosen direction is an event-first interactive CLI. It should be
line-oriented and terminal-friendly, with a machine-readable JSONL mode kept as
a first-class output. TUI remains a researched follow-up after real usage shows
which views deserve persistent panes.

## Current Evidence

The current runtime already records the core interaction path:

- `Runtime::step` emits runtime events such as `SessionStarted`,
  `StepStarted`, `ToolCallPending`, `ArtifactRecorded`,
  `ToolCallResolved`, `StepCompleted`, `Failed`, and `Cancelled`.
- `Runtime::run_agent_loop` composes step -> tool execution -> continuation
  until completion, cancellation, failure, or a bounded blocked state.
- Tool output is recorded as an artifact before `ToolCallResolved` is emitted.
- Ledger facts are recorded alongside observable events so state is written
  before the event claims it.
- `merry shell --events-jsonl` can already print some raw runtime event streams.
- `merry --with-sandbox debug coding-loop-live-smoke` proves the live loop, but
  currently reports only `coding-loop-live-smoke: ok` on success.

This creates an observability gap: the runtime has evidence, but the human
operator cannot easily see it while using the successful live loop.

## User Experience

Add a non-default event-first CLI surface for the coding loop. The preferred
first command shape is under debug until the interaction contract stabilizes:

```bash
merry --with-sandbox debug agent --task "Fix the greeting and verify it."
```

The command should run the same kind of sandboxed live coding loop as the live
smoke, but instead of printing only `ok`, it should stream a human-readable
timeline:

```text
session started: coding-loop-live
step 1 started
tool requested: run_process
  argv: rg --files
  cwd: .merry/local/...
artifact recorded: artifact-...
tool resolved: succeeded

step 2 started
tool requested: workspace_read_file
  path: src/lib.rs
artifact recorded: artifact-...
tool resolved: succeeded

step 3 started
tool requested: workspace_patch_file
  path: src/lib.rs
  old/new: "unfixed" -> "fixed-by-live-llm"
artifact recorded: artifact-...
tool resolved: succeeded

final answer:
...
```

The same command should support:

```bash
--events-jsonl
```

JSONL mode should emit exact `RuntimeEvent` JSON for debugging and regression
capture. Human-readable mode may summarize fields, but it must never replace
the raw event mode.

## Scope

In scope:

- A line-oriented CLI surface for running one sandboxed live coding-loop task.
- Human-readable rendering of runtime events in event order.
- Tool-call summaries for `run_process`, `workspace_read_file`, and
  `workspace_patch_file`.
- Artifact summaries that show artifact IDs and short content-derived previews
  where safe.
- Clear final status: completed, failed, cancelled, or blocked.
- Clear error output when sandbox/config/provider/tool policy gates fail.
- JSONL mode for exact event capture.
- Deterministic tests using fake provider/fake runner for renderer behavior.
- Existing opt-in live/sandbox smoke remains non-default.

Out of scope:

- Full-screen TUI.
- General coding agent behavior.
- Arbitrary shell strings, pipelines, inherited env, stdin, network tools, or
  broad filesystem writes.
- A new approval system.
- Runtime/provider conversation state.
- Making live provider behavior part of default tests.

## Architecture

The first implementation should keep the CLI as the UI owner while avoiding
new runtime policy decisions in CLI rendering code.

Recommended components:

- `AgentEventRenderer`: CLI-local renderer that accepts `RuntimeEvent` values
  plus optional artifact reads and writes human-readable lines.
- `AgentRunMode`: exact-event JSONL mode or human-readable timeline mode.
- `AgentCommandConfig`: task text, model/config path, max output tokens,
  sandbox requirement, and output mode.
- Reuse existing live smoke setup for the first version, then extract common
  setup once the interactive path proves the needed shape.

The renderer should read artifact contents only after receiving
`ArtifactRecorded` or `ToolCallResolved` references. For large artifacts, show
bounded summaries and keep exact content available by artifact ID.

## Runtime Event Mapping

The CLI should map events conservatively:

- `SessionStarted`: print session start once.
- `StepStarted`: increment and print step number.
- `ToolCallPending`: print tool name and safe argument summary.
- `ArtifactRecorded`: print artifact ID, kind if known, and a bounded summary.
- `ToolCallResolved`: print call ID, status, and artifact reference.
- `StepCompleted`: print step completion.
- `Failed`: print diagnostic code and message.
- `Cancelled`: print diagnostic code and message.
- `EvidenceReferenced`: print evidence reference only if it helps trace output.

For process artifacts, summarize exact argv, cwd, exit status, and bounded
stdout/stderr lengths or short text. For patch artifacts, summarize path and
old/new snippets. For read-file artifacts, summarize path and byte/line count.

## Error Handling

The command should fail closed when:

- `--with-sandbox` evidence is missing.
- local OpenAI-compatible config is missing or invalid.
- the model emits multiple pending tool calls.
- the model emits completion and a pending call in the same step.
- a required artifact cannot be read for rendering.
- the loop reaches max steps.

Human-readable mode should explain the gate or failure. JSONL mode should keep
raw events as the primary diagnostic and print command-level usage errors to
stderr.

## Testing

Default tests must stay deterministic and offline.

Test coverage should include:

- Rendering a fixed event sequence into stable human-readable lines.
- Rendering process, read-file, and patch artifact summaries with bounded
  output.
- JSONL mode preserving exact `RuntimeEvent` serialization.
- Non-sandbox denial before config/network access.
- Config-gating behavior remains non-network.
- Loop status formatting for completed, failed, cancelled, and blocked results.

Opt-in checks:

- Existing deterministic bwrap smoke remains ignored by default.
- Existing live smoke remains ignored/manual.
- A later live interactive CLI smoke may be added only as an ignored command or
  documented manual check.

## TUI Follow-Up

TUI is technically viable with Ratatui and Crossterm. Ratatui is a current Rust
terminal UI library and commonly uses Crossterm as its backend; Crossterm owns
raw terminal mode, alternate screen, key events, styling, and cursor control.

Do not start with TUI in this milestone. TUI should be considered after the
event-first CLI has produced real usage evidence about:

- which event types need persistent visibility
- whether artifact summaries need expandable panes
- whether approval/policy prompts need keyboard interaction
- whether scrollback and search matter more than live updates
- what should remain machine-readable for regression capture

Reference links:

- Ratatui documentation: https://ratatui.rs/
- Crossterm crate documentation: https://docs.rs/crossterm/latest/crossterm/

## Acceptance Criteria

- A user can run one sandboxed live coding-loop task and see a readable event
  timeline instead of only `ok`.
- The CLI shows which tool was requested, what key arguments were used, which
  artifact was recorded, and whether the tool resolved successfully.
- The CLI shows final loop status and final answer.
- `--events-jsonl` preserves exact runtime events.
- Default tests do not require bwrap, network, or live credentials.
- The design does not introduce a full TUI or general coding-agent scope.
