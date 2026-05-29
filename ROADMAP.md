# Roadmap

This roadmap is public-safe and implementation-focused. Private product strategy and design notes remain ignored under `docs/`.

## Current Phase

Merry has enough foundation to stop treating policy and sandbox design as the
main product output. Runtime/provider/tool execution hardening, Memory
Activation MVP internals, read-only workspace tools, workspace patch tooling,
the serial `Runtime::run_agent_loop`, OpenAI Responses provider wiring, process
action intent/evidence, and the CLI `bwrap` sandbox bootstrap are now
foundation for a real capability test.

The current P0 is the **Minimal Useful Coding Loop**: prove that Merry can run a
small coding-style task through runtime-owned state, tools, artifacts,
continuations, and verification. This is not because Merry's product identity is
"coding shell". Coding is the hard benchmark that forces exact evidence,
artifact-backed tool output, patch/write behavior, test loops, cancellation, and
prompt/context stability to work together.

The target loop is:

```text
user task
-> runtime agent loop
-> inspect workspace with read-only process/workspace tools
-> read exact source evidence
-> apply one constrained workspace patch
-> run verification inside a bwrap sandbox
-> final answer backed by runtime events, artifacts, and ledger facts
```

Safety remains mandatory, but it is a runtime property of the loop, not the
loop's substitute. Policy, profile, classifier, risk taxonomy, reviewer
evidence, and role-scoped model work should advance only when they unblock this
executable acceptance target or when the user explicitly requests that planning
work as the deliverable.

Default `cargo test` must stay deterministic, offline, and fake-provider based.
In addition, Merry needs explicit opt-in smoke lanes:

- `bwrap` sandbox smoke using a disposable fixture repository and real process
  runner.
- Live OpenAI-compatible smoke using locally supplied credentials and
  `MERRY_OPENAI_DEBUG=1`.

Local credentials must never be committed. The long-term config direction is an
XDG TOML config file:

```text
$XDG_CONFIG_HOME/merry/config.toml
fallback: ~/.config/merry/config.toml
```

That config should own global defaults, provider/model settings, and
observability settings such as log enabled/level/format/path. Logs should be
off by default and, when file-backed, should use XDG state paths such as
`$XDG_STATE_HOME/merry/logs/merry.jsonl` with
`~/.local/state/merry/logs/merry.jsonl` as fallback when no path is configured.
Opening or creating the configured/default log file should fail clearly instead
of silently falling back to stderr.
The legacy `.merry/secrets/openai.env` live-smoke config has been replaced for
the CLI debug smoke path by XDG TOML provider config; the legacy `--config`
flag is rejected. Config-relative `api_key_file` remains available so
sandboxed live smoke credentials do not have to pass through bwrap argv.
OpenAI-compatible config must set exactly one credential source: `api_key` or
`api_key_file`.
Sandbox self-reexec preserves the non-secret `MERRY_OPENAI_DEBUG=1` opt-in
marker only when its outer value is exactly `1`; API keys remain outside argv
and generic environment inheritance.
The tracked copy-and-edit example is `examples/config.toml`; future accepted
config-key changes should update that example in the same change and keep the
example free of real secrets or host-private paths.

The OpenAI provider target is the Responses API only. The provider request path is `/responses`; it preserves the Merry-owned `merry-llm` provider boundary, keeps OpenAI wire types private to `merry-provider-openai`, sets `store: false`, omits `previous_response_id`, avoids provider conversation state as Merry runtime state, and keeps `parallel_tool_calls: false` until runtime policy supports parallel tool calls. This provider work does not imply a live/OpenAI judgment path or public judgment API.

## Status Summary

### Completed

- Rust 2024 virtual workspace skeleton with the initial implementation crates.
- Core protocol vocabulary, typed IDs, event contracts, artifact references, tool specs, and provider boundary types.
- Runtime skeleton with session state, task ledger, artifact metadata, event streaming, cancellation, and deterministic fake-provider tests.
- CLI debug surface for inspecting runtime events as JSON lines.
- OpenAI Responses provider configuration, request rendering, streaming parser, and loopback/live smoke surfaces using private provider wire types.
- Context, ledger, and artifact loop for structured runtime state and reproducible context compilation.
- Provider step boundary that keeps runtime state separate from provider conversation state.
- Artifact-backed model output handling.
- Generation configuration propagation through Merry-owned request types.
- Pending tool call representation.
- Tool result resolution into runtime state.
- Tool continuation flow after tool results are supplied.
- Registered tool execution through the runtime tool registry.
- Runtime-reserved artifact IDs for tool/model output paths that must be claimed before events are emitted.
- Opt-in OpenAI Responses debug/tool flow for manual provider integration checks.
- Memory Activation MVP internal runtime integration with session-owned in-memory stored source, deterministic activation, evidence validation, provider-step timing, and lifecycle cleanup coverage.
- Read-only workspace navigation/search foundation through `workspace_read_file`, `workspace_list_dir`, and `workspace_search_text` under explicitly configured trusted/stable roots.

### Recently Completed

- Observability-first coding-loop direction is selected and specified in
  `specs/2026-05-23-observability-first-coding-loop.md`. This corrects the
  previous event-first CLI direction: the next value gap is structured
  runtime/tool/process/provider logging with stable correlation fields, not a
  new CLI view. Event JSONL, future interactive CLI, and TUI are consumers of
  this observability layer, not the layer itself.
