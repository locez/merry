# Handoff

Status: complete

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding loop.

Session milestone:

- Add and enforce the user-facing `examples/config.toml` copy-and-edit
  contract requested by the user.

Task queue status:

- Tasks 1-6 remain complete from prior observability slices.
- Task 6A is complete: a tracked example config now exists and is parsed by
  deterministic CLI config tests.
- `AGENTS.md` now requires future accepted config-key changes to update
  `examples/config.toml` in the same change unless a reason is recorded.
- Plan, roadmap, README, execution state, and handoff updated. Task 7 remains
  next.

Done condition:

- `examples/config.toml` is a tested, copy-and-edit config starting point and
  future config schema drift has a tracked maintenance rule.

## What Changed

Files changed:

- `AGENTS.md`
- `README.md`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `crates/merry-cli/src/config.rs`
- `examples/config.toml`
- `plans/2026-05-23-config-backed-observability.md`

Summary:

- Added `examples/config.toml` with the current user-facing config shape:
  `[global]`, `[observability.log]`, `[providers.default]`, and
  `[providers.openai-compatible]`.
- Documented copy destination and the usual local edits: model, provider base
  URL, and credential source.
- Kept secrets out of the example; it references `OPENAI_API_KEY` and a
  config-relative `secrets/openai.key`.
- Added a CLI config unit test that includes and parses the example config,
  proving it matches the current schema and resolves expected defaults.
- Added repository guidance requiring future config-key changes to update the
  example config.
- Updated README/roadmap/plan/continuity status so the example config is treated
  as a maintained artifact.

## Validation

Commands run:

- `cargo test -p merry-cli example_config_toml_matches_current_schema_and_resolves_user_defaults -- --nocapture`
- `cargo test -p merry-cli config::tests -- --nocapture`
- `cargo fmt --all --check`
- `cargo clippy -p merry-cli --all-targets --all-features -- -D warnings`
- `cargo test -p merry-cli`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `git diff --check`
- `git status --short --untracked-files=all`

Result:

- Passed.
- The example-config test failed first because `examples/config.toml` was
  missing, then passed after adding the file.
- Validation is deterministic/offline; no bwrap, network, or live credentials
  were required.
- No private ignored docs, credentials, or generated build artifacts were added.

## Decisions

Decisions made:

- Use `examples/config.toml` as the canonical tracked example path.
- Keep `api_key_env` and config-relative `api_key_file` in the example so it
  works for both ordinary host runs and sandboxed live smoke setup.
- Enable debug JSON logging in the example because its main use is smoke/debug
  diagnosis; the comments explain the default XDG log path when `path` is
  omitted.
- Treat future config-key changes as incomplete unless they update
  `examples/config.toml` or record why the key is intentionally omitted.

Pending decisions:

- None required before Task 7.

## Blockers

Blockers:

- None.

Residual risk:

- The example is now schema-tested, but Task 7 still needs to verify the
  end-to-end smoke log content generated from config-backed logging.

Next exact action:

- Start `plans/2026-05-23-config-backed-observability.md`, Task 7:
  End-To-End Log-Enabled Smoke Verification. Add a deterministic CLI log smoke
  with XDG TOML observability enabled and assert the log contains runtime loop,
  provider request, workspace tool, process execution, artifact/tool resolution,
  diagnostic, and final status records without secrets or raw payload contents.

## Scope For Next Session

Allowed edits:

- `crates/merry-cli/tests/debug.rs`
- `README.md` only if implemented command behavior changes public usage text
- Follow-on Task 7 test/support files if needed
- Continuity file updates

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI, REPL, or multi-turn UI before observability exists.
- Reintroducing repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.

Do not reconsider:

- The next proof gap is log-enabled smoke verification on top of completed
  config/log, runtime/process trace, workspace/provider trace, and example
  config slices.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed by this lease

Message:

- docs: add maintained example config

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
