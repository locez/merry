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

- Roadmap/MVP recalibration around a real sandboxed coding-agent runtime loop.

Session milestone:

- Re-anchor Merry's current P0 to a minimal useful coding loop that proves runtime value through artifact-backed, bwrap-sandboxed, eventually live-provider execution instead of policy-only progress.

Goal:

- Update durable continuity and roadmap state so future `/goal $project-continuity` sessions advance the real sandboxed coding-loop MVP and know where local API credentials belong.

Task queue:

- Created missing continuity artifacts.
- Recorded the roadmap/MVP decision.
- Marked ignored private source material and local credential files.
- Updated the public roadmap current phase and MVP acceptance target.
- Aligned `README.md` and `AGENTS.md` entry points with the corrected P0.
- Wrote handoff and validation status.

Allowed expansion:

- Small entry-point doc alignment when needed to keep the current roadmap discoverable.
- No Rust implementation in this lease.

Done condition:

- Continuity files exist, `ROADMAP.md` names the real sandboxed coding-loop MVP as current P0, local credential/config handling is documented, and `HANDOFF.md` points the next session at the first implementation slice.

Drift boundary:

- Stop before designing or implementing the full coding agent, broad skill VM, graph memory, Python SDK, or live-provider harness beyond the acceptance target and configuration policy.

Task type: planning

Acceptance criteria:

- `SESSION_RUNBOOK.md`, `AGENT_ROLES.md`, `EXECUTION_STATE.md`, and `HANDOFF.md` exist.
- `ROADMAP.md` current phase no longer treats policy taxonomy as the primary output.
- The next MVP acceptance test includes a real bwrap sandbox lane and an opt-in live provider lane.
- API keys/base URL are assigned to ignored local env/config only.

## Scope

Allowed edits:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `DECISIONS.md`
- `.gitignore`
- `ROADMAP.md`
- small `AGENTS.md` or `README.md` routing updates if needed

Forbidden edits:

- Rust runtime/provider/CLI implementation
- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `ROADMAP.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `git diff --check`

Validation status: passed

Validation notes:

- `git diff --check` passed.
- Rust checks were not run because this lease changed only documentation,
  continuity state, and ignore rules.

## Research

Research required: no

Research reason:

- User explicitly requested the roadmap/MVP correction, and repo/private-doc evidence is sufficient.

Research artifact:

- none

## Next Action

Next exact action:

- Implement the first Runtime Coding Loop Harness slice: an opt-in test/harness that can run inside `merry --with-sandbox` against a disposable fixture repo, using fake provider by default and a separately gated live OpenAI-compatible provider path.

Do not reconsider:

- Do not make policy taxonomy the primary P0 output.
- Do not commit `docs/`, `merry-raw-docs/`, `.env.merry.local`, `.merry/local/`, or `.merry/secrets/`.
- Do not require live provider tests in default `cargo test`.
