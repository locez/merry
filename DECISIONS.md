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
`src/lib.rs`, patches `"old"` to `"new"` through `workspace_patch_file`, runs
`rg new`, verifies `AgentLoopStatus::Completed`, checks four successful tool
resolutions, and validates the patched file content. The ignored integration
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