- Runtime/provider/tool execution MVP hardening moved into maintenance and foundation status.
- Provider output storage, pending tool calls, tool result resolution, tool continuations, registered tool execution, and public runtime export/rustdoc alignment have enough deterministic coverage to support the next runtime milestone.
- Memory Activation MVP moved into maintenance and foundation status for internal runtime use.
- Default memory activation is no longer noop: it is backed by a session-owned in-memory stored source. Public memory write APIs, external persistence, and a stable activation contract remain absent, so external/default sessions still start with no candidate memories.
- Memory activation tests cover validation, deterministic matching/scoring/conflict behavior, stored-source projection, evidence failures before provider calls, replacement/clearing behavior, pending-tool gating, cancellation/drop cleanup, and provider lifecycle retention/cleanup.
- M18F-G added a crate-internal provider-neutral uncertainty review harness with deterministic scripted-source coverage for evidence preflight, cancellation, source failures, outcome evidence validation, completed internal audit recording, and non-authoritative tool-risk advisory outcomes.
- M18F-H completed the internal summary-draft promotion safety path by adding a crate-private checked internal context append helper with candidate snapshot compilation before context mutation.
- M18F-I closed out public status and roadmap alignment for M18F without changing Rust behavior.
- Strict model judgment parsing and a crate-private provider-neutral `ModelBackedJudgmentSource` now exist for internal advisory tool-risk review, with deterministic fake-provider runtime harness coverage through `Runtime::run_uncertainty_review`.
- OpenAI Responses debug/tool flows remain opt-in manual verification paths, not deterministic test dependencies.
- Runtime agent loop contract hardening now gates public raw context writes behind active-step admission and maps loop-owned tool execution cancellation to a cancelled loop status without resolving the pending call.
- Runtime Agent Loop MVP first slice is implemented in `merry-runtime`: a bounded serial public loop composes `Runtime::step`, registered tool execution, and continuation steps, returns ordered events with typed completed/failed/cancelled/blocked outcomes, and keeps provider wire formats and real FS/shell tools out of runtime.
- `merry-tool-workspace` has moved from the read-file first slice into read-only workspace navigation/search as a separate tool crate exposing `workspace_read_file`, `workspace_list_dir`, and `workspace_search_text` under explicitly configured trusted/stable roots. It prevents ordinary path traversal and ordinary symlink traversal before read/list/search operations, and on Unix uses `O_NOFOLLOW` for file opens. It is not an OS sandbox and does not claim complete hardening against malicious concurrent filesystem mutation; residual TOCTOU risk remains. It is not a shell, write API, network API, or complete coding agent.
- CLI Sandbox Bootstrap is implemented in `merry-cli`: the root `--with-sandbox` flag uses `clap` and performs Linux `bwrap` self-reexec with a minimal environment, `PATH` lookup for `bwrap`, plan-stage missing-`bwrap` handling, recursion avoidance, sandbox-local `/tmp`, the current repo/project as the primary read-write workspace, and a minimal `/etc` allowlist. `/etc` file paths such as `/etc/ld.so.cache`, resolver, host, NSS, and `/etc/ld.so.conf` are mounted through a file helper that creates mount target parents before read-only binding; SSL/PKI and `/etc/ld.so.conf.d` use the directory helper. v1 still allows network access and is not a complete security boundary. A real smoke of `target/debug/merry --with-sandbox debug` has passed.
- Shell/Process SP1/SP2/SP3-A plus the latest CLI admission slices are implemented: `merry-runtime` has provider-neutral process intent/evidence, proposed/executed process action audit variants, explicit injected `ProcessRunner` boundaries, process intent classification, opt-in informational process admission, accepted local workspace process admission, bounded stdout/stderr result artifacts, payload-free proposal/execution evidence, default deny behavior, cancellation paths that keep pending calls unresolved until runner output exists, and deterministic fake-runner tests. `merry-runtime` now also exposes `TokioProcessRunner` as the runtime-owned Tokio process adapter; `merry-cli` reuses that adapter for the narrow debug/demo `merry shell -- <argv>` path. Informational `rustc --version` / `rg --version` can run, and exact `cargo test -p merry-runtime` requires accepted local workspace risk plus the CLI bwrap handoff and sandbox runtime evidence. This does not implement general shell/process/coding-agent capability, broad raw shell mode, arbitrary env/stdin, a complete sandbox proof, or a general approval/review admission UX.
- Minimal Useful Coding Loop first deterministic slice is implemented in `merry-tool-workspace` integration tests. `coding_loop_harness_inspects_patches_verifies_and_completes` builds a runtime with workspace read/patch tools plus `process_command_tool`, runs `Runtime::run_agent_loop` for inspect -> exact read -> patch -> verification -> final answer, uses a fake provider and injected fake process runner, mutates only a temporary workspace fixture through `workspace_patch_file`, records exact process argv for `rg --files` and `cargo test -p merry-runtime`, verifies tool-result continuation flow, and checks artifact-before-resolution ledger ordering. This is not yet the real `bwrap` smoke or live provider lane.
- Real `bwrap` coding-loop smoke is implemented in `merry-cli`: `merry --with-sandbox debug coding-loop-smoke` is an explicit non-default command that refuses to run without validated CLI bwrap child handoff evidence, creates a disposable fixture under `.merry/local/coding-loop-smoke`, composes a runtime with a deterministic scripted provider, workspace read/patch tools, and `process_command_tool`, then runs inspect -> exact read -> constrained patch -> real process verification -> final answer through `Runtime::run_agent_loop`. The process steps use runtime-owned `TokioProcessRunner` for real `rg --files` and `rg fixed-by-live-llm` inside the sandbox; the edit uses `workspace_patch_file`; the smoke validates `AgentLoopStatus::Completed`, no pending tool calls, four successful tool resolutions, and the patched fixture content. The integration test is ignored by default and passed in this environment with `cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`. This is still deterministic-provider and CLI-owned harness assembly, not a complete sandbox hardening claim.
- Configurable disposable coding-loop task smoke first slice is implemented in `merry-cli`: `merry --with-sandbox debug coding-loop-task-smoke --task status-text` creates a tiny disposable Rust fixture under `.merry/local/coding-loop-task-smoke`, drives inspect -> failing verification -> exact read -> constrained patch -> verification -> final answer through the runtime loop, and validates the final patched fixture. Default deterministic tests use a fake provider and fake process runner for `rg --files` / `rg done`; the real bwrap smoke remains explicit and ignored for outer-environment validation with `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`.
- Minimal Useful Coding Loop continuity Tasks 13-16 are closed for the current
  runtime foundation: request assembly has a stable prefix lane with project
  rules and tool-profile hashing, context budget/window resolution helpers
  exist, checkpoint decisions reserve a deterministic segment boundary, and
  final deterministic verification passed. Real `bwrap` and live OpenAI smokes
  remain explicit host-shell checks; nested `bwrap` inside Codex or another
  outer sandbox is an expected environment limitation, not completion evidence
  by itself.
- The opt-in live LLM coding-loop smoke command is implemented as `merry --with-sandbox debug coding-loop-live-smoke`. It refuses to run without the real CLI bwrap child handoff, uses `OpenAiProvider` for model decisions, keeps `TokioProcessRunner` and `workspace_patch_file` for the real tool path, and validates runtime events for process inspection, exact source read, patch, process verification, loop completion, and patched fixture content. The user reported that the credentialed live smoke passed against their trusted configured server. That run exposed a provider HTTP metadata gap, now fixed by setting `User-Agent: merry/<crate version>` in `merry-provider-openai`; deterministic request-construction and loopback integration tests cover the header.
- The first config-backed observability implementation slice is complete in
  `merry-cli`: XDG TOML config discovery, config-backed log settings, file
  tracing subscriber setup, sandbox config/log mount planning, host log
  directory creation before bwrap re-exec, and XDG TOML provider config for
  `debug openai` / `debug coding-loop-live-smoke`. The legacy live-smoke
  `--config .merry/secrets/openai.env` path is rejected.
