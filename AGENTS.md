# AGENTS.md

This repository is a Rust-first agent runtime project. Treat this file as the working contract for any human, agent, or subagent making changes here.

## Repository Rules

- Keep implementation changes scoped to the requested module or task.
- Do not commit private planning material, product strategy, market notes, or design drafts unless explicitly asked.
- `docs/` is intentionally ignored by git and may contain private local notes.
- `merry-raw-docs/` is ignored original source material; use it as local evidence only and do not commit it.
- Do not move private notes into tracked files.
- Prefer small, reviewable changes over broad rewrites.
- If the worktree contains changes you did not make, preserve them and adapt around them.

## Delivery Focus Discipline

This file defines lifecycle-level working rules. It must not hard-code a
temporary roadmap milestone as the permanent project goal. The active delivery
focus belongs in tracked planning/status files such as `ROADMAP.md`, or in the
user's explicit current instruction.

Agents must keep each round tied to the active delivery focus:

- Start substantial work by naming the current gap being advanced.
- Before dispatching research, state which implementation decision or
  acceptance test the research will unblock.
- Prefer implementation, tests, and executable verification over more planning
  when the next implementation step is known.
- Before ending a round, report whether the work advanced the active delivery
  focus. If not, mark it as drift unless it resolved a named blocker.
- Do not let policy, classifier, roadmap, role-model, or research work become
  the primary output unless the user asked for that specifically or it resolves
  a named blocker for the active focus.
- User-requested docs, status, planning, or research-only rounds are not drift
  solely because they do not deliver a new runtime capability. Treat the
  requested tracked update, answer, or research result as that round's delivery
  focus and report it accurately.

## Roadmap Change Control

`ROADMAP.md` is a controlled planning artifact, not an agent scratchpad.
Agents may update implementation status, completed evidence, validation notes,
or a user-approved correction. Agents must not unilaterally change the active
product priority, reorder major milestones, or promote a supporting design
thread such as policy/profile/classifier work into the primary `Next Active`
track without explicit user approval.

When a roadmap change affects priority or milestone ordering:

- State the current active goal and the proposed replacement before editing.
- Name the user instruction or tracked change request that authorizes the
  priority change.
- Keep supporting architecture work subordinate to a runnable or testable
  capability unless the user explicitly asked for planning/design only.
- Record the reason in `ROADMAP.md` or a tracked spec when the change corrects
  drift or changes the near-term sequence.
- If the only known next step is more taxonomy, policy, classifier, profile, or
  roadmap work, stop and ask whether that should replace the current executable
  acceptance target.

## Architecture Boundaries

Merry is runtime-first. Keep these ownership boundaries clear:

- `core` owns shared types, errors, schemas, and event contracts.
- `runtime` owns sessions, task ledger, artifact references, memory activation, context compilation, validation, and checkpoints.
- `llm` owns provider traits and normalized model events.
- provider crates adapt external APIs into Merry-owned traits.
- macro crates generate boilerplate only; they must not hide runtime control flow.
- Python bindings expose the Rust runtime; they must not reimplement the runtime in Python.

Do not leak provider-specific response formats into runtime, memory, artifact, skill, or compiler code.

## Initial Workspace Decisions

- Use a Cargo virtual workspace.
- Start with these implementation crates:
  - `merry-core`
  - `merry-llm`
  - `merry-runtime`
  - `merry-provider-openai`
  - `merry-cli`
- Defer these crates until the event protocol and runtime builder are stable:
  - `merry-macros`
  - `merry-py`
  - a Rust facade crate named `merry`
- Use Rust 2024 edition and workspace resolver 3 unless a concrete toolchain issue forces a revision.
- Keep features additive. Do not use mutually exclusive feature sets unless there is no practical alternative.
- Provider integrations belong in provider crates, not runtime feature flags.
- Forbid unsafe at the workspace lint level.

## Context And Evidence Rules

- Runtime state is structured. Do not make raw chat history the source of truth.
- Summaries are navigation. Exact evidence must remain available through artifacts or source reads.
- Tool outputs should become artifacts and compact ledger updates, not permanent prompt text.
- Memory activation must record why memory entered context.
- Context compilation should be deterministic and reproducible from stored runtime state.

## Model Request And Prompt Cache Discipline

- Any change to provider-visible model request composition must explicitly
  assess prompt/KV cache reuse. This includes system or developer text, ordered
  context segments, tool additions or removals, tool names, descriptions, input
  schemas, tool ordering, output schemas, and phase-dependent availability.
