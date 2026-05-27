# Handoff

Status: complete

## Current Work

Current milestone or track:

- M2 Shell-Compatible Runtime Boundary.

Session milestone:

- First implementation slice: separate read-only shell-wrapper admission from
  the structured read-only argv lane.

Task queue status:

- Added `process.shell.read_only.v1` as a distinct permission profile.
- Added a narrow plain shell-wrapper classifier for `bash`/`sh`/`zsh -c|-lc`
  scripts joined by `|`, `&&`, `||`, or `;`, where every segment must match the
  direct read-only process classifier.
- Added `RuntimeBuilder::allow_read_only_shell_process_actions` so shell
  wrappers require an explicit shell runner opt-in.
- Added deterministic tests proving:
  - `bash -lc "rg ProcessRunner | wc -l"` derives the shell read-only profile.
  - the same proposal is denied when only structured low-risk process actions
    are enabled.
  - it executes only under the shell read-only opt-in and records
    `process.shell.read_only.v1`.
  - redirects, command substitution, and mutating pipeline segments are denied
    without runner calls.
- Updated `ROADMAP.md` and `DECISIONS.md` with the M2 slice and guardrails.

Done condition:

- The M2 shell-compatible boundary now has a first executable, test-backed
  runtime admission slice without introducing a model-facing shell tool or a
  broad shell parser.

## What Changed

Files changed:

- `crates/merry-runtime/src/process.rs`
- `crates/merry-runtime/src/action_policy.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/lib.rs`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Summary:

- Split read-only shell-wrapper handling from the structured argv lane.
- Kept shell-wrapper execution fail-closed unless a dedicated shell runner lane
  is explicitly configured.
- Preserved artifacts/audit/ledger evidence behavior by reusing the existing
  process execution path with the new shell profile id.
- Recorded that the classifier is evidence/admission plumbing only, not the
  authorization model for complex shell syntax.

## Validation

Commands run:

- `cargo fmt --all --check`
- `cargo test -p merry-runtime process --lib`
- `cargo test -p merry-runtime --lib`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`

Result:

- Passed.
- `cargo test --all` did not run ignored live/bwrap smokes; those remain
  explicit opt-in lanes.

## Decisions

Decisions made:

- `process.shell.read_only.v1` is separate from `process.read_only.v1`.
- Existing `allow_low_risk_process_actions` does not admit shell wrappers.
- Read-only shell wrapper execution requires
  `allow_read_only_shell_process_actions`.
- The current shell classifier is intentionally narrow and must not grow into
  the broad shell authorization model.

Pending decisions:

- Exact shell command/script artifact schema.
- Payload-free shell trace metadata fields.
- Whether the first real shell runner uses the existing CLI process runner
  adapter or a runtime-owned shell runner wrapper.
- Approval/session semantics for shell commands beyond the read-only wrapper
  lane.

## Blockers

Blockers:

- None.

Residual risk:

- The classifier is hand-bounded and intentionally conservative. A stronger
  parser or execution interception layer may replace it later, but this slice
  keeps the profile/admission boundary stable.
- No real shell runner was added in this lease; tests use fake runners.

Next exact action:

- Continue M2 by defining shell execution input/output artifacts and payload-free
  trace metadata for the future shell runner: exact command/script artifact,
  script byte/hash metadata, stdout/stderr/status artifacts, compact ledger
  reduction, and cancellation behavior.

## Scope For Next Session

Allowed edits:

- Runtime shell/process artifact and trace boundary modules.
- Focused tests for command/script input evidence, output artifacts, cancellation,
  and ledger reduction.
- Public-safe roadmap/decision/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content.
- Broad model-facing shell tool before shell artifacts/traces are defined.
- A Merry-owned subset shell parser as the authorization model.
- Approval/session implementation unless explicitly chosen as the next slice.
- Full-screen TUI, REPL, or multi-turn UI.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Shell compatibility must use real shell execution under explicit profiles;
  do not revive parser-first M2.
- `process.shell.read_only.v1` must stay distinct from `process.read_only.v1`.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- feat(runtime): add read-only shell process profile

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