- Runtime loop and process-action tracing are implemented in `merry-runtime`:
  the serial agent loop emits loop/step/tool start-finish records with session,
  step, tool-call, tool-name, artifact, status, and diagnostic-code fields; the
  process path emits argv/cwd, output byte counts, truncation flags, and status
  without logging stdout/stderr contents. Policy-denied process proposals emit
  one denied tool-finish trace and no process execution trace.
- Workspace tool and OpenAI-compatible provider trace alignment is implemented:
  workspace read/list/search/patch emit bounded safe start/finish traces for
  success, domain failure, invalid arguments, cancellation, and infrastructure
  errors; search logs query byte counts rather than query text; patch logs
  preimage/replacement byte counts rather than patch text; provider tracing
  records safe request metadata without API keys, prompts, provider wire
  payloads, or response payloads.
- A tracked user-facing config example exists at `examples/config.toml` and is
  parsed by deterministic CLI config tests. `AGENTS.md` now requires future
  config-key changes to maintain that example.
- End-to-end log-enabled smoke verification is implemented. A deterministic
  CLI-crate smoke enables file-backed JSON logs through XDG TOML config and
  proves runtime loop, provider request, workspace tool, process execution,
  artifact record, tool resolution, diagnostic-code, and completed terminal
  status records without raw prompt, source, process stdout, provider wire
  payload, or secret-like content. A real `bwrap` run of
  `target/debug/merry --with-sandbox debug coding-loop-smoke` with temporary
  XDG log config passed and produced the expected combined smoke log.
- Structured Process Boundary M1 is implemented. Process intent classification
  covers the first read-only inspection shapes, local workspace verification
  shapes, unknown requests, and forbidden patterns; admitted intents are routed
  through explicit process permission profiles; accepted local workspace
  execution stores and checks the construction-time `bwrap` admission profile;
  stdout/stderr/status output is artifact-backed and reduced into compact
  ledger facts with evidence references; and process execution traces include
  `permission_profile_id` without stdout/stderr payloads.
- M2 first shell-compatible boundary slice is implemented in `merry-runtime`:
  narrow plain read-only shell wrappers such as
  `bash -lc "rg ProcessRunner | wc -l"` derive
  `process.shell.read_only.v1`, remain separate from the structured
  `process.read_only.v1` argv lane, and execute only when runtime construction
  explicitly opts in with `allow_read_only_shell_process_actions`. Complex or
  mutating shell forms are denied without runner calls. This is classifier and
  admission plumbing only, not a general shell parser, not a model-facing shell
  tool, and not a reusable real shell runner profile yet.
- M2 shell input evidence and payload-free metadata slice is implemented in
  `merry-runtime`: admitted shell-wrapper process result artifacts include
  exact `input_evidence` with shell, flag, script text, script byte count, and a
  stable `fnv1a64` script fingerprint, without duplicating the same script in
  `intent.argv`. Shell process execution traces and compact ledger observations
  omit raw argv/script text and carry only
  shell/flag, byte count, fingerprint, status, output byte counts, and artifact
  references. This is still the existing result artifact path, not a separate
  pre-execution input artifact and not a real shell runner.
- M2 pre-execution shell input artifact slice is implemented in
  `merry-runtime`: admitted shell-wrapper process actions now record a
  runtime-owned `process-input-*` JSON artifact before the process runner is
  called. That artifact is the sole exact shell input carrier for the
  execution, containing shell, flag, script text, byte count, and stable
  fingerprint. The process result artifact references this input artifact via
  `input_artifact` and no longer duplicates the script under `input_evidence`.
  Compact ledger observations remain payload-free and refer to artifact ids plus
  fingerprints; traces remain payload-free and carry shell/profile/status/output
  metadata plus fingerprints instead of raw script text. Cancellation or runner
  infrastructure failure after input recording leaves the pending call
  unresolved, records no result artifact or action audit, but keeps the exact
  input artifact/evidence available in runtime state.
- M2 reusable real runner adapter slice is implemented: `TokioProcessRunner`
  moved from CLI-private code into `merry-runtime` and is exported as the
  runtime-owned process adapter. The CLI shell/debug smokes now reuse that
  adapter. A runtime provider-boundary test executes a real read-only shell
  wrapper pipeline, proves `process-input-*` is recorded before the result
  artifact, and proves the result references `input_artifact` without copying
  raw script payload. This still does not add a broad model-facing shell tool,
  approval/session semantics, arbitrary env/stdin, or a general shell
  authorization model.
- Roadmap drift correction is recorded: the shell/parser/profile direction was
  useful support work, but it had been promoted too far into `Next Active`.
  The active product proof is corrected back to testing coding-loop capability.
  Future roadmap priority changes require explicit user approval or a tracked
  change request; routine implementation status updates remain allowed.

### Active

- P0: continue the Minimal Useful Coding Loop as the current MVP proof. The loop now has a deterministic fake-provider/fake-runner slice, a deterministic real `bwrap` process-runner smoke, a configurable disposable task smoke, a user-verified live-provider smoke command, end-to-end config-backed log verification, and stable-prefix/context-budget/checkpoint scaffolding needed for dynamic context assembly.
- P0: keep the Runtime Coding Loop Harness executable against a disposable fixture repository. The default lane uses fake provider/fake runner for deterministic `cargo test`; the deterministic `bwrap` lane is an explicit ignored smoke; the live OpenAI-compatible lane is also explicit and ignored and has passed in the user's trusted configured environment.
- P0: move the next proof from smoke wiring to context fidelity. The
  configurable task smoke is in place; the next acceptance target is dynamic
  context assembly that keeps the stable prefix cacheable while projecting only
  compact, evidence-backed runtime facts late in the request.
- Supporting constraint: shell/process profile work remains subordinate to that
  coding-loop proof. Structured argv remains the narrow typed lane; richer
  shell syntax must run through a real interpreter inside explicit permission
  profiles, session grants, and sandbox constraints, but that design is not the
  next deliverable by itself.