- Prefer a stable request prefix throughout a session. Keep provider-visible
  tool definitions and their order stable when runtime admission or typed
  execution validation can enforce the behavior without changing the request.
- Do not dynamically add, remove, rename, rewrite, or reorder tools merely to
  represent runtime phase, role, permission, or UI state. Treat permissions and
  execution admission as runtime policy unless a genuinely different model
  contract requires a separate runtime or request surface.
- Place dynamic state after stable instructions and tool definitions when the
  context contract permits it. Cache reuse is an optimization and must never be
  required for correctness.
- When a cache-breaking request change is necessary, state its expected scope
  and reason in the implementation or review report, and add deterministic
  request/schema/order coverage where accidental churn would be costly.

## Configuration Example Contract

- Keep the tracked user-facing example config at `examples/config.toml`.
- When adding, removing, renaming, or changing any `config.toml` key accepted by
  `merry-cli`, update `examples/config.toml` in the same change unless there is
  a recorded reason it intentionally does not expose that key yet.
- Do not put real API keys, host-specific secrets, private endpoints, or local
  machine paths in `examples/config.toml`.
- The CLI config tests must continue to parse `examples/config.toml`, so schema
  drift breaks deterministic tests before it reaches users.

## Goal And MVP Discipline

Agents must keep the project pointed at the current product goal, not just at
locally coherent architecture work. When the active goal is an MVP capability,
the next milestone must be framed around a runnable or testable capability
unless the user explicitly asks for docs, status, research, planning, or design
only.

- State the capability being advanced before implementation.
- Define at least one concrete acceptance command, test, or observable runtime
  behavior before doing substantial implementation work.
- Treat research, policy, taxonomy, docs, and guardrail work as support for the
  acceptance target, not as a substitute for delivering the capability, unless
  the user explicitly requested that kind of work as the deliverable.
- If a proposed milestone cannot say what new command, API behavior, runtime
  event, artifact, or test will work after the change, stop and reframe it.
- Prefer the smallest real vertical slice that proves the capability over a
  broad framework that only describes the capability.
- Before reporting completion, answer whether the change moved the current MVP
  capability forward, and name the evidence.
- If the user challenges direction or says the work is drifting, pause feature
  work and update the tracked guardrails or roadmap before continuing.

Active product priorities must come from the current roadmap or user
instruction. `AGENTS.md` may define how agents avoid drift, but it must not
choose one temporary capability as the permanent project direction.

## Subagent And Parallel Work Rules

Subagents are bounded workers, not chat participants.

When multiple agents work in this repository:

- Assign each worker a clear file/module ownership scope.
- Workers must not edit files outside their assigned scope unless the parent explicitly expands it.
- Workers must not revert or overwrite changes made by others.
- Read-only exploration workers should return evidence references and findings, not patches.
- Review workers should inspect concrete diffs, evidence, and tests.
- Implementation workers should list changed files and verification commands in their final report.

Future subagent support should use explicit task contracts, artifact references, allowed tools, budgets, and merge policies.

## Rust Engineering Standards

- Use stable Rust unless a feature is explicitly justified.
- Prefer typed data structures over stringly typed protocols.
- Use `serde` for serialization boundaries.
- Use `schemars` or equivalent schema generation for tool/provider schemas when introduced.
- Use `thiserror` for library errors and preserve actionable error context.
- Use `tracing` for structured runtime diagnostics.
- Keep async boundaries explicit and avoid blocking inside async code.
- Avoid global mutable state.
- Avoid hidden registration side effects in macros.

## Async Runtime Rules

- Use Tokio for the MVP runtime surface. Do not introduce a runtime-agnostic executor abstraction yet.
- Runtime and provider event APIs should be stream-first.
- Public dyn/plugin async boundaries should use explicit `BoxFuture` or `BoxStream` rather than bare `async fn` in public traits.
- Internal/private traits may use native async syntax when dyn compatibility is not required.
- Use bounded `tokio::sync::mpsc` channels for event production unless a design explicitly justifies another stream source.
- Dropping an event stream or cancelling its token must stop producers and avoid new side effects.
- Long-running loops must include documented cancellation checkpoints.
- No observable `RuntimeEvent` may claim an artifact, ledger update, or checkpoint before that state is durably written.
- Use cooperative cancellation with cancellation tokens and `tokio::select!` where appropriate.
- `spawn_blocking` is only for bounded blocking work; do not use it as a general escape hatch.

## Rust Code Quality Rules

These rules are intentionally concrete. Passing the compiler is not enough; code should read like idiomatic Rust written by someone who understands ownership, allocation, and API boundaries.

