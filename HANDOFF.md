# Handoff

Status: complete

## Current Work

Current milestone or track:

- M1 Structured Process Boundary MVP.

Session milestone:

- Process permission profile routing slice: classified process intents now
  route through explicit read-only and accepted local workspace permission
  profiles, and local workspace admission is stored and checked at runtime.

Task queue status:

- Completed this slice. Process permission profile id is now derived from the
  admitted intent shape before execution.
- `AcceptedLocalWorkspaceProcessAdmission` carries the admitted profile id.
- Runtime stores local workspace admission with its runner and denies execution
  when the admission profile does not match the classified intent.
- Process execution artifacts, audit evidence, compact ledger facts, and traces
  record the admitted `permission_profile_id`.
- `ROADMAP.md` marks M1 complete and points the next active work at M2.

Done condition:

- Focused and full validation passed, state files are updated, and this lease
  is committed.

## What Changed

Files changed:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`
- `crates/merry-runtime/src/action_audit.rs`
- `crates/merry-runtime/src/action_policy.rs`
- `crates/merry-runtime/src/process.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/tests/agent_loop.rs`

Summary:

- Added `required_process_permission_profile_id` for structured process intent
  to profile routing.
- Kept `is_low_risk_process_action_intent` on the same routing path as the
  read-only profile.
- Added profile id storage and matching to
  `AcceptedLocalWorkspaceProcessAdmission`.
- Made `RuntimeBuilder::allow_accepted_local_workspace_process_actions` retain
  admission instead of discarding it.
- Passed the admitted profile id into process execution instead of deriving it
  after execution from policy risk tier.
- Added denial coverage for mismatched local workspace admission profile.
- Added `permission_profile_id` to process execution trace records.
- Recorded the decision that permission profiles are admission-time routing
  inputs.

## Validation

Commands run:

- `cargo test -p merry-runtime --lib process_permission_profile`
- `cargo test -p merry-runtime --lib local_workspace_process_admission_matches_only_its_permission_profile`
- `cargo test -p merry-runtime --lib process_admission_predicates_keep_low_and_local_workspace_lanes_distinct`
- `cargo test -p merry-runtime --lib accepted_local_workspace_process_action_denies_when_admission_profile_mismatches`
- `cargo test -p merry-runtime --test agent_loop agent_loop_traces_loop_steps_tool_process_and_terminal_status`
- `cargo test -p merry-runtime --lib`
- `cargo test -p merry-runtime --test agent_loop`
- `cargo test -p merry-tool-workspace --test runtime_integration coding_loop_harness_inspects_patches_verifies_and_completes`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`

Result:

- Passed.
- Live provider and real `bwrap` smokes were not run for this deterministic
  runtime slice.

## Decisions

Decisions made:

- Process permission profiles are admission-time routing inputs, not only
  execute-time labels.
- Local workspace effect process execution requires an admission profile that
  matches the classified intent.
- M1 Structured Process Boundary MVP is complete enough to start M2.

Pending decisions:

- Exact shell-compatible model tool name and JSON schema.
- How simple shell parsing should represent unsupported pipelines, scripts,
  stdin, env overrides, and unknown commands before M3 approval/session support.
- Whether to add an explicit stable-prefix change reason event or metadata in
  the next cache-observability slice.

## Blockers

Blockers:

- None.

Residual risk:

- Permission profiles are still a small static set. That is expected for M1;
  broader process/session approval remains a later milestone.
- The CLI `bwrap` profile remains an opt-in smoke boundary, not a complete
  sandbox proof.

Next exact action:

- Start M2 by adding the first shell-compatible model tool on top of the
  structured process intent path. Begin with simple command strings that parse
  into already supported argv shapes and deny unsupported shell forms.

## Scope For Next Session

Allowed edits:

- Runtime process tool/parser modules and focused tests.
- Runtime/tool specs needed for a simple shell-compatible model-facing command
  tool.
- Existing deterministic agent-loop/coding-loop tests that consume the new tool.
- Public-safe roadmap/decision/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content.
- Pipelines, scripts, approval sessions, long-running process sessions, or
  broad shell execution before M2's simple-command slice is proven.
- Full-screen TUI, REPL, or multi-turn UI before reusable runtime/tool
  registration is clearer.

Do not reconsider:

- Observability-first Task 7 is complete.
- Base instructions are included in the stable prefix cache boundary.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
- Process permission profiles now route admission before execution.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- feat(runtime): route process intents through permission profiles

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