- P0: keep process output artifact-backed and context-friendly. Accepted process actions should record stdout/stderr/exit metadata as artifacts before observable events claim them, reduce large output into compact ledger facts plus exact evidence references, and keep dynamic evidence late in compiled context so stable prefixes remain cacheable.
- P0: keep edit/write on the typed workspace patch path. The MVP loop should apply one constrained patch through `workspace_patch_file` or its runtime-owned successor, record artifact/audit/ledger evidence, and then run verification.
- Keep safety tiered but subordinate to the executable acceptance target: read-only inspection automatic, constrained patch opt-in, verification in `bwrap`, high-risk or unknown process actions denied or escalated.
- Keep CLI shell as smoke/debug, not the design owner. The main contract is the runtime library and its registered tool/profile set.
- Keep judgment advisory. Do not wire model-backed judgment to live provider, public events, ledger facts, or authorization until a concrete coding-loop acceptance test needs reviewer evidence.
- Keep default verification deterministic and offline. Live provider and
  host/sandbox process smokes are explicit opt-in host-shell checks with local
  credentials and disposable workspaces; nested outer-sandbox `bwrap` failures
  are not roadmap blockers by themselves.
- Improve public docs as implementation status changes, while keeping private notes under `docs/` and `merry-raw-docs/` ignored.

### Next Active

- Prepare the Dynamic Context Assembly slice before coding it. The next
  assistant turn should restate the intended implementation against
  `plans/2026-05-28-minimal-useful-coding-loop-continuity.md`, the stable
  prefix layout, and this roadmap, then wait for user confirmation before
  runtime edits.
- Implementation target: assemble the provider-request dynamic body from
  runtime-owned state while preserving the stable prefix lane. Dynamic context
  should include the current task/user request, append-only conversation body,
  pending tool continuations, and compact ledger/artifact evidence references.
- Keep large or exact payloads out of default prompt text. Process
  stdout/stderr, source reads, patch payloads, provider wire payloads, and
  artifact bodies stay in artifacts; the dynamic body carries bounded facts,
  ids, diagnostics, fingerprints, and evidence locators.
- Acceptance: deterministic runtime/provider-boundary tests prove dynamic
  evidence changes do not change stable prefix hash, relevant facts appear late
  in compiled context, soft/hard budget decisions use the existing
  `ContextBudget` / `ResolvedContextWindow` / `CheckpointDecision` helpers,
  and default `cargo test` remains offline.
- Out of scope for this next slice: model-written checkpoint summaries,
  provider conversation state, `previous_response_id`, live OpenAI judgment,
  new shell/session authorization design, `/task` or TUI commands, arbitrary
  absolute-path editing, and broader process permissions.
- Roadmap priority changes require explicit user approval or a tracked change
  request. Agents may update completion/status evidence, but must not promote
  policy/profile/classifier work into `Next Active` on their own.

### Drift Audit: 2026-05-28

Roadmap history shows the drift point:

- `5426896 project-continuity: recalibrate mvp roadmap` set the P0 to the
  Minimal Useful Coding Loop.
- `ca0a4b2 project-continuity: add coding loop harness`, `e53f721
  project-continuity: add bwrap coding loop smoke`, and `3d0aeb9
  project-continuity: add live coding loop smoke` advanced that executable
  proof.
- `e945ebc docs: reframe shell boundary away from parser allowlists` made a
  correct architectural correction, but also moved `Next Active` toward M2
  shell boundary work. From there, `32fd2af`, `d78c4ef`, and `7680eb3`
  continued useful shell/profile support while the roadmap language made that
  support look primary.

Correction: keep the shell/process boundary work as supporting architecture and
restore the next executable target to testing coding ability through a less
scripted, user-testable coding-loop smoke.

### Next Milestone Ladder: Shell/Process Boundary With Artifact-Backed Context Reduction

Goal: make ordinary process/shell-style inspection and verification a reusable
runtime-owned boundary without turning raw tool output into prompt history or
expanding a catalog of one-off read/search tools.

Shared invariants:

- Runtime owns policy, state, audit, artifacts, ledger, reducers, and context
  compilation.
- Permission profiles describe filesystem, network, and side-effect capability.
- Tool profiles describe stable model-visible tool sets and schema/cache lanes.
- Command classifiers describe concrete process risk and feed action policy;
  they are not authorization by themselves.
- Do not build a subset shell parser as the authorization model. Shell syntax
  is delegated to a real shell runner under permission profiles and sandbox
  constraints; classifiers can only narrow, deny, or explain risk.
- Pipes and shell control flow are legitimate process-composition mechanisms.
  Do not force them into separate model tool calls merely to preserve an argv
  allowlist.
- Process output becomes artifacts before observable events, ledger facts, or
  final answers claim it.
- Summaries are navigation; exact evidence must remain retrievable through
  artifacts or source reads.
- Stable context prefix should stay stable; dynamic ledger/evidence context goes
  late in the compiled model request.
- Default verification stays deterministic and offline. Real `bwrap` and live
  provider lanes remain explicit opt-in smokes.

M0 Direction Correction:

- Correct public wording away from "read-only process profile".
- Document the permission profile, tool profile, command classifier,
  artifact/evidence/ledger/reducer, and context compiler boundaries.
- Keep private design notes under ignored `docs/`.

M1 Structured Process Boundary MVP:

- Status: complete.
- Extend command classification for `rg --files`, literal `rg <pattern>`,
  exact source slices such as `sed -n RANGE FILE`, safe read-only git commands,
  `cargo test`/`cargo check` as local workspace effects, unknown requests, and
  forbidden patterns.
- Route classified intents through read-only and workspace-write/sandbox
  permission profiles.
- Record process stdout/stderr/exit metadata as artifacts.
- Reduce process artifacts into compact ledger facts plus exact evidence refs.
- Test with deterministic fake providers and fake runners.

M2 Shell-Compatible Runtime Boundary:

- Status: in progress; the read-only shell-wrapper admission slice and the
  shell input artifact / payload-free trace+ledger metadata / real runner
  adapter slices are implemented.
- Add a shell-compatible execution boundary after the structured intent path is
  solid, but do not emulate full shell parsing in Merry.
- Keep `process.shell.read_only.v1` separate from `process.read_only.v1`.
  Recognizing a plain read-only pipeline must not silently grant shell
  execution to existing structured argv runners.
- A future model-facing `shell_command` or `exec_command` tool should be a thin
  request shape over this runtime boundary, not a parser that reclassifies
  shell grammar into structured argv for authorization.
- Treat command/script text as execution input evidence. The current MVP stores
  exact shell input in a standalone pre-execution `process-input-*` artifact and
  omits duplicate `intent.argv` and result-artifact `input_evidence`; result
  artifacts reference the input artifact by id/kind. Traces and compact ledger
  observations record only shell, flag, byte count, stable fingerprint, status,
  output metadata, and artifact references.
