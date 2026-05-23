# Execution State

Lease status: complete

## Source Of Truth

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `ROADMAP.md`
- `README.md`
- `docs/design/mvp-design.md` (ignored private local design source)
- `docs/design/global-design.md` (ignored private local design source)
- `docs/product/product-strategy.md` (ignored private local product source)
- `merry-raw-docs/` (ignored original local source material; do not commit)

## Planning Maturity

Level: structured-roadmap

Current planning artifact:

- `ROADMAP.md`

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- Real bwrap coding-loop smoke.

Session milestone:

- Add an opt-in CLI smoke that runs the coding-loop shape inside the existing `merry --with-sandbox` bwrap handoff.

Goal:

- Prove the same inspect -> exact evidence -> constrained patch -> verification -> final loop shape can be executed from `merry-cli` inside the real bwrap sandbox, while keeping default tests deterministic and not requiring live provider credentials.

Task queue:

- Dispatch read-only subagent exploration for CLI bwrap smoke and process/profile blockers. Done.
- Add a narrow opt-in CLI debug smoke subcommand for the sandboxed coding-loop shape. Done.
- Add tests for help/usage and non-sandbox denial; run a real smoke manually if bwrap works in this environment. Done.
- Update roadmap/continuity status with what the bwrap slice proves and what remains. Done.
- Commit the lease. Done.

Allowed expansion:

- Minimal CLI dependency on `merry-tool-workspace` for outer-layer smoke composition.
- Minimal CLI helper code for scripted provider, disposable fixture setup, runtime construction, and result validation.
- Small documentation/status updates tied to the implemented slice.

Done condition:

- The CLI exposes an explicit opt-in smoke command that refuses to run outside a validated sandbox handoff, executes the coding-loop shape inside bwrap with real process runner and workspace patch tooling, and reports deterministic success. Continuity state and handoff record the command and residual live-provider/config work.

Drift boundary:

- Do not implement a full autonomous coding agent, live-provider harness, broad process profile, broad CLI UX, graph memory, skill VM, Python SDK, or arbitrary shell expansion in this lease.

Task type: implementation

Acceptance criteria:

- The CLI command is explicit and non-default, under debug/smoke tooling.
- The command requires the real `--with-sandbox` child handoff evidence before running local workspace effect verification.
- The smoke uses a deterministic scripted provider, not a live provider.
- The smoke uses `TokioProcessRunner` for real `rg --files` and verification process execution inside the sandbox.
- The smoke uses `workspace_patch_file` for the edit and mutates only a disposable fixture under ignored local state.
- The smoke reaches `AgentLoopStatus::Completed`, leaves no pending tool calls, and validates the patched fixture content.
- Default `cargo test` does not require bwrap or live credentials.

## Scope

Allowed edits:

- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `DECISIONS.md`
- `ROADMAP.md`
- `crates/merry-cli/Cargo.toml`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- small `README.md` status update if needed

Forbidden edits:

- Rust production runtime/provider/CLI implementation outside the test harness slice
- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `ROADMAP.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo test -p merry-cli`
- `cargo fmt --all --check`
- `cargo clippy -p merry-cli --all-targets -- -D warnings`
- `git diff --check`
- opt-in real smoke command when supported by the host

Validation notes:

- Passed:
  - `cargo fmt --all --check`
  - `cargo clippy -p merry-cli --all-targets -- -D warnings`
  - `cargo test -p merry-cli`
  - `cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`
- The default CLI test suite skips the real bwrap smoke; the ignored smoke runs successfully when explicitly selected on this host.

## Research

Research required: no

Research reason:

- User allowed subagents. Read-only explorer subagents are being used to confirm CLI bwrap smoke entry points and process/profile blockers; no external research is required.

Research artifact:

- Explorer outputs from subagent workers in this lease.

## Next Action

Next exact action:

- Implement a reusable runtime-owned read-only process profile or tool-set registration layer for the coding-loop harness, covering `rg --files`, literal search, and exact source evidence retrieval without adding one command match at a time.

Do not reconsider:

- Do not make policy taxonomy the primary P0 output.
- Do not commit `docs/`, `merry-raw-docs/`, `.env.merry.local`, `.merry/local/`, or `.merry/secrets/`.
- Do not require live provider tests in default `cargo test`.
