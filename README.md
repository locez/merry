# Merry

Merry is an early-stage Rust-first agent runtime project. APIs and crate boundaries are still unstable.

## Current Coding-Agent Boundary

Merry is not yet a full coding-agent runtime. The current workspace tool
surface is a read-only evidence and navigation foundation, bootstrap,
fallback, and maintenance path. It is not the main actuator for coding-agent
behavior.

The current roadmap P0 is the Minimal Useful Coding Loop: run a small
coding-style task through runtime-owned state, tools, artifacts, continuations,
and verification. The target loop inspects a disposable fixture repository,
reads exact source evidence, applies one constrained workspace patch, runs
verification inside the CLI `bwrap` sandbox, and returns a final answer backed
by runtime events, artifacts, and ledger facts.

Policy and shell/process design remain important, but they are support work for
that executable acceptance target. Merry now has configuration-backed
observability for the runtime-owned tool and action surface that already proves
the loop: XDG/TOML logging can show runtime loop boundaries, provider request
metadata, tool choices, process execution, artifact IDs, diagnostics,
cancellation, and final loop status while deterministic and live smokes run.
The next boundary is a runtime-owned shell/process boundary with permission
profiles, stable tool profiles, command classification, and artifact-backed
context reduction rather than another one-off CLI assembly path.

Shell/process remains the primary verification and inspection actuator under
runtime control. A future model should be able to compose ordinary process
tools such as `rg`, `sed`, `cargo`, and `git` through runtime policy and command
classification rather than through one-off CLI-only matches or a growing
catalog of built-in read/search tools. The runtime should own policy, risk
review, audit, artifacts, cancellation, approval, ledger updates, and context
compilation. The CLI shell path should remain a smoke/debug adapter rather than
the design owner.

In this roadmap, a permission profile describes filesystem, network, and
side-effect capability. A tool profile describes the stable model-visible tool
set and schema/cache lane for a task. A command classifier maps a concrete
process or shell request to a risk category that action policy can allow, deny,
or route to approval. These are separate concepts and should not be collapsed
into command-family-specific profiles.

The implemented shell/process surface is still narrow and split by boundary:
`merry-runtime` owns provider-neutral `ProcessActionIntent` and
`ProcessExecutionEvidence` protocol values, action-audit variants for proposed
and executed process actions, the intent classifier, and injected
`ProcessRunner` admission lanes. The runtime crate does not directly spawn OS
processes and has no concrete OS process adapter. `merry-cli` has a debug/demo
`merry shell -- <argv>` path with a real runner adapter built on
`tokio::process::Command`. That CLI path routes exact argv through the runtime
process protocol and prints runtime JSONL events with artifact-backed tool
results; raw process stdout is not printed directly as CLI stdout.

Low-risk informational commands such as `rustc --version` and `rg --version`
can run through the CLI shell path. A local workspace effect such as exact
`cargo test -p merry-runtime` requires explicit
`--accept-local-workspace-process-risk`, the CLI `bwrap` child handoff, and
sandbox runtime evidence. Default host execution, environment-spoofed
sandbox markers, forged hidden handoff markers, forbidden commands, and unknown
commands remain denied.

This is not a general shell or coding-agent capability. There is no raw shell
string parsing, pipeline/script execution, arbitrary environment or stdin
support, complete sandbox/security/provenance proof, or general approval/review
admission UX. Reviewer or LLM output remains evidence for policy; it is not
authorization.

The CLI Sandbox Bootstrap slice is implemented in `merry-cli`: the root
`--with-sandbox` flag uses `clap` and performs Linux `bwrap` self-reexec before
normal CLI execution. The v1 behavior keeps a narrow sandbox assumption:
minimal explicit environment, `PATH` lookup for `bwrap`, plan-stage
missing-`bwrap` handling, recursion avoidance, sandbox-local `/tmp`, the
current repo/project as the primary read-write workspace, and a minimal `/etc`
allowlist including `/etc/ld.so.cache`, resolver/host/NSS files, and SSL/PKI
paths. v1 still allows network access and is not a complete security boundary;
repo-local destructive effects remain possible. A real smoke of
`target/debug/merry --with-sandbox debug` has passed.