- Execute shell syntax through a real shell runner inside explicit
  permission/session profiles. Pipelines, conditionals, and small scripts are
  allowed only when the selected profile and sandbox make their side effects
  acceptable, or when an approval/session grant admits them.
- Keep default behavior fail-closed for ungranted shell execution. Static
  classifiers may provide hard-deny/advisory risk evidence, but broad shell
  authorization comes from profiles, sandboxing, and approvals.

M3 Approval And Permission Session:

- Represent approval requests, grants, denials, timeouts, and cancellations as
  runtime events/artifacts/ledger facts.
- Support bounded session approvals or prefix rules where policy allows them.
- Keep reviewer-model output as policy evidence only, never as authorization.

M4 Richer Shell Capability:

- Add stdin/input artifacts, env policy, long-running sessions, `write_stdin`,
  timeout, cancellation, and output range rehydration incrementally.
- Let real shell profiles carry pipelines and control flow instead of
  reimplementing them as Merry syntax. Require artifact, audit, cancellation,
  approval, and reducer coverage as profiles become broader.
- Keep shell write side effects out of the default edit path; typed patch or
  apply-patch remains the preferred edit mechanism.

M5 Reusable Coding Runtime Construction:

- Move coding-loop tool/profile/runner/reducer registration into reusable
  runtime or library construction.
- Make deterministic harnesses, real `bwrap` smokes, and live smokes use the
  same construction path.

### Completed Milestone: Observability-First Coding Loop

Goal: make Merry's already-proven sandboxed/live coding loop observable before
adding another interaction surface. The operator should be able to run the
existing deterministic and live coding-loop smokes with logs enabled and see
what happened: runtime loop boundaries, provider boundary metadata, model tool
choices, tool execution, process argv/cwd/exit evidence, artifact IDs, failure
or cancellation diagnostics, and final loop status.

Spec:

- `specs/2026-05-23-observability-first-coding-loop.md`

Acceptance target:

```text
log-enabled smoke:
  config: `$XDG_CONFIG_HOME/merry/config.toml` or `~/.config/merry/config.toml`
  example command remains: `merry --with-sandbox debug coding-loop-smoke`
  log enablement, level, format, and path come from TOML config
  `--with-sandbox` mounts the resolved Merry config directory read-only
  file logs use configured or default XDG state/log directory
  logs runtime loop start/finish and each step boundary
  logs provider request metadata without provider wire payloads
  logs tool pending/execution/result status with tool_call_id and tool_name
  logs process argv/cwd/exit status and stdout/stderr byte counts
  logs artifact IDs and diagnostic codes where applicable
  fails closed when sandbox or required provider/model config is missing

default tests:
  config and tracing capture tests use deterministic fake provider/fake runner
  no bwrap, network, or live credentials required
  log assertions cover completed, failed, cancelled, and blocked loops
```

Completed first slice:

- XDG config discovery and TOML parsing for global, observability, default
  model, and provider settings.
- Sandbox mount planning so `--with-sandbox` exposes the Merry config directory
  read-only and only exposes a log/state path when file logging is enabled.
- CLI-owned `tracing-subscriber` setup driven by config, not by new logging
  command-line flags.
- XDG TOML provider config for OpenAI-compatible debug and live-smoke paths,
  including config-relative `api_key_file` support.

Completed second slice:

- Runtime loop, step, pending-tool, tool execution, terminal status, and
  process execution traces with stable correlation fields.
- Denied process-action traces that record `status = "denied"` and diagnostic
  code `action_policy_denied` without emitting process start/finish records.
- Deterministic runtime trace-capture tests for completed process execution,
  policy denial, and executor infrastructure error paths.

Completed third slice:

- Workspace read/list/search/patch tools emit `runtime.workspace_tool.start`
  and `runtime.workspace_tool.finish` traces with `tool_call_id`, `tool_name`,
  status, diagnostic code, output byte count where applicable, and bounded
  action summaries that avoid file contents, raw search queries, and patch
  text.
- OpenAI-compatible provider tracing records safe request metadata through
  `runtime.provider.request` and keeps the provider stream span separate as
  `runtime.provider.stream`.
- Deterministic workspace/provider tests cover redaction, bounded summaries,
  invalid arguments, cancellation after start, domain failures, and
  request-render metadata without secrets or prompt text.

Completed fourth slice:

- Runtime now emits provider-neutral `runtime.provider.request` metadata before
  calling any provider, so deterministic/scripted providers are observable
  without exercising a live adapter.
- Runtime artifact writes emit `runtime.artifact.record` after artifact state is
  written and before the observable event path claims the artifact.
- A deterministic CLI-crate smoke covers the combined coding loop log with
  file-backed JSON logging from XDG TOML config and asserts no raw prompt, file
  content, process stdout, model final text, provider wire payload, or
  secret-like value leaks into the log.
- Default CLI integration tests cover the default XDG state log path and clear
  failure when the log parent cannot be created.
- The existing deterministic `bwrap` and live provider smokes remain explicit
  and non-default; the deterministic `bwrap` smoke has passed with temporary
  XDG log config.

Non-goals:

- Do not build a full-screen TUI in this milestone.
- Do not add a new REPL or multi-turn prompt UI in this milestone.
- Do not turn the CLI into a general autonomous coding agent.
- Do not add arbitrary shell parsing, pipelines, inherited env, stdin, network
  tools, or broad filesystem writes.
- Do not make live provider behavior part of default tests.

Verification:

- `cargo fmt --all --check`
- focused deterministic cargo tests for config-backed logging setup and
  runtime/process tracing capture
- existing opt-in real bwrap smoke may remain ignored/manual
- existing opt-in live provider smoke may remain ignored/manual

### Continuing Acceptance Skeleton: Minimal Useful Coding Loop Harness

Goal: prove Merry's runtime value with one small coding-style task that performs inspection, exact evidence retrieval, constrained edit, verification, continuation, and final answer through runtime-owned events and artifacts.

Acceptance target:

```text
fake-provider default test:
  model step 1 requests workspace inspection (`rg --files` or workspace list)
  model step 2 requests exact evidence (`rg <literal>` / file slice / read tool)
  model step 3 requests one constrained workspace patch
  model step 4 requests verification
  model final step returns an answer

runtime evidence:
  exact command/tool intent is recorded
  stdout/stderr or file outputs are artifacts
  patch proposal/execution evidence is recorded
  verification result is an artifact
  tool continuations are provider-neutral
  event order is deterministic
  ledger facts prove state-before-event ordering
```

Real smoke target:

