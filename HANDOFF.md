# Handoff

Status: complete

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding loop.

Session milestone:

- Task 7, end-to-end log-enabled smoke verification.

Task queue status:

- Tasks 1-6 and 6A remain complete from prior observability slices.
- Task 7 is complete: deterministic coding-loop log verification now covers
  config-backed JSON logs for runtime loop, provider request, workspace tool,
  process execution, artifact record, tool resolution, diagnostic-code, and
  completed terminal status records.
- The observability-first coding-loop milestone is complete enough for the next
  lease to move to the roadmap's process-profile/tool-set work.

Done condition:

- Task 7 log smoke and validation evidence are recorded in tracked tests,
  roadmap/plan/status files are updated, and this lease is committed.

## What Changed

Files changed:

- `README.md`
- `ROADMAP.md`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `plans/2026-05-23-config-backed-observability.md`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `crates/merry-runtime/src/agent_loop.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/session.rs`
- `crates/merry-runtime/tests/agent_loop.rs`

Summary:

- Added provider-neutral `runtime.provider.request` tracing in
  `merry-runtime` before any provider call, so deterministic/scripted providers
  are observable without live adapter behavior.
- Added `runtime.artifact.record` tracing after session-owned artifact state is
  written, with artifact id, kind, and byte count but no payload content.
- Stabilized successful loop/tool finish logs so `diagnostic_code` is present
  as an empty string when no diagnostic applies.
- Added a deterministic CLI-crate coding-loop log smoke that enables
  file-backed JSON logs from XDG TOML config, runs the scripted coding-loop
  runtime with a fake process runner, and asserts expected records are present
  without raw prompt, source, process stdout, model final text, provider wire,
  or secret-like content.
- Added CLI integration tests for default XDG state log path and log-parent
  failure behavior.
- Isolated `crates/merry-cli/tests/debug.rs` command helpers from host XDG
  config so user-local config no longer pollutes default integration tests.
- Updated README, roadmap, implementation plan, decisions, execution state, and
  handoff for Task 7 completion.

## Validation

Commands run:

- `cargo test -p merry-cli coding_loop_smoke_writes_configured_json_log_records_without_payloads -- --nocapture`
- `cargo test -p merry-cli debug_command --test debug -- --nocapture`
- `cargo test -p merry-runtime agent_loop_traces_loop_steps_tool_process_and_terminal_status -- --nocapture`
- `cargo test -p merry-cli`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- Temporary XDG real bwrap smoke:
  `target/debug/merry --with-sandbox debug coding-loop-smoke`
- `git diff --check`
- `git status --short --untracked-files=all`

Result:

- Passed.
- The focused log-smoke test failed first on missing `diagnostic_code`; the
  implementation now emits stable empty diagnostic-code fields on successful
  loop/tool finish logs.
- Initial cargo test attempts needed network escalation to download missing
  crates. After dependencies were cached, default validation ran normally.
- Real deterministic bwrap smoke with temporary XDG log config printed
  `coding-loop-smoke: ok` and the log tail contained provider request,
  workspace patch, process verification, artifact record, tool finish, and loop
  finish records.

## Decisions

Decisions made:

- Runtime owns provider-neutral request tracing for all providers, while
  provider adapters may still add adapter-specific metadata such as endpoint
  path.
- Artifact record tracing belongs after artifact state is written, preserving
  state-before-observation ordering and avoiding payload logging.
- CLI integration tests should not inherit host XDG config by default; tests
  that need config must opt into an explicit temp XDG root.

Pending decisions:

- Exact shape and ownership of the next runtime-owned read-only process profile
  for reusable inspection/evidence commands.

## Blockers

Blockers:

- None.

Residual risk:

- OpenAI-compatible live runs may now show both provider-neutral runtime
  request metadata and adapter-specific request metadata. This is intentional
  for now and recorded in `DECISIONS.md`.
- The live provider smoke remains opt-in/manual; default tests do not exercise
  network or live model behavior.

Next exact action:

- Start the next lease from `ROADMAP.md` Next Active: define a runtime-owned
  read-only process profile for reusable workspace inspection and exact
  evidence retrieval. Initial coverage should include `rg --files`, literal
  `rg <pattern>`, and a file-slice shape such as `sed -n RANGE FILE` or an
  equivalent typed read tool.

## Scope For Next Session

Allowed edits:

- Runtime process/profile modules and focused tests needed for the read-only
  process profile slice.
- Existing CLI/debug smoke wiring only as needed to consume the profile.
- Public-safe roadmap/plan/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI, REPL, or multi-turn UI before reusable runtime/tool
  registration is clearer.
- Reintroducing repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.

Do not reconsider:

- Observability-first Task 7 is complete.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.
- The next proof gap is reusable runtime-owned process/tool profiling, not a
  new interaction surface.

## Commit

Status: committed by this lease

Message:

- project-continuity: complete task 7 log smoke

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
