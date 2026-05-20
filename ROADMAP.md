# Roadmap

This roadmap is public-safe and implementation-focused. Private product strategy and design notes remain ignored under `docs/`.

## Current Phase

Merry is in M18F-E LLM-assisted judgment boundary work, building on the M9 Memory Activation MVP runtime integration.

M8 runtime/provider/tool execution hardening and M9 Memory Activation MVP are now maintenance and foundation work. Memory Activation now uses a session-owned in-memory stored source by default, but external/default sessions have no candidate memories until runtime-owned state records them. Production memory storage, public memory APIs, external persistence, and a stable activation contract are still not complete. Live provider flows remain explicit opt-in/manual debug paths; they are not the basis for deterministic verification.

## Status Summary

### Completed

- Rust 2024 virtual workspace skeleton with the initial implementation crates.
- Core protocol vocabulary, typed IDs, event contracts, artifact references, tool specs, and provider boundary types.
- Runtime skeleton with session state, task ledger, artifact metadata, event streaming, cancellation, and deterministic fake-provider tests.
- CLI debug surface for inspecting runtime events as JSON lines.
- OpenAI-compatible provider configuration and request rendering skeleton using private provider wire types.
- Context, ledger, and artifact loop for structured runtime state and reproducible context compilation.
- Provider step boundary that keeps runtime state separate from provider conversation state.
- Artifact-backed model output handling.
- Generation configuration propagation through Merry-owned request types.
- Pending tool call representation.
- Tool result resolution into runtime state.
- Tool continuation flow after tool results are supplied.
- Registered tool execution through the runtime tool registry.
- Runtime-reserved artifact IDs for tool/model output paths that must be claimed before events are emitted.
- Opt-in OpenAI debug/tool flow for manual provider integration checks.
- Memory Activation MVP internal runtime integration with session-owned in-memory stored source, deterministic activation, evidence validation, provider-step timing, and lifecycle cleanup coverage.

### Recently Completed

- Runtime/provider/tool execution MVP hardening moved into maintenance and foundation status.
- Provider output storage, pending tool calls, tool result resolution, tool continuations, registered tool execution, and public runtime export/rustdoc alignment have enough deterministic coverage to support the next runtime milestone.
- Memory Activation MVP moved into maintenance and foundation status for internal runtime use.
- Default memory activation is no longer noop: it is backed by a session-owned in-memory stored source. Public memory write APIs, external persistence, and a stable activation contract remain absent, so external/default sessions still start with no candidate memories.
- Memory activation tests cover validation, deterministic matching/scoring/conflict behavior, stored-source projection, evidence failures before provider calls, replacement/clearing behavior, pending-tool gating, cancellation/drop cleanup, and provider lifecycle retention/cleanup.
- OpenAI-compatible debug/tool flows remain opt-in manual verification paths, not deterministic test dependencies.

### Active

- Finish M18F-E accepted summary-draft promotion lifecycle as a crate-internal `merry-runtime` contract with explicit non-LLM acceptance, context compilation validation, rejection tracking, and exact replay idempotency.
- Keep judgment advisory: semantic recommendations can inform runtime policy, but hard runtime policy still decides tool execution, actions, and context mutation.
- Keep the initial judgment contract provider-neutral, evidence-aware, cancellable, and deterministic-testable.
- Do not connect live LLM judgment, public judgment APIs, public runtime events, ledger facts, or tool execution gates in this milestone.
- Keep deterministic verification based on fake providers, stored runtime state, artifact references, and ledger assertions.
- Improve public docs as implementation status changes, while keeping private notes under `docs/`.

### Deferred / Next

- Production memory store, public Memory Activation APIs, external persistence, and stable activation contract.
- Live LLM-backed judgment source, public judgment API, public runtime events/ledger facts for judgment or promotion, and tool execution gate integration.
- Python SDK and `merry-py`.
- Rust facade crate `merry`.
- Macro crate support for boilerplate generation.
- Collaboration and subagent runtime support beyond reserved public contracts.

## Adopted Engineering Decisions

- Rust 2024 virtual Cargo workspace with resolver 3.
- Initial crates:
  - `merry-core`
  - `merry-llm`
  - `merry-runtime`
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
- MVP OpenAI-compatible provider uses a Merry-owned adapter boundary and direct `reqwest`.

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

Goal: prepare OpenAI-compatible provider integration without depending on it for runtime tests.

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
- Keep OpenAI-compatible debug/tool flows explicit opt-in paths for manual verification only.

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

## Active Milestone

### M18F-A / M18F-B / M18F-C / M18F-D / M18F-E: LLM-Assisted Judgment Boundary

Goal: reserve an internal runtime boundary and audit carrier for semantic judgment without giving judgment authority over runtime policy.

M18F-A established the crate-internal contract skeleton in `merry-runtime`. M18F-B adds a crate-internal completed-judgment audit registry with exact internal request/outcome payload carriers. M18F-C wires the first narrow summary-draft audit path through a crate-private helper that records completed advisory `SummaryDraft` judgments only after artifact evidence validation. M18F-D adds an internal explicit acceptance and promotion boundary for accepted summary drafts; promotion is still crate-private, validates exact selected evidence, and compiles a candidate context snapshot before mutation. M18F-E adds a session-owned internal promotion lifecycle registry: exact promoted replays are idempotent no-ops, conflicting payloads are rejected without context mutation, and compile failures become terminal rejected records. Judgment outcomes are advisory semantic evidence; they cannot authorize tool execution, actions, or context mutation. Provider wire formats do not enter runtime, and summary/evidence exact artifact rules remain unchanged.

Done:

- Define internal purpose, provenance, confidence, evidence, request, recommendation, outcome, context, source trait, and typed error shapes.
- Add object-safe boxed-future source boundary and deterministic noop source.
- Add unit tests for validation, evidence requirements, object-safe calls, advisory noop behavior, and cancellation context.
- Add internal completed-only judgment record ids, deterministic registry snapshots, and exact internal request/outcome payload artifacts.
- Add session-private judgment recording helpers that validate request/outcome evidence against session artifacts before writing the internal registry.
- Add a crate-private summary-draft judgment helper and deterministic tests proving recorded drafts stay out of compiled context, ledger projection, runtime event sequence, and pending-tool state.
- Add crate-private summary-draft acceptance and promotion helpers that reject LLM authority, require exact draft text match, require selected judgment evidence, and leave context unchanged on validation failure.
- Add a crate-internal summary-draft promotion lifecycle registry with deterministic snapshots, promoted exact-replay idempotency, payload conflict detection, and rejected-record replay protection.

Active:

- Keep the boundary internal while runtime policy integration is designed.
- Preserve the advisory/hard-policy split in docs, names, and storage boundaries.
- Keep summary-draft audit and promotion internal, with no record-id-authorized or automatic context promotion.
- Keep promotion lifecycle state out of public runtime APIs, runtime events, ledger facts, and tool-call policy.

Not yet:

- Live LLM-backed judgment source.
- Public judgment API.
- Public summary-draft recording API.
- Public summary-draft promotion API.
- Tool execution gate integration.
- New public `merry-core` event, id, or reference types.
- Public runtime events or ledger facts for judgments.
- Tool-call policy changes or automatic provider-context inclusion from judgment drafts.

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
