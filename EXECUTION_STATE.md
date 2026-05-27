# Execution State

Lease status: complete

## Source Of Truth

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `ROADMAP.md`
- `README.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`
- `docs/design/mvp-design.md` (ignored private local design source)
- `docs/design/global-design.md` (ignored private local design source)
- `docs/product/product-strategy.md` (ignored private local product source)
- `merry-raw-docs/` (ignored original local source material; do not commit)

## Planning Maturity

Level: implementation-in-progress

Current planning artifact:

- `ROADMAP.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- M1 Structured Process Boundary MVP.

Session milestone:

- Process permission profile routing slice: make classified process intents
  route through explicit read-only and accepted local workspace permission
  profiles, with the local workspace admission stored and checked at runtime.

Goal:

- Prove that a process runner is not enough to execute a local workspace effect:
  the classified intent must match the permission profile admitted for that
  runner, and execution artifacts/ledger/trace records must report the profile
  that was actually admitted.

Task queue status:

- Added intent-to-permission-profile derivation for structured process intents.
- `is_low_risk_process_action_intent` now uses that derivation for the
  read-only lane.
- `AcceptedLocalWorkspaceProcessAdmission` now carries the admitted
  `ProcessPermissionProfileId` and can check whether an intent matches it.
- `RuntimeBuilder::allow_accepted_local_workspace_process_actions` now stores
  the admission with the runner instead of discarding it.
- Runtime process execution receives the admitted profile id directly instead
  of inferring it after the fact from `ActionRiskTier`.
- Local workspace effect execution is denied when the injected runner's
  admission profile does not match the classified intent.
- Process execution traces now include `permission_profile_id` alongside
  payload-free argv/cwd/status/output-byte metadata.
- `ROADMAP.md` now marks M1 complete and points the next active work at M2
  shell-compatible model tooling.
- `DECISIONS.md` records the admission-time profile routing decision.

Allowed expansion:

- Runtime process classifier/profile/admission modules and focused tests.
- Runtime/provider-neutral metadata needed to expose the admitted process
  permission profile.
- Existing agent-loop/coding-loop tests needed to prove the structured process
  boundary still works.
- Public-safe roadmap/decision/continuity updates.

Done condition:

- Read-only process intents map to `process.read_only.v1`.
- Local workspace verification intents map to
  `process.local_workspace.bwrap.v1`.
- Unknown, forbidden, stdin-bearing, or non-empty-env intents do not receive an
  auto-admitted profile.
- Local workspace execution is denied without a matching stored admission, even
  when a runner is injected.
- Process artifacts, ledger facts, and traces record the admitted
  `permission_profile_id`.
- Focused runtime tests and full default validation pass.
- Handoff updated and lease committed.

Drift boundary:

- Do not add TUI, REPL, or interactive CLI scope.
- Do not implement shell string parsing or pipelines in this M1 lease.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs`.
- Do not make live provider behavior part of default tests.
- Do not expand this slice into approval/session grants.

Task type: implementation/docs

Acceptance criteria:

- `process_permission_profile_id_is_derived_from_admitted_intent_shape`
  demonstrates read-only/local-workspace/unknown/stdin profile routing.
- `local_workspace_process_admission_matches_only_its_permission_profile`
  demonstrates admission matching.
- `process_admission_predicates_keep_low_and_local_workspace_lanes_distinct`
  keeps low-risk and local workspace lanes separate and rejects mismatched
  admissions.
- `accepted_local_workspace_process_action_denies_when_admission_profile_mismatches`
  denies without calling the runner.
- `agent_loop_traces_loop_steps_tool_process_and_terminal_status` verifies
  process traces include `permission_profile_id`.
- `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  and `cargo test --all` pass.

## Scope

Allowed edits:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`
- `crates/merry-runtime/src/action_audit.rs`
- `crates/merry-runtime/src/action_policy.rs`
- `crates/merry-runtime/src/process.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/tests/agent_loop.rs`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content
- shell-compatible model tool implementation, pipelines, scripts, or approval
  session behavior
- full-screen TUI, REPL, or multi-turn UI scope

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

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
- `git diff --check`
- `git status --short --untracked-files=all`

Validation notes:

- Focused and full default validation passed.
- Live provider and real `bwrap` smoke lanes were not run for this slice; they
  remain explicit opt-in lanes and are not required for deterministic M1
  profile routing.

## Research

Research required: no

Research reason:

- This lease used existing roadmap/status/code evidence. No internet or ignored
  private source research was needed.

Research artifact:

- None.

## Next Action

Next exact action:

- Start M2 from `ROADMAP.md`: add the first shell-compatible model tool on top
  of the structured process intent path. Begin with simple command strings that
  parse into already supported argv shapes and keep pipelines, scripts, stdin,
  env overrides, and unknown forms denied or approval-gated.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before reusable runtime/process/tool profiles are
  clearer.
- Do not move private Codex/raw-doc findings into tracked source text.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
