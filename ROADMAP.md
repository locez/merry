# Roadmap

This roadmap is public-safe and implementation-focused. Private product strategy and design notes remain ignored under `docs/`.

## Current Phase

Merry is in M9 Memory Activation MVP internal runtime integration and contract validation.

M8 runtime/provider/tool execution hardening is now maintenance and foundation work. The current focus is proving that structured memory activation can enter compiled context deterministically inside `merry-runtime` without claiming that production memory storage, public APIs, external persistence, or a stable activation contract are complete. Live provider flows remain explicit opt-in/manual debug paths; they are not the basis for deterministic verification.

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

### Recently Completed

- Runtime/provider/tool execution MVP hardening moved into maintenance and foundation status.
- Provider output storage, pending tool calls, tool result resolution, tool continuations, registered tool execution, and public runtime export/rustdoc alignment have enough deterministic coverage to support the next runtime milestone.
- OpenAI-compatible debug/tool flows remain opt-in manual verification paths, not deterministic test dependencies.

### Active

- Integrate Memory Activation MVP internally in `merry-runtime`.
- Validate deterministic activation projection and evidence handling against stored runtime state.
- Validate provider-step timing so activated memory is available to context compilation before model requests.
- Keep deterministic verification based on fake providers, stored runtime state, artifact references, and ledger assertions.
- Improve public docs as implementation status changes, while keeping private notes under `docs/`.

### Deferred / Next

- Production memory store, public Memory Activation APIs, external persistence, and stable activation contract.
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

## Active Milestone

### Milestone 9: Memory Activation MVP

Goal: prove structured memory enters context through activation, not chat history.

Memory Activation MVP is in internal `merry-runtime` integration and contract validation. The default production source remains noop until production storage, public APIs, external persistence, and the stable contract are ready.

Done:

- Define internal activation data shapes before public runtime APIs.
- Add deterministic projection and evidence validation for activated memory.
- Add provider-step timing skeleton so activation can be projected before model requests.

Active:

- Validate activation seeds and deterministic scoring inside runtime-owned state.
- Prove activated memory records why it entered context.
- Keep activation deterministic and reproducible from stored runtime state.
- Add memory projection template.

Not yet:

- Add production memory store or external persistence.
- Add public Memory Activation API surface.
- Stabilize the activation contract for external consumers.
- Replace the default production noop source.
- Add tests for scope, trigger, confidence, priority, and conflict handling.

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