Action Policy needs a first-class risk taxonomy before write or process actions
become normal runtime capabilities. The intended direction is to classify
candidate actions into policy-owned levels such as `ReadOnly`, `EditLow`,
`EditElevated`, `ProcessLow`, `ProcessHigh`, and `Forbidden`. Automatic edit
should only mean that a runtime policy classified a concrete patch/edit action
as low risk. It must not mean arbitrary file writes are automatically allowed.
Capability boundaries and approval/review policy are separate concerns: a
reviewer or approval policy can add required evidence or approval, but cannot
expand what the runtime capability boundary allows.

Shell/process support is not being rejected as out of scope. The direction is to
bring it into a runtime-owned reliable audit path: policy-gated,
artifact-backed, ledger/checkpoint-aware, cancellable, bounded,
deterministic-testable, and provider-neutral. The protocol should support open
composition of ordinary shell/process tools under runtime audit and control. It
is not a deny-only gate and not an exhaustive allowlist of every useful CLI
operation. LLM judgment may remain advisory semantic input, but it is not the
authorization gate for side-effectful runtime actions.

High-risk shell/process actions need hard runtime policy. Depending on the
policy classification, they may require explicit user approval and/or
review-role LLM evidence. Review LLM output is policy evidence, not an
authorization gate. If review output is unavailable, times out, fails to parse,
or does not satisfy the policy schema, the admission path must fail closed.
Uncertain risk can route through reviewer evidence and approval policy when
runtime policy requires it, but hard runtime policy decides. User approval is an
authorization source when policy accepts it; reviewer recommendations are only
structured evidence, not authorization.

Future internal LLM roles should be configurable by role, such as `Primary`,
`ToolRiskReview`, `ApprovalReview`, and `SummaryMemory`. That direction is for
runtime/provider composition, not a current stable public API. Role-scoped model
configuration must not leak provider conversation state into runtime state.

The CLI Sandbox Bootstrap slice is now the implemented v1 assumption for the
first real coding-loop smoke, and the runtime process protocol foundation plus
narrow CLI real-runner debug path are in place. The Runtime Coding Loop Harness
has deterministic fake-provider/fake-runner coverage by default, plus opt-in
bwrap and OpenAI-compatible live-provider smokes using ignored local
credentials. `merry --with-sandbox debug coding-loop-smoke` proves the
deterministic bwrap/process/patch path. `merry --with-sandbox debug
coding-loop-live-smoke` is the live LLM proof lane, and the user has reported a
successful credentialed run against their trusted configured server.
Config-backed structured logs/traces for the deterministic coding-loop smoke
are now covered by deterministic tests and a real bwrap smoke with temporary
XDG log config.
Workspace Patch/Write and Shell/Process should both serve that loop through
runtime-owned policy rather than define ad hoc permission rules of their own.

The long-term config direction is `$XDG_CONFIG_HOME/merry/config.toml`, falling
back to `~/.config/merry/config.toml`. That TOML config should own global
defaults, provider/model settings, and observability settings such as whether
logs are enabled, log level, log format, and log output path. Logs should be off
by default. If logging is enabled and no path is configured, file logs should
use `$XDG_STATE_HOME/merry/logs/merry.jsonl`, falling back to
`~/.local/state/merry/logs/merry.jsonl`; failure to create/open that file should
be a clear error, not an implicit stderr fallback.
Use [examples/config.toml](examples/config.toml) as the copy-and-edit starting
point for local configuration. The example is part of the tested config
contract; changes to accepted config keys should update it in the same change.

The existing `.merry/secrets/openai.env` live-smoke config is a transitional
debug path from the first live proof. It is ignored and must not be committed.
The `--with-sandbox` path should move toward mounting the resolved Merry config
directory read-only, plus a log/state path only when file logging is enabled.

M8 runtime/provider/tool execution hardening has shifted into maintenance and foundation work: structured runtime state, artifact-backed model output, provider step boundaries, pending tool calls, tool result resolution, tool continuations, registered tool execution, public runtime API contract cleanup/review/alignment, and opt-in OpenAI Responses debug/tool flows remain the base for later runtime work.