### File Size And Module Structure

- Keep source files focused and structurally readable. A single source file should usually stay under 1000 lines.
- Treat files approaching 1000 lines as a design smell to inspect before adding more code.
- Do not split files mechanically just to satisfy a line count. Extract modules around real ownership boundaries, such as command handlers, configuration loading, provider setup, output formatting, fixtures, or focused test support.
- Top-level entrypoint files such as `main.rs` should stay small and route work to modules instead of owning command implementation details.
- Large test blocks are not a reason to let production files grow indefinitely. Prefer moving tests into focused `tests` modules, sibling test modules, or integration tests when test code dominates the file.
- When a file must exceed 1000 lines temporarily, state why in the final report and name the follow-up split that would restore a reasonable structure.

### Unsafe

- `unsafe` is forbidden by default.
- If `unsafe` is truly required, isolate it in the smallest possible module.
- Every `unsafe` block must have a `SAFETY:` comment explaining the invariant.
- Unsafe abstractions must expose a safe API and include tests for boundary behavior.
- Do not use `unsafe` for performance until there is a benchmark showing the safe version is insufficient.

### Ownership And Borrowing

- Do not clone just to appease the borrow checker. First reconsider ownership and API shape.
- Prefer borrowed inputs for read-only APIs:
  - `&str` over `String`
  - `&Path` over `PathBuf`
  - `&[T]` over `Vec<T>`
  - `&T` over `T` when the callee does not need ownership
- Store owned data only when the struct owns the value beyond the call.
- Public APIs should avoid unnecessary lifetime complexity. Use lifetimes when they clearly express ownership and reduce allocation, not to be clever.
- Prefer moving ownership at clear boundaries over keeping long-lived borrows that make the API hard to use.
- Be explicit when cloning is semantically correct, such as cloning cheap handles, IDs, `Arc`, immutable config snapshots, or data that must cross task boundaries.

### Allocation And Data Shapes

- Avoid repeated allocation in hot or obvious loops.
- Avoid building large intermediate `String` or `Vec` values when streaming, iterators, or references are natural.
- Use `Cow<'_, str>` or `Cow<'_, [T]>` only when it simplifies an actual borrow-or-own boundary.
- Do not prematurely micro-optimize. Prefer simple ownership first, then measure.
- Use newtypes for important IDs and protocol names:
  - `SessionId`
  - `ArtifactId`
  - `ToolName`
  - `SkillId`
  - `ProviderName`
- Avoid passing raw `String` values for domain concepts that need validation or stable identity.

### Errors

- Library crates should use typed errors with `thiserror`.
- `anyhow` is acceptable in binaries, examples, tests, and thin CLI layers, but not as the main error type for core library APIs.
- Preserve source errors with `#[source]` or transparent variants when useful.
- Error messages should include actionable context, not vague labels like "failed" or "invalid".
- Do not silently collapse provider, validation, IO, and protocol errors into one generic error.

### Async And Concurrency

- Do not perform blocking IO inside async runtime paths. Use async APIs or isolate blocking work with the appropriate runtime mechanism.
- Do not hold `std::sync::MutexGuard`, `RwLock` guards, or mutable borrows across `.await`.
- Prefer message/event boundaries over shared mutable state.
- Use `Arc` to share ownership across tasks only when sharing is part of the design.
- Avoid `Arc<Mutex<Everything>>`. If it appears, split the state or define a narrower synchronization boundary.
- Cancellation, checkpoint, and retry boundaries should be explicit in long-running runtime code.
- Spawned tasks must have clear ownership, error propagation, and shutdown behavior.
- Async tests should avoid wall-clock sleeps. Prefer fake streams, paused Tokio time, cancellation/drop tests, and deterministic artifact/ledger assertions.

### Traits And Dynamic Dispatch

- Prefer generics or concrete types until dynamic dispatch solves a real boundary problem.
- Use `dyn Trait` for plugin/provider/tool boundaries when runtime polymorphism is needed.
- Keep object-safe traits small and focused.
- Avoid trait objects that hide important capability differences. Use capability structs or enums where behavior affects runtime policy.

### Public API Design

- Keep public APIs boring, typed, and stable.
- Constructors should validate invariants instead of allowing invalid structs to circulate.
- Prefer explicit builders for complex configuration.
- Avoid "stringly typed" control flow.
- Avoid exposing internal storage layout through public APIs.
- Make invalid states unrepresentable when doing so does not overcomplicate the design.
- Keep module visibility narrow. Start with `pub(crate)` unless external use is intended.