```text
opt-in bwrap smoke:
  runs the same disposable fixture repo inside `merry --with-sandbox`
  mounts only the fixture/workspace path plus sandbox-local temp
  uses no inherited secrets
  confirms the patch and verification behavior with a real process runner
  current CLI smoke command: `merry --with-sandbox debug coding-loop-smoke`

opt-in live provider smoke:
  current CLI smoke command: `merry --with-sandbox debug coding-loop-live-smoke`
  reads XDG TOML provider config inside the sandbox
  requires `MERRY_OPENAI_DEBUG=1`
  requires `[providers.default]` model config unless `--model` overrides it
  requires `[providers.openai-compatible]` with exactly one of `api_key` or `api_key_file`
  optionally uses `[providers.openai-compatible].base_url`
  is never part of default `cargo test`
```

Tasks:

- Add a fixture repository purpose-built for the loop, with a tiny failing behavior or deterministic text replacement target. The first deterministic slice uses a temporary workspace fixture in `merry-tool-workspace` tests; a reusable real bwrap fixture remains.
- Add a harness command or integration test wrapper that builds a runtime with the coding-loop tool set. The first deterministic integration test exists; the first real bwrap CLI smoke wrapper exists.
- Add a structured shell/process boundary or equivalent reusable admission layer for file listing, literal search, exact source slice retrieval, and local verification, with process output reduced into artifact-backed ledger/evidence.
- Register the runtime-owned default coding-loop tools from library code, not by ad hoc CLI-only assembly.
- Use `workspace_patch_file` or its successor for the edit step and keep shell side effects out of the edit path. The first deterministic slice now does this.
- Add deterministic fake-provider/fake-runner tests for the full multi-step loop. The first slice now covers inspect, exact read, patch, verification, continuation, and final answer.
- Keep XDG TOML config guidance for live provider credentials and base URL.
- Run the explicit, non-default live smoke command with local credentials and treat any model deviation as evidence for the next smallest runtime/tool-contract fix.

Non-goals:

- Do not implement a full autonomous coding agent.
- Do not add arbitrary shell parsing, pipelines, inherited env, stdin, network tools, or broad filesystem writes.
- Do not make live provider behavior a default test dependency.
- Do not make risk taxonomy or reviewer models the milestone output unless they unblock this loop.
- Do not build graph memory, skill VM, Python SDK, or subagent runtime in this milestone.

Verification:

- `cargo fmt --all --check`
- focused deterministic cargo test for the coding-loop harness and task smoke
- opt-in real bwrap smoke command: `cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`
- opt-in real bwrap task smoke command: `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`
- opt-in live provider smoke command: `cargo test -p merry-cli debug_coding_loop_live_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`

### Follow-On Milestone: Approval, Risk Taxonomy, and Role-Scoped Models

Goal: define the runtime-owned risk taxonomy and role-scoped model configuration direction before Workspace Patch/Write and Shell/Process Protocol become full implementation milestones.

Tasks:

- Define action risk categories such as `ReadOnly`, `EditLow`, `EditElevated`, `ProcessLow`, `ProcessHigh`, and `Forbidden`, or equivalent names.
- Define the policy meaning of low-risk automatic edit as a classified patch/edit action, not blanket file-write authority.
- Define shell/process policy tiers around the sandbox assumption: read-only automatic, medium local effects automatic only inside sandbox and/or after user risk acceptance, high-risk ask/reviewer, and critical deny.
- Define hard-policy requirements for high-risk shell/process actions, including explicit approval and/or review-role LLM evidence where policy requires it.
- Define the uncertain-risk path where runtime policy can require review-role LLM evidence and/or explicit approval before admission.
- Specify fail-closed behavior for review timeout, provider/source failure, schema/parse failure, and insufficient evidence.
- Reserve internal role-scoped model configuration for roles such as `Primary`, `ToolRiskReview`, `ApprovalReview`, and `SummaryMemory` without exposing a stable public API or provider conversation state.

Reviewer evidence contract:

- A reviewer may only produce structured risk evidence, such as risk class, reason, recommendation, confidence, and evidence references.
- A reviewer recommendation is never directly executable. It cannot authorize an action, enlarge runtime capability, or bypass sandbox state, active profile limits, approval policy, or hard-policy denial.
- Runtime policy is the authorization owner. It decides admission by combining sandbox state, active profile, approval policy, hard deny rules, explicit user approval, and reviewer evidence.
- User approval is an authorization source when policy accepts it; reviewer output is not.
- Reviewer timeout, provider failure, schema/parse failure, low confidence, or insufficient evidence must fail closed or escalate to human approval. These cases must not become automatic allow paths.
- Hard deny examples include network pipe-to-shell, secret probing or exfiltration, privilege escalation, and sandbox escape attempts. Runtime policy must deny these even if reviewer evidence recommends allow.
- Medium-risk example: `cargo test --all` may use reviewer output as evidence, but allow conditions must come from runtime policy, such as sandbox present, user accepted medium risk, and reviewer confidence meeting the configured threshold.

### Follow-On Milestone: Shell/Process Primary Actuator Protocol

Goal: continue shell/process as the primary coding-agent actuator protocol without overstating the implemented surface. The implemented surface includes provider-neutral runtime protocol values, injected runner boundaries, narrow informational process admission, accepted local workspace process admission, and a debug/demo CLI real runner path; it does not implement general shell/process/coding-agent capability. A future model should be able to compose normal process tools such as `rg`, `sed`, `cargo`, `git`, pipelines, and small scripts while Merry owns the policy, risk review, audit, artifact, cancellation, and approval boundaries around those actions. This protocol should build on the `merry --with-sandbox` bootstrap assumption for v1 shell work instead of assuming bare host execution.

Tasks:

- Define runtime-owned process action records for command intent, working directory, environment policy, stdin/input artifacts, stdout/stderr/output artifacts, exit status, timing, and cancellation result.
- Define admission through Action Policy risk classes, including approval and review-role evidence requirements for high-risk or uncertain actions.
- Preserve tiered shell/process policy: read-only automatic, medium local effects automatic only inside sandbox and/or after user risk acceptance, high-risk ask/reviewer, and critical deny.
- Treat review-role LLM output as evidence only. Hard runtime policy decides whether evidence and approval are sufficient to admit or reject an action.
- Preserve open shell/process composition under runtime control instead of building an exhaustive set of built-in CLI-shaped tools.
- Record process input/output through artifacts and ledger/checkpoint-aware state before observable runtime events claim them.
- Keep edits on the typed patch/apply-patch path with evidence and audit records rather than treating shell write side effects as the primary edit mechanism.
- Keep deterministic verification provider-neutral with fake process runners, fake providers where needed, stored runtime state, artifact references, and ledger assertions.
- Preserve the runtime/provider boundary: no direct `std::process::Command` in runtime call paths, no lossy raw-shell-to-argv splitting, no reviewer-as-authorization, and no automatic admission for stdin/env expansion until execution evidence covers those inputs. Concrete OS adapters belong at outer layers such as `merry-cli`, not in `merry-runtime`.