The first Runtime Agent Loop MVP slice is implemented in `merry-runtime` as a bounded serial loop over the existing `Runtime::step`, registered tool execution, and continuation step primitives. It returns ordered runtime events and typed completed/failed/cancelled/blocked outcomes without adding provider wire state or real filesystem/shell tools. Tool execution cancellation during the loop is reported as a cancelled loop status while leaving the pending tool call unresolved.

The `merry-tool-workspace` slice has moved from the read-file first slice into
read-only workspace navigation/search: it exposes registered
`workspace_read_file`, `workspace_list_dir`, and `workspace_search_text` tools
for UTF-8 reads, non-recursive directory listing, and bounded literal text
search under explicitly configured workspace roots. It is still not a shell,
write API, network API, or full coding-agent surface. Read-only
navigation/search is foundation, bootstrap, fallback, and maintenance work;
future write and shell/process capabilities belong in runtime-owned action
protocols rather than provider-specific flows or bare shell escape paths.

The workspace path-safety contract assumes trusted, stable workspace roots. The
MVP prevents ordinary path traversal and ordinary symlink traversal before
read/list/search operations, and on Unix uses `O_NOFOLLOW` for file opens so a
symlink swapped into a file leaf path is rejected. It is not an OS sandbox and
does not claim complete hardening against malicious concurrent filesystem
mutation, such as replacing intermediate directories during validation/open;
that remains a residual TOCTOU risk.

The OpenAI provider targets the Responses API only. The provider request path is `/responses`; it keeps the Merry-owned `merry-llm` provider boundary intact, keeps OpenAI wire types private to `merry-provider-openai`, sets `store: false`, omits `previous_response_id`, avoids provider conversation state as Merry runtime state, and keeps `parallel_tool_calls: false` until runtime policy supports parallel tool calls. This provider work does not imply a live/OpenAI judgment path or public judgment API.

Memory Activation MVP work is internally integrated in `merry-runtime`. The default activation source is a session-owned in-memory stored source; external/default sessions have no candidate memories until runtime-owned state records them. There is still no public memory write API, external persistence, or stable activation contract.

M18F LLM-assisted judgment boundary work is closed out through M18F-I. It reserved an internal advisory judgment boundary, audit carrier, summary-draft promotion lifecycle, uncertainty review harness, and crate-private checked internal context append helper. Promotion still compiles the candidate context snapshot before mutating session context, while public direct context writes remain raw/manual.

The internal model-backed judgment foundation now includes a strict tool-risk review model-output parser, crate-private provider-neutral `ModelBackedJudgmentSource`, and deterministic fake-provider runtime harness proof through `Runtime::run_uncertainty_review`. It remains advisory and fake-provider deterministic only.

Public direct context writes remain raw/manual MVP context mutation helpers. `Runtime::record_context_entry` and `Runtime::record_context_summary` append direct context entries and rely on later context compilation to validate exact evidence readability, but they now share runtime active-step admission and return `StepAlreadyActive` while a step, agent loop, or tool execution owns the session. They are not summary-draft promotion, do not create promotion lifecycle records, and are not governed by promotion acceptance/replay rules. The summary-draft promotion lifecycle remains crate-internal.

Judgment remains advisory semantic input only: it is not a public API, public summary-draft API, public runtime event, ledger fact, tool execution gate, automatic provider-context inclusion, automatic context mutation or promotion, OpenAI/live-provider path, or builder/runtime configured judgment source.

Deterministic verification is based on fake providers and stored runtime state. Live provider flows are manual and opt-in, not required for normal tests.

See [ROADMAP.md](ROADMAP.md) for the current public status.

## Repository Notes

- Engineering rules for agents and contributors live in [AGENTS.md](AGENTS.md).
- Project lead operating rules live in [PROJECT_LEAD.md](PROJECT_LEAD.md).
- Development subagent workflow lives in [SUBAGENT_WORKFLOW.md](SUBAGENT_WORKFLOW.md).
- Local product and design notes are intentionally ignored by git.
- Do not commit private planning documents unless they have been explicitly reviewed for public exposure.
