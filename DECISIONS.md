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