Non-goals:

- Do not claim general shell/process/coding-agent capability is implemented.
- Do not replace ordinary shell/process composition with a growing catalog of built-in read/search/edit tools.
- Do not treat the protocol as only a deny gate; it must also define auditable, cancellable, bounded execution for admitted actions.
- Do not treat the CLI shell path as raw shell mode; it accepts exact argv only and does not support pipelines or scripts.
- Do not treat the CLI sandbox/admission lane as complete containment or proof: repo-local destructive effects remain possible, v1 network access is allowed, and the current lane is not a general approval/review admission system.
- Do not use live provider behavior, OpenAI state, network access, or provider conversation state as deterministic verification dependencies.
- Do not deprecate the existing read-only workspace tools; they remain foundation, bootstrap, fallback, and maintenance capabilities.

Verification:

- Deterministic fake-process tests for admission, rejection, cancellation, output capture, artifact ordering, ledger/checkpoint assertions, and failure modes.
- Deterministic fake-provider or scripted-source tests only where model-role evidence is needed.
- No live-provider, network, or host-specific command dependency for required tests.

### Deferred

- Production memory store, public Memory Activation APIs, external persistence, and stable activation contract.
- Broaden OpenAI Responses API provider coverage beyond the first streaming/text/function-call slice as runtime policy expands.
- Live LLM-backed judgment path, public judgment API, public runtime events/ledger facts for judgment or promotion, tool execution gate integration, automatic provider-context inclusion, automatic context mutation or promotion, and builder/runtime configured judgment source.
- Python SDK and `merry-py`.
- Rust facade crate `merry`.
- Macro crate support for boilerplate generation.
- Collaboration and subagent runtime support beyond reserved public contracts.
- Network workspace tools and full coding-agent runtime behavior. The current workspace tool slice remains read-only navigation/search only, write work moves through runtime-owned protocols first, and the implemented shell/process slice remains narrow rather than a general coding-agent capability.

## Adopted Engineering Decisions

- Rust 2024 virtual Cargo workspace with resolver 3.
- Initial crates:
  - `merry-core`
  - `merry-llm`
  - `merry-runtime`
  - `merry-tool-workspace`
  - `merry-provider-openai`
  - `merry-cli`
- Deferred crates:
  - `merry-macros`
  - `merry-py`
  - Rust facade crate `merry`
- Tokio is the MVP async runtime.
- Runtime event APIs are stream-first.
- Public dyn async boundaries use explicit boxed futures/streams.
- PyO3/maturin comes after the Rust event loop is stable.
- MVP OpenAI provider target is the Responses API through a Merry-owned adapter boundary and direct `reqwest`; the current provider implementation uses `/responses` with typed SSE parsing.

## Completed Milestones

### Milestone 1: Workspace Skeleton

Goal: establish the repository shape and compile an empty workspace.

Tasks:

- Add root virtual `Cargo.toml`.
- Add five initial crates.
- Configure workspace package metadata, dependencies, and lints.
- Forbid unsafe at workspace lint level.
- Add minimal crate docs.
- Verify:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

### Milestone 2: Core Protocol Types

Goal: define the stable vocabulary shared by runtime, provider adapters, and CLI.

Tasks:

- Add typed IDs such as `SessionId`, `ArtifactId`, `ToolName`, `SkillId`, and `ProviderName`.
- Add `RuntimeEvent`.
- Add artifact and evidence references.
- Add `ToolSpec` and schema-facing structs.
- Add typed core errors.
- Add serialization tests for public protocol types.

### Milestone 3: Provider Boundary

Goal: define model/provider contracts without binding runtime to any provider API.

Tasks:

- Add `ModelProvider` trait in `merry-llm`.
- Add `ModelRequest`, `ModelResponse`, `ModelEvent`, `ModelCapabilities`, and `Usage`.
- Use stream-first provider events.
- Add a fake provider for deterministic tests.
- Add provider boundary tests that prove no OpenAI wire types are required by runtime.

### Milestone 4: Runtime Skeleton

Goal: make `Runtime::step` emit deterministic events without a live model.

Tasks:

- Add `RuntimeBuilder`.
- Add session state skeleton.
- Add in-memory ledger skeleton.
- Add in-memory artifact metadata skeleton.
- Add bounded event stream output.
- Add cancellation token support.
- Add deterministic tests for event order and cancellation.

### Milestone 5: CLI Debug Surface

Goal: provide a simple way to inspect the runtime event stream.

Tasks:

- Add `merry` debug binary in `merry-cli`.
- Run deterministic runtime skeleton.
- Print events as JSON lines.
- Add smoke tests where practical.

### Milestone 6: Provider Adapter Skeleton

Goal: prepare the OpenAI provider adapter boundary without depending on live provider tests. The provider now uses the Responses API path and private Responses wire types.

Tasks:

- Add provider config types.
- Add private wire structs.
- Add request rendering from Merry-owned model types.
- Add response/event parsing unit tests from static fixtures.
- Keep live network tests behind explicit opt-in.

### Milestone 7: Context, Ledger, Artifact Loop

Goal: connect structured state to compiled context and artifact references.

Tasks:

- Add task ledger update primitives.
- Add artifact write/read references.
- Add context compiler skeleton.
- Add snapshot-style tests for compiled context.
- Ensure summaries never replace required exact evidence in compiler tests.

## Recently Completed Milestone

### Milestone 8: Runtime/Provider/Tool Execution MVP Hardening

Goal: make the implemented provider step and tool execution loop robust enough to support later memory, SDK, and collaboration work.

Tasks:

- Keep provider output stored as artifacts before observable runtime events claim it.
- Keep tool call, tool result, and continuation behavior reproducible from runtime state.
- Keep registered tool execution bounded by explicit runtime policy and artifact ownership.
- Keep public runtime exports/rustdoc aligned with implemented runtime/provider/tool behavior.
- Preserve deterministic tests around fake providers and local tool execution.
- Keep OpenAI Responses debug/tool flows explicit opt-in paths for manual verification only.

### Milestone 9: Memory Activation MVP

Goal: prove structured memory enters context through activation, not chat history.

Memory Activation MVP is internally integrated in `merry-runtime`. The default source is a session-owned in-memory stored source. This does not imply production memory storage, public memory APIs, external persistence, or a stable activation contract; external/default sessions have no candidate memories until runtime-owned state records them.

