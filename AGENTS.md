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