### Serde And Schemas

- Serialized structs should be explicit and versionable.
- Use `#[serde(deny_unknown_fields)]` for strict external input where forward compatibility is not required.
- Use stable field names; changing serialized names is a compatibility decision.
- Avoid serializing internal-only implementation details.
- Schema-generating types should be small, documented, and tested with snapshot-style checks once the schema surface stabilizes.

### Macros

- Macros are allowed to remove boilerplate, not to create hidden behavior.
- Macro expansion should be conceptually obvious from the call site.
- Macros must not silently register global runtime state.
- Macros must not choose providers, alter context compiler behavior, or change execution policy.
- Prefer explicit registration even when a macro generates the registered value.

### Tests

- Unit tests should cover invariants, parsing, scoring, validation, and reducers.
- Integration tests should cover runtime event flow and artifact/ledger interactions.
- Provider-dependent tests must be isolated behind features, mocks, or explicit opt-in environment variables.
- Prefer deterministic tests over tests that depend on live model behavior.
- If behavior is important enough to encode in `AGENTS.md`, it is a candidate for a future test or lint.

## Rust Review Checklist

Use this checklist before reporting a Rust change as complete:

- Did this introduce an unnecessary `clone`, allocation, or owned `String`?
- Is each clone semantically justified by ownership transfer, task boundary, or cheap handle semantics?
- Could read-only inputs be `&str`, `&Path`, `&[T]`, or `&T`?
- Are important IDs represented as typed newtypes rather than raw strings?
- Are error types precise, typed, and actionable?
- Does async code avoid blocking IO and guards across `.await`?
- Is shared state narrower than `Arc<Mutex<Everything>>`?
- Is `unsafe` absent, or isolated with a clear `SAFETY:` invariant?
- Are public APIs smaller and more stable than internal implementation details?
- Does the change preserve runtime/provider/context boundaries?
- If provider-visible model request composition changed, was the prompt/KV cache
  impact assessed and were stable prefix, tool schema, and ordering invariants
  preserved or intentionally documented?
- Can the behavior be tested without a live provider or network call?
- Did `cargo fmt`, `cargo clippy`, and `cargo test` run for the touched area?

## Python Binding Standards

- Use PyO3/maturin for Python bindings.
- Keep PyO3 wrappers thin.
- Python APIs should be ergonomic wrappers around Rust-owned behavior.
- Do not call arbitrary Python callbacks from deep runtime code in early implementations.
- Prefer event bridging for Python tools: Rust emits a tool call, Python executes it, Python returns the result.
- When Python bindings are added, use a mixed maturin layout with Rust exposed as `merry._merry` and ergonomic Python wrappers in `python/merry`.
- PyO3 and `pyo3-async-runtimes` dependencies are allowed only in `merry-py`.
- PyO3 types such as `Python<'py>` and `Py<PyAny>` must not cross into core, runtime, llm, or provider traits.
- Python should expose async event iteration as the primary API, such as `async for event in runtime.step(...)`.
- Rust code must not hold the GIL while awaiting, blocking, or locking Rust mutexes.

## Provider Integration Rules

- Merry-owned provider traits and normalized event/request/response types live in `merry-llm`.
- Provider wire structs must remain private to provider crates.
- Tool schemas are generated from Merry-owned types; provider crates render those schemas into provider-specific request formats.
- MVP OpenAI-compatible support should use direct `reqwest` in the provider crate unless a later implementation plan justifies wrapping another crate.
- Do not wrap multiprovider abstraction crates in MVP if they compete with Merry's own provider boundary.
- Do not use provider conversation state as Merry runtime state.
- If an OpenAI Responses adapter is added later, set `store = false` by default and do not use `previous_response_id` as the Merry task ledger.
- Disable parallel tool calls by default until runtime policy explicitly supports more than one pending tool call.

## Testing And Verification

Before claiming completion, run the relevant checks for the touched area.

For Rust code, expected checks are:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

For Python bindings or SDK code, expected checks should be added once the Python package exists.

If a check cannot be run, state exactly why and what remains unverified.

## Commit Hygiene

- Keep commits focused.
- Use Conventional Commit subjects consistent with the repository history, such
  as `feat(runtime): add checkpoint compaction`, `fix(cli): reject invalid
  config`, `docs(plan): record validation notes`, or `refactor(runtime): name
  checkpoint context explicitly`.
- Do not commit ignored private docs.
- Do not commit generated build artifacts.
- Do not include secrets, API keys, local machine paths, or unpublished product strategy in tracked files.
