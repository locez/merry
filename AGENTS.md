# AGENTS.md

This repository is a Rust-first agent runtime project. Treat this file as the working contract for any human, agent, or subagent making changes here.

## Repository Rules

- Keep implementation changes scoped to the requested module or task.
- Do not commit private planning material, product strategy, market notes, or design drafts unless explicitly asked.
- `docs/` is intentionally ignored by git and may contain private local notes.
- Do not move private notes into tracked files.
- Prefer small, reviewable changes over broad rewrites.
- If the worktree contains changes you did not make, preserve them and adapt around them.

## Architecture Boundaries

Merry is runtime-first. Keep these ownership boundaries clear:

- `core` owns shared types, errors, schemas, and event contracts.
- `runtime` owns sessions, task ledger, artifact references, memory activation, context compilation, validation, and checkpoints.
- `llm` owns provider traits and normalized model events.
- provider crates adapt external APIs into Merry-owned traits.
- macro crates generate boilerplate only; they must not hide runtime control flow.
- Python bindings expose the Rust runtime; they must not reimplement the runtime in Python.

Do not leak provider-specific response formats into runtime, memory, artifact, skill, or compiler code.

## Context And Evidence Rules

- Runtime state is structured. Do not make raw chat history the source of truth.
- Summaries are navigation. Exact evidence must remain available through artifacts or source reads.
- Tool outputs should become artifacts and compact ledger updates, not permanent prompt text.
- Memory activation must record why memory entered context.
- Context compilation should be deterministic and reproducible from stored runtime state.

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

## Rust Code Quality Rules

These rules are intentionally concrete. Passing the compiler is not enough; code should read like idiomatic Rust written by someone who understands ownership, allocation, and API boundaries.

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
- Can the behavior be tested without a live provider or network call?
- Did `cargo fmt`, `cargo clippy`, and `cargo test` run for the touched area?

## Python Binding Standards

- Use PyO3/maturin for Python bindings.
- Keep PyO3 wrappers thin.
- Python APIs should be ergonomic wrappers around Rust-owned behavior.
- Do not call arbitrary Python callbacks from deep runtime code in early implementations.
- Prefer event bridging for Python tools: Rust emits a tool call, Python executes it, Python returns the result.

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
- Do not commit ignored private docs.
- Do not commit generated build artifacts.
- Do not include secrets, API keys, local machine paths, or unpublished product strategy in tracked files.
