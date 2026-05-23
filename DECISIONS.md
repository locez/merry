# Decisions

## 2026-05-23 - Re-anchor MVP On Real Sandboxed Coding Loop

Decision:
Merry's current P0 is the minimal useful coding loop: a runtime-owned agent loop that can inspect a disposable repo, read exact evidence, apply a constrained patch, run verification, and emit artifact-backed events inside the CLI bwrap sandbox. Policy, risk taxonomy, and review models are supporting work, not the primary deliverable.

Reason:
The project had accumulated strong policy/sandbox/judgment foundations, but the visible MVP value was drifting away from a runnable task that demonstrates runtime usefulness.

Evidence:
Private design/raw docs define Merry as a runtime for structured state, artifact-backed evidence, compiled context, skills, and long-task execution. Current code already has `run_agent_loop`, registered tool execution, `process_command_tool`, workspace patch tooling, OpenAI-compatible provider wiring, and `merry --with-sandbox`.

Tradeoff:
Live and sandboxed tests add operational complexity and require local credentials for some lanes. Default tests must remain deterministic and offline, while opt-in smoke tests prove real behavior.

Reversible:
Yes. If the real coding-loop harness exposes missing lower-level contracts, the roadmap can split the harness into smaller runtime/tool/provider slices without abandoning the MVP acceptance target.

Follow-up:
Add the first Runtime Coding Loop Harness slice, then add the read-only process profile and fixture patch/verification path.

## 2026-05-23 - First Real Bwrap Coding-Loop Smoke Stays Deterministic

Decision:
The first real `bwrap` coding-loop smoke is an explicit CLI debug command:
`merry --with-sandbox debug coding-loop-smoke`. It uses a deterministic scripted
provider and real `TokioProcessRunner` process execution inside the CLI bwrap
handoff, but it does not call a live provider.

Reason:
This proves the runtime loop, sandbox handoff, real process runner, workspace
patch tool, continuation flow, and fixture verification without making local
credentials or live model behavior part of default validation.

Evidence:
The command creates `.merry/local/coding-loop-smoke`, runs `rg --files`, reads
`src/lib.rs`, patches `"unfixed"` to `"fixed-by-live-llm"` through
`workspace_patch_file`, runs `rg fixed-by-live-llm`, verifies
`AgentLoopStatus::Completed`, checks four successful tool resolutions, and
validates the patched file content. The ignored integration
test passed with:
`cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`.

Tradeoff:
The smoke is still CLI-assembled and deterministic-provider based. It proves a
real sandbox/process/edit path, but not a reusable runtime-owned process profile
or live-provider coding-agent behavior.

Reversible:
Yes. Once a reusable coding-loop harness exists, this CLI command can become a
thin wrapper around the library-level profile/tool-set registration.

Follow-up:
Implement the runtime-owned read-only process profile and reusable coding-loop
tool-set registration, then add the live OpenAI-compatible smoke lane.

## 2026-05-23 - Live LLM Coding-Loop Proof Is A Separate Acceptance Gate

Decision:
The live LLM coding-loop smoke is an explicit second CLI debug command:
`merry --with-sandbox debug coding-loop-live-smoke`. It must use
`OpenAiProvider` for model decisions inside the existing bwrap handoff and must
not be counted as passed until a credentialed local run succeeds.

Reason:
A scripted provider can prove runtime/tool/sandbox wiring, but it cannot prove
that a real model can retain the task goal, choose one constrained tool call at
a time, apply the patch, and verify the result. Treating scripted success as
live-agent proof would hide the exact drift the MVP is meant to expose.

Evidence:
The command now builds a live-provider runtime, requires the real
`--with-sandbox` child handoff before reading config or attempting network,
reads ignored local config from `.merry/secrets/openai.env`, uses real
`TokioProcessRunner` process execution, and validates runtime events for
`rg --files`, `workspace_read_file`, `workspace_patch_file`, and
`rg fixed-by-live-llm`. `Runtime::run_agent_loop` also carries the original
task text into continuation turns so a real model does not lose the objective
after tool results. The user reported that the credentialed live smoke passed
against their trusted configured server. That live run exposed a provider HTTP
metadata gap: Merry did not set a `User-Agent` header.

Tradeoff:
The live smoke is nondeterministic and credential-dependent, so it stays
ignored/non-default. The payoff is that failures become real evidence about
prompting, tool schemas, continuation shape, process profile gaps, or provider
adapter behavior.

Reversible:
Yes. The command can later become a thin wrapper around reusable runtime-owned
coding-loop profile registration once that contract exists.

Follow-up:
Keep the live smoke as an opt-in regression lane, fix the provider
`User-Agent` gap, and treat any future live failure as the next minimal
runtime/tool-contract fix rather than widening the roadmap.

## 2026-05-23 - OpenAI-Compatible Requests Send Merry User-Agent

Decision:
`merry-provider-openai` sets `User-Agent: merry/<crate version>` on
OpenAI-compatible Responses requests.

Reason:
The live coding-loop smoke passed against the user's trusted configured server,
but exposed that Merry requests lacked a product identity header. This belongs
at the provider HTTP boundary because runtime, CLI, and tool code should not
know provider wire headers.

Evidence:
The provider request-construction test asserts the `User-Agent` header without
network, and the ignored loopback integration test asserts the actual HTTP
request carries the same header.

Tradeoff:
The value is intentionally simple and stable. Richer telemetry or per-command
metadata can be added later only if there is a concrete provider/debugging need.

Reversible:
Yes. The header value can be made configurable later without changing runtime
state or the provider trait boundary.

## 2026-05-23 - Observability Before Interactive CLI Or TUI

Decision:
The next milestone is structured observability for the coding loop, specified
in `specs/2026-05-23-observability-first-coding-loop.md`, not an event-first
CLI and not a full TUI. The first implementation should add opt-in logging and
`tracing` instrumentation at runtime, tool, process, provider, sandbox, and
artifact boundaries.

Reason:
The live coding-loop smoke has proven that tool calling works, but its success
path still collapses the runtime's evidence into `ok`, and runtime events alone
do not explain in-flight behavior well enough for real debugging. The user needs
to know what action is happening, why it happened, what arguments and policy
shape were used, what artifact was recorded, and where a multi-step or later
multi-turn run drifted. Adding another CLI view before the logging contract
would still leave the system opaque.

Evidence:
`Runtime::step` and `Runtime::run_agent_loop` already emit session, step, tool,
artifact, resolution, completion, failure, and cancellation events. The live
smoke has passed in the user's trusted configured environment. The OpenAI
provider already has localized `tracing` spans, but the runtime/tool/process
loop lacks a consistent structured log contract. Ratatui and Crossterm are
viable Rust TUI building blocks, but the current gap is observability, not
full-screen interaction.

Tradeoff:
This delays a new interactive CLI/REPL/TUI surface. The benefit is that every
later surface can consume stable correlation fields and operators can debug real
smoke behavior immediately. The cost is that multi-turn UI input waits behind
one observability slice.

Reversible:
Yes. Once logs show which state and action views matter, an event renderer,
interactive CLI, REPL, or Ratatui-based TUI can be added on top without
redefining runtime behavior.

Follow-up:
Create an implementation plan for CLI-owned tracing subscriber setup plus
runtime/tool/process/provider instrumentation and deterministic tracing tests.
