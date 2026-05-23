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
loop's substitute. Policy, risk taxonomy, reviewer evidence, and role-scoped
models should advance only when they unblock this executable acceptance target.

Default `cargo test` must stay deterministic, offline, and fake-provider based.
In addition, Merry needs explicit opt-in smoke lanes:

- `bwrap` sandbox smoke using a disposable fixture repository and real process
  runner.
- Live OpenAI-compatible smoke using locally supplied credentials and
  `MERRY_OPENAI_DEBUG=1`.

Local credentials must never be committed. Existing live provider config uses:

```text
MERRY_OPENAI_DEBUG=1
MERRY_OPENAI_API_KEY=<local secret>
MERRY_OPENAI_MODEL=<model>
MERRY_OPENAI_BASE_URL=<optional OpenAI-compatible base URL>
OPENAI_ORG_ID=<optional>
OPENAI_PROJECT_ID=<optional>
```

If file-based local config is introduced, it must live under ignored paths such
as `.env.merry.local`, `.merry/local/`, or `.merry/secrets/`.

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
- CLI Sandbox Bootstrap is implemented in `merry-cli`: the root `--with-sandbox` flag uses `clap` and performs Linux `bwrap` self-reexec with a minimal environment, `PATH` lookup for `bwrap`, plan-stage missing-`bwrap` handling, recursion avoidance, sandbox-local `/tmp`, the current repo/project as the primary read-write workspace, and a minimal `/etc` allowlist including `/etc/ld.so.cache`, resolver/host/NSS files, and SSL/PKI paths. v1 still allows network access and is not a complete security boundary. A real smoke of `target/debug/merry --with-sandbox debug` has passed.
- Shell/Process SP1/SP2/SP3-A plus the latest CLI admission slices are implemented: `merry-runtime` has provider-neutral process intent/evidence, proposed/executed process action audit variants, explicit injected `ProcessRunner` boundaries, process intent classification, opt-in informational process admission, accepted local workspace process admission, bounded stdout/stderr result artifacts, payload-free proposal/execution evidence, default deny behavior, cancellation paths that keep pending calls unresolved until runner output exists, and deterministic fake-runner tests. `merry-cli` has the narrow debug/demo `merry shell -- <argv>` real runner adapter using `tokio::process::Command`; informational `rustc --version` / `rg --version` can run, and exact `cargo test -p merry-runtime` requires accepted local workspace risk plus the CLI bwrap handoff and sandbox runtime evidence. This does not implement general shell/process/coding-agent capability, raw shell mode, pipelines/scripts, arbitrary env/stdin, a complete sandbox proof, or a general approval/review admission UX.

### Active

- P0: implement the Minimal Useful Coding Loop as the current MVP proof. The loop must show multi-step runtime behavior, not only one tool call followed by a final response.
- P0: add a Runtime Coding Loop Harness that can run against a disposable fixture repository. The required default lane uses fake provider/fake runner for deterministic `cargo test`; opt-in lanes run with the real `bwrap` sandbox and live OpenAI-compatible provider.
- P0: define a runtime-owned read-only process profile for command families, not one-off command matches. Initial coverage should include `rg --files`, literal `rg <pattern>`, and a read-only file-slice command shape such as `sed -n RANGE FILE`, or an equivalent typed process/read tool that proves the same evidence loop.
- P0: keep edit/write on the typed workspace patch path. The MVP loop should apply one constrained patch through `workspace_patch_file` or its runtime-owned successor, record artifact/audit/ledger evidence, and then run verification.
- Keep safety tiered but subordinate to the executable acceptance target: read-only inspection automatic, constrained patch opt-in, verification in `bwrap`, high-risk or unknown process actions denied or escalated.
- Keep CLI shell as smoke/debug, not the design owner. The main contract is the runtime library and its registered tool/profile set.
- Keep judgment advisory. Do not wire model-backed judgment to live provider, public events, ledger facts, or authorization until a concrete coding-loop acceptance test needs reviewer evidence.
- Keep default verification deterministic and offline. Live provider and host/sandbox process smokes are explicit opt-in checks with local credentials and disposable workspaces.
- Improve public docs as implementation status changes, while keeping private notes under `docs/` and `merry-raw-docs/` ignored.

### Next Active

- Runtime Coding Loop Harness for the Minimal Useful Coding Loop.
- Read-only process profile for reusable workspace inspection and exact evidence retrieval.
- Real `bwrap` sandbox smoke against a disposable fixture repository.
- Opt-in live OpenAI-compatible smoke configuration for the same loop.

### Next Milestone: Minimal Useful Coding Loop Harness

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

opt-in live provider smoke:
  requires `MERRY_OPENAI_DEBUG=1`
  requires local `MERRY_OPENAI_API_KEY` or `OPENAI_API_KEY`
  requires `MERRY_OPENAI_MODEL`
  optionally uses `MERRY_OPENAI_BASE_URL`
  is never part of default `cargo test`
```

Tasks:

- Add a fixture repository purpose-built for the loop, with a tiny failing behavior or deterministic text replacement target.
- Add a harness command or integration test wrapper that builds a runtime with the coding-loop tool set.
- Add a read-only process profile or equivalent reusable admission layer for file listing, literal search, and exact source slice retrieval.
- Register the runtime-owned default coding-loop tools from library code, not by ad hoc CLI-only assembly.
- Use `workspace_patch_file` or its successor for the edit step and keep shell side effects out of the edit path.
- Add deterministic fake-provider/fake-runner tests for the full multi-step loop.
- Add ignored local config guidance for live provider credentials and base URL.
- Add an explicit, non-default bwrap/live smoke command once the deterministic loop passes.

Non-goals:

- Do not implement a full autonomous coding agent.
- Do not add arbitrary shell parsing, pipelines, inherited env, stdin, network tools, or broad filesystem writes.
- Do not make live provider behavior a default test dependency.
- Do not make risk taxonomy or reviewer models the milestone output unless they unblock this loop.
- Do not build graph memory, skill VM, Python SDK, or subagent runtime in this milestone.

Verification:

- `cargo fmt --all --check`
- focused deterministic cargo test for the coding-loop harness
- opt-in real bwrap smoke command once implemented
- opt-in live provider smoke command once implemented

### Next Milestone: Action Policy Risk Taxonomy and Role-Scoped Models

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

### Next Milestone: Shell/Process Primary Actuator Protocol

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