Done:

- Define internal activation data shapes before public runtime APIs.
- Add session-owned in-memory candidate storage and deterministic stored activation source.
- Add deterministic projection, scoring, scope, trigger, confidence, priority, conflict, and evidence validation.
- Add provider-step timing so activated memory is projected before model requests.
- Record why activated memory entered context.
- Validate lifecycle behavior for replacement, clearing, pending-tool gating, cancellation/drop cleanup, and provider setup/stream completion paths.

Not included:

- Public Memory Activation API surface.
- External persistence or production memory backend.
- Stable activation contract for external consumers.
- External/default candidate memories.

## Closed Milestone

### M18F-A / M18F-B / M18F-C / M18F-D / M18F-E / M18F-F / M18F-G / M18F-H: LLM-Assisted Judgment Boundary

Goal: reserve an internal runtime boundary and audit carrier for semantic judgment without giving judgment authority over runtime policy.

M18F-A established the crate-internal contract skeleton in `merry-runtime`. M18F-B adds a crate-internal completed-judgment audit registry with exact internal request/outcome payload carriers. M18F-C wires the first narrow summary-draft audit path through a crate-private helper that records completed advisory `SummaryDraft` judgments only after artifact evidence validation. M18F-D adds an internal explicit acceptance and promotion boundary for accepted summary drafts; promotion is still crate-private, validates exact selected evidence, and compiles a candidate context snapshot before mutation. M18F-E adds a session-owned internal promotion lifecycle registry: exact promoted replays are idempotent no-ops, conflicting payloads are rejected without context mutation, and compile failures become terminal rejected records. M18F-F characterizes the public direct context write boundary: `Runtime::record_context_entry` and `Runtime::record_context_summary` remain raw/manual MVP append helpers with delayed context-compile validation, not summary-draft promotion and not lifecycle-governed. M18F-G adds a crate-internal provider-neutral uncertainty review harness that preflights request evidence, invokes `JudgmentSource` without holding session state across await, validates outcome evidence before commit, and records exactly one completed internal audit payload on success. M18F-H extracts the summary-draft promotion candidate compile-before-mutation path into a crate-private checked internal context append helper; public direct context writes remain raw/manual. Judgment outcomes are advisory semantic evidence; they cannot authorize tool execution, actions, or context mutation. Provider wire formats do not enter runtime, and summary/evidence exact artifact rules remain unchanged.

M18F-I closed this milestone as documentation/status alignment only. Later internal foundation work added strict model-output parsing, a crate-private provider-neutral `ModelBackedJudgmentSource` for advisory tool-risk review, and deterministic fake-provider runtime harness coverage through `Runtime::run_uncertainty_review`. That source remains internal, fake-provider deterministic only, and not wired to a live provider or public runtime configuration.

Done:

- Define internal purpose, provenance, confidence, evidence, request, recommendation, outcome, context, source trait, and typed error shapes.
- Add object-safe boxed-future source boundary and deterministic noop source.
- Add unit tests for validation, evidence requirements, object-safe calls, advisory noop behavior, and cancellation context.
- Add internal completed-only judgment record ids, deterministic registry snapshots, and exact internal request/outcome payload artifacts.
- Add session-private judgment recording helpers that validate request/outcome evidence against session artifacts before writing the internal registry.
- Add a crate-private summary-draft judgment helper and deterministic tests proving recorded drafts stay out of compiled context, ledger projection, runtime event sequence, and pending-tool state.
- Add crate-private summary-draft acceptance and promotion helpers that reject LLM authority, require exact draft text match, require selected judgment evidence, and leave context unchanged on validation failure.
- Add a crate-internal summary-draft promotion lifecycle registry with deterministic snapshots, promoted exact-replay idempotency, payload conflict detection, and rejected-record replay protection.
- Add characterization coverage and docs for public direct context writes as raw/manual MVP context mutation outside the summary-draft promotion lifecycle.
- Add a crate-private uncertainty review harness with deterministic scripted-source tests for request preflight, cancellation, source error, outcome evidence validation, exact internal audit recording, and non-authoritative high/unknown tool-risk advisory results.
- Add a crate-private checked internal context append helper used by summary-draft promotion, with candidate snapshot compilation before session context mutation.
- Add a strict model judgment parser and crate-private provider-neutral `ModelBackedJudgmentSource` for internal advisory tool-risk review, with deterministic fake-provider runtime harness coverage through `Runtime::run_uncertainty_review`.

Closed-out guardrails:

- Keep the boundary internal while runtime policy integration is designed.
- Preserve the advisory/hard-policy split in docs, names, and storage boundaries.
- Keep summary-draft audit and promotion internal, with no record-id-authorized or automatic context promotion.
- Keep promotion lifecycle state out of public runtime APIs, runtime events, ledger facts, and tool-call policy.
- Keep public direct context write behavior unchanged while it remains a raw/manual MVP surface.
- Keep model-backed judgment out of OpenAI/live-provider paths, public runtime configuration, public events, ledger facts, tool gates, automatic provider-context inclusion, and automatic context mutation or promotion.

Still absent:

- Live LLM-backed or OpenAI-backed judgment path.
- Public judgment API.
- Public summary-draft recording or promotion APIs.
- Builder/runtime configured judgment source.
- Tool execution gate integration.
- New public `merry-core` event, id, or reference types.
- Public runtime events or ledger facts for judgments or promotions.
- Tool-call policy changes.
- Automatic provider-context inclusion from judgment drafts.
- Automatic summary-draft or judgment-based promotion.

## Deferred Milestones

### Milestone 10: Python SDK Shell

Goal: expose the runtime event API to Python.

Tasks:

- Add `merry-py` crate.
- Add mixed maturin layout.
- Expose Rust module as `merry._merry`.
- Add Python package wrappers under `python/merry`.
- Expose async event iteration as the primary Python API.
- Keep Python tool execution as event bridging.

### Milestone 11: Collaboration Contract Skeleton

Goal: reserve the runtime shape for future subagents without implementing full orchestration.

Tasks:

- Add `AgentTask` contract type.
- Add parent/child session references.
- Add collaboration event variants.
- Add artifact ownership metadata.
- Add basic merge policy type.
- Add tests that prove subagent work can be represented as bounded tasks.

## Execution Model

Each milestone should be decomposed into small implementation tasks. Prefer:

- one implementer subagent per independent task
- spec review after implementation
- Rust code quality review before merge
- focused commits per milestone

Research tasks should precede implementation when decisions affect async behavior, provider APIs, PyO3, storage, memory providers, or subagent scheduling.
