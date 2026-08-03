# AGENTS.md

This repository is a Rust-first agent runtime project. Treat this file as the working contract for any human, agent, or subagent making changes here.

## Repository Rules

- Keep this file limited to long-lived, repeatable engineering rules. Do not put
  issue lists, temporary migration plans, or current technical-debt inventories
  here; track those in issues, `ROADMAP.md`, or local design documents.
- Keep implementation changes scoped to the requested module or task.
- Do not commit private planning material, product strategy, market notes, or design drafts unless explicitly asked.
- `docs/` is intentionally ignored by git and may contain private local notes.
- Design documents, specs, and plans belong in `local_docs/` by default and must remain local unless explicitly requested otherwise.
- `merry-raw-docs/` is ignored original source material; use it as local evidence only and do not commit it.
- Do not move private notes into tracked files.
- Prefer small, reviewable changes over broad rewrites.
- If the worktree contains changes you did not make, preserve them and adapt around them.
- Workers must not develop directly on the default branch. Use a dedicated
  branch/worktree; the coordinator may use the default branch only for reviewed
  integration.
- Treat `Cargo.toml`, the current source and tests, and `ROADMAP.md` as the
  source of truth when this file points to a path, crate, or capability that
  may have changed. Verify stale-looking instructions before relying on them,
  and report the mismatch instead of silently broadening the task.

## Agent Operating Loop

Use this loop for every substantial task, whether it is executed directly or
delegated to another agent:

1. **Orient.** Read this file, the relevant roadmap section, and the owning
   module/tests. Run `git status --short --branch` and
   `git worktree list --porcelain` before editing.
2. **State the contract.** Name the active gap, observable acceptance target,
   base commit, allowed write paths, and expected integration method.
3. **Inspect before changing.** Search for sibling paths, existing tests, and
   user changes. Treat a dirty worktree as input, not cleanup work.
4. **Implement the smallest slice.** Keep one owner per file and preserve the
   runtime, provider, and facade boundaries below.
5. **Verify.** Run deterministic checks for the touched area first, then the
   required repository checks. Record blocked or skipped checks with the exact
   cause.
6. **Hand off clearly.** Report changed files, commits, verification, known
   limits, integration notes, and whether the active gap advanced.

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

Merry is runtime-first. The layer map below is the long-lived ownership
contract for future changes. It describes where new responsibility belongs; it
does not require a mechanical move of every existing module in one change.

### Layer Ownership And Directory Classification

- `crates/merry-core` is the contract layer. It owns shared IDs, errors,
  schemas, artifacts, journal/event contracts, and other provider-neutral data
  that must cross more than one boundary.
- `crates/merry-llm` is the model boundary. It owns provider traits, normalized
  model requests/responses/events, model capabilities, and provider-neutral
  tool continuations.
- `crates/merry-runtime` is the runtime layer. It owns sessions, task ledger,
  artifacts, context compilation, memory activation, checkpoints, permission
  admission, tool execution, cancellation, and lifecycle state. Runtime state
  is the source of truth for these concerns.
- `crates/merry-provider-openai` and `crates/merry-provider-anthropic` are
  provider adapter layers. They translate external requests and responses at
  the `merry-llm` boundary; external wire structs and protocol details remain
  inside these crates.
- `crates/merry-tool-workspace` and `crates/merry-mcp` are resource/tool
  adapter layers. They connect workspace or MCP capabilities to runtime tool
  contracts, but do not own session, ledger, artifact, or policy state.
- The coding composition layer owns coding prompts, project rules, tool
  catalog, permission policy, validation policy, retry/recovery policy, and
  final-report policy. A dedicated `merry-coding` crate is a proposed future
  workspace member, not a current crate; until that boundary exists, new code
  must not add another parallel coding composition path.
- `crates/merry` is the Rust facade layer. It provides the application-facing
  construction and event surface without exposing provider wire types or CLI
  implementation details. Its eventual publishability is a product decision,
  not a reason to move runtime state into the facade.
- `crates/merry-cli` is the product surface. It owns CLI/TUI interaction,
  configuration, presentation, debug fixtures, and host process/sandbox
  adaptation. It may select providers and pass runtime inputs, but it must not
  own a second runtime state model or a second coding policy.
- `crates/merry-py` and `sdks/python` are binding surfaces. They convert and
  expose Rust-owned behavior; runtime state, ledger, artifact, permission,
  retry, and policy ownership stays in Rust. Detailed PyO3, GIL, and Python
  API rules are in `Python Binding Standards` below.
- Evaluation protocols, harnesses, and benchmark adapters are an upper-layer
  concern. They may consume public runtime/facade contracts and normalize
  external task formats, but evaluation models must not become runtime state or
  provider wire types.

### Dependency Direction And Evolution Rules

- Normal production dependencies point from surfaces and adapters toward
  provider-neutral contracts and runtime services. Lower layers must not import
  CLI, PyO3, Python SDK, evaluation, or product-specific composition code.
- Among Merry workspace crates, `merry-core` must remain independent of
  `merry-llm`, runtime, providers, composition, CLI, and bindings.
  `merry-llm` may depend on `merry-core`, but not on runtime or a concrete
  provider.
- `merry-runtime` may depend on `merry-core` and `merry-llm`. It must not take
  a production dependency on a provider crate, CLI, facade, PyO3, or a provider
  wire type. Test-only provider fixtures are allowed as `dev-dependencies`
  when they do not enter production targets.
- Provider crates may depend on `merry-core` and `merry-llm`, but not on
  runtime, CLI, facade, bindings, or another provider's wire protocol.
- Tool/resource adapters, the coding composition layer, the facade, CLI, and
  bindings may depend on runtime contracts as appropriate; the reverse
  direction is forbidden. A binding may call runtime APIs to bridge them, but
  may not reimplement their ownership or policy.
- Provider-specific request/response formats must not cross the provider
  adapter boundary into `merry-core`, `merry-llm`, runtime, coding composition,
  facade, CLI, evaluation records, or Python public types. The detailed
  visibility and rendering rules remain in `Provider Integration Rules` below.
- When a change touches more than one layer, keep the cross-layer contract in
  the lower owning layer and put translation, presentation, or orchestration in
  the higher layer. Do not duplicate a domain type or bypass the owner through
  raw JSON, provider structs, PyO3 objects, or string dispatch.
- When multiple current entry points assemble the same coding behavior, treat
  them as a migration boundary. Extend the shared composition owner or record a
  deliberate exception; do not introduce another independent prompt, tool
  policy, or runtime builder path.
- Add a new crate only when an ownership boundary, dependency direction, or
  public compatibility boundary is genuinely different. Record the reason and
  the intended dependency direction before adding it to `Cargo.toml`.

Macro crates generate boilerplate only; they must not hide runtime control flow.

## Workspace And Crate Boundaries

- Use the Cargo virtual workspace, Rust 2024 edition, and resolver 3 already
  declared in `Cargo.toml`.
- Treat the workspace members in `Cargo.toml` as authoritative. The current
  implementation includes `merry-core`, `merry-llm`, `merry-runtime`,
  `merry-tool-workspace`, `merry-mcp`, both provider crates, `merry`,
  `merry-cli`, and `merry-py`; do not assume this list is permanent.
- Keep features additive. Do not use mutually exclusive feature sets unless there is no practical alternative.
- Provider integrations belong in provider crates, not runtime feature flags.
- A proposed layer or crate is not an implementation boundary until it exists
  in the workspace manifest. Architecture notes may describe future ownership,
  but code and dependency decisions must use the current `Cargo.toml`.
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

## Worktree And Parallel Agent Protocol

Parallel work consists of isolated worktrees plus one coordinator. The
coordinator owns task decomposition and integration; each worker owns its
assigned branch and files. Subagents are bounded workers, not chat
participants.

### Coordinator Task Contract

Before dispatching a worker, provide all of the following:

- **Objective:** the active delivery gap and the smallest result that advances
  it.
- **Base:** the commit the worker must start from, normally the coordinator's
  current `HEAD`.
- **Isolation:** the exact worktree path and unique branch name, or an explicit
  instruction to remain in an already linked worktree.
- **Ownership:** allowed write paths and important read-only paths. One active
  worker owns a file at a time; shared files require an explicit serialized
  handoff.
- **Dependencies:** interfaces, commits, artifacts, or other workers that must
  exist first. State the integration order when it matters.
- **Evidence:** source paths, fixtures, artifacts, or prior findings the worker
  should use or produce.
- **Limits:** allowed tools and commands, network or live-provider access, and
  a time or token budget when those constraints matter.
- **Acceptance:** at least one command, test, event, artifact, or other
  observable behavior that proves the task is useful.
- **Verification:** targeted checks and any broader repository checks expected
  before handoff.
- **Handoff:** whether the worker must commit, which merge policy applies, and
  the report fields required below.

Do not dispatch research or review work without stating which implementation
decision or acceptance check its evidence will unblock.

### Worktree Lifecycle

- Check isolation before creating anything:
  `git rev-parse --git-dir`, `git rev-parse --git-common-dir`,
  `git branch --show-current`, and `git worktree list --porcelain`.
- If `git-dir` differs from `git-common-dir` and the checkout is not a
  submodule, the agent is already in a linked worktree. Do not create a nested
  worktree or switch branches.
- When starting from the ordinary checkout, prefer the host's native worktree
  mechanism when one is available. Otherwise use the ignored `.worktrees/`
  directory and verify it from the primary checkout before creation:

  ```bash
  git check-ignore -q .worktrees
  git worktree add .worktrees/<task-slug> -b <type>/<task-slug>
  ```

- Make the branch and path unique. Inspect `git worktree list` first and never
  reuse another active worker's worktree or branch without coordinator approval.
- After entering the worktree, confirm `git status --short --branch` and the
  base commit before editing. If creation is blocked by permissions or a
  branch collision, report it and wait for the coordinator; do not silently
  edit `main`.
- The coordinator removes a worktree only after its changes are integrated or
  explicitly abandoned and the worktree is clean. Workers must not remove
  another worker's worktree.

### Worker Rules

- Run the requested baseline checks before implementation. If the baseline is
  already failing, record the exact command and failure and do not repair
  unrelated failures as part of the task.
- Edit only owned paths. Do not modify another worktree through an absolute
  path, copy files between worktrees, or change shared files opportunistically.
- If progress requires a shared file or an interface owned by another worker,
  stop at the boundary and request a serialized handoff or coordinator change
  to ownership.
- Preserve user and other-agent changes. Never use `git reset --hard`,
  `git checkout --`, `git restore`, `git clean`, or broad deletion to make a
  worktree look clean. Do not rebase or merge moving branches unless the
  coordinator explicitly requests it.
- Read-only explorers return evidence references and findings, not patches.
  Review workers inspect concrete diffs, evidence, and tests without editing
  the implementation under review.
- Keep commits focused and leave generated output, credentials, and ignored
  private notes out of the commit.

### Worker Handoff

Before reporting completion, run `git diff --check`, inspect
`git diff --name-only <base>...HEAD`, and confirm `git status --short`. Use this
report shape so the coordinator can integrate without reconstructing context:

```text
Worktree:
Branch:
Base:
Commits:
Objective and acceptance:
Owned paths:
Changed paths:
Verification:
Baseline or blocked checks:
Known limitations:
Integration notes:
Delivery focus advanced: yes/no, with evidence
```

Do not claim a check passed when it was skipped, unavailable, or only run on a
different commit. Distinguish baseline failures from regressions introduced by
the worker.

### Coordinator Integration

- Inspect the worker's commit and path scope before integrating:
  `git diff --stat <base>...<branch>`, `git diff --check <base>...<branch>`,
  and the handoff report.
- Integrate locally with fast-forward-only history by default. Fetch the target
  branch when needed, then use `git merge --ff-only <source>`; do not copy files
  or manually recreate a worker's patch in another worktree.
- Do not use `--no-ff`, merge commits, squash merges, or rebase-and-merge. If a
  fast-forward is impossible, stop and report the diverged commits; serialize
  the work or obtain explicit approval for a different integration method.
- Resolve conflicts in the coordinator worktree, preserving the ownership
  contract and recording any changed integration decision. If a conflict
  reveals overlapping ownership, stop and reassign or serialize the work.
- Re-run the acceptance checks after integration, then the relevant repository
  checks before reporting the combined result.
- Only after successful integration may the coordinator remove the worker's
  worktree and delete its branch when no further review or rollback is needed.

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

## Explicit Contracts And Dynamic Access

- Initialize every field of a public or cross-layer struct through its
  constructor or an explicit builder. Optional state must be represented as
  `Option` and checked explicitly; do not use a missing value as an implicit
  default when `false`, `0`, or an empty value has different meaning.
- Normalize external JSON, provider payloads, configuration, and PyO3 values
  at the boundary. Do not let `serde_json::Value`, `HashMap<String, Value>`,
  `Box<dyn Any>`, raw Python objects, or provider wire structs become domain
  state.
- Do not use reflection, string-based dispatch, `Any` downcasts, dynamic
  imports, or global registries for ordinary control flow. Use typed enums,
  explicit maps, protocols, registries, or adapters with a documented plugin
  boundary instead.
- Dependencies and ownership must be explicit constructor or function inputs.
  Do not locate services through global mutable state, hidden factories, or
  cross-layer parent traversal.
- Use `TryFrom`, validated constructors, and typed errors for external values.
  Do not use unchecked casts, `unwrap`, `expect`, or assertions to hide a
  missing validation contract in library or runtime code.
- Assertions are appropriate for internal invariants that cannot be supplied
  by external input and for tests. External input failures must return an
  actionable error or a typed failure result.
- Search for an existing capability before adding a helper. Extend the owner
  contract when responsibility is the same; add a new abstraction only when
  ownership, coupling, or testability is genuinely different.

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
- Every spawned task must have a clear creator and owner. Retain its handle,
  provide cancellation and await paths, and inspect its result at the owner
  lifecycle boundary.
- Treat `spawn_blocking`, subprocesses, file handles, network sessions, and
  streams as owned resources. Cancelling a wrapper must not be mistaken for
  cancelling the underlying operation; define timeout, cleanup, and shutdown
  behavior explicitly.
- Handle cancellation as control flow: release owned resources, preserve the
  cancellation signal, and re-propagate it unless the caller explicitly owns
  cancellation recovery.
- Start, stop, retry, and shutdown operations should be predictable and
  idempotent where practical. Do not rely on object destruction or process exit
  to release critical resources.
- Test cancellation, timeout, repeated calls, and exceptional shutdown paths.

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
- Do not use `unwrap`, `expect`, or `panic!` in core, runtime, provider, or
  binding production paths. A narrowly scoped internal invariant may use an
  assertion only when external input cannot supply the invariant and the
  reason is documented.

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
- Do not use `dyn Any`, unchecked downcasts, or stringly typed handler names to
  recover a type that should be represented by a trait, enum, or typed adapter.

### Public API Design

- Keep public APIs boring, typed, and stable.
- Constructors should validate invariants instead of allowing invalid structs to circulate.
- Prefer explicit builders for complex configuration.
- Avoid "stringly typed" control flow.
- Avoid exposing internal storage layout through public APIs.
- Make invalid states unrepresentable when doing so does not overcomplicate the design.
- Keep module visibility narrow. Start with `pub(crate)` unless external use is intended.

## Documentation And Compatibility

- Public modules, types, traits, functions, and methods must have concise
  Rustdoc describing responsibility, relevant inputs/outputs, side effects,
  ownership, and failure behavior.
- Document fields whose role, default, sensitivity, lifecycle, or ownership is
  not obvious from the type alone, especially configuration, sessions, tasks,
  credentials, and injected services.
- Private helpers need a short comment when they enforce an invariant,
  normalize external data, handle security-sensitive values, or coordinate
  cancellation and cleanup.
- Comments explain intent and constraints rather than repeating the code. Keep
  them synchronized with the behavior they describe.
- Compatibility shims must include a `TODO` naming their removal condition and
  tracking issue. Do not add a fallback without an explicit owner and removal
  path.
- Before rewriting existing documentation, preserve non-obvious protocol
  mappings, return-code meanings, invariants, lifecycle constraints, security
  requirements, and operational guidance.

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

- Tests are executable evidence for behavior, contracts, invariants, and failure
  paths. They are not a way to count changed lines or restate the
  implementation.
- Before writing a test, name the observable behavior or contract it protects
  and the regression that would make it fail. If that cannot be stated
  clearly, do not add the test.
- Prefer behavior, interface, integration, and failure-path tests that survive
  internal refactoring and fail when an externally observable contract breaks.
- Do not add tests only to increase coverage, mirror every branch mechanically,
  or confirm that an edited file contains an expected line.
- Do not use raw source reads, substring checks, import order, private fields,
  private call ordering, or implementation details as primary assertions. Exact
  literals are appropriate when they are part of a stable user-visible,
  protocol, or serialized-data contract.
- Test structural rules at the correct level: use dependency-graph checks,
  parsers, `cargo check`, package/build validation, or CI checks for structure.
  Do not replace those checks with hand-written literal searches through files.
- For metadata-, lockfile-, formatting-, configuration-, or workflow-only
  changes, prefer the real consumer (`cargo metadata`, `cargo package`, the
  configured formatter, or CI validation) over a unit test that repeats text.
- Exercise behavior through public constructors, builders, and interfaces. Do
  not bypass initialization by constructing invalid structs, populating private
  fields, or using test-only escape hatches for ordinary behavior. A focused
  lifecycle harness may isolate unavailable platform resources only when it
  initializes the tested contract explicitly.
- Use fakes, stubs, fixtures, or adapters for external services. Cover normal,
  failure, cancellation, timeout, repeated-call, and exceptional-shutdown
  paths.
- Synchronize asynchronous tests with events, futures, cancellation tokens,
  paused Tokio time, or task handles. Do not rely on arbitrary sleeps or
  wall-clock timing except when testing an explicit timeout contract.
- Run the full suite when changing public behavior, lifecycle management, or
  cross-module contracts. Run relevant build, packaging, schema, and CI checks
  when changing dependencies or build configuration.
- Never solve a failing test by deleting coverage, weakening assertions,
  expanding exclusions, or converting failure into success without documenting
  the reason and remaining risk.
- Record environment limitations and remaining risk whenever a check cannot
  run. Distinguish pre-existing failures from regressions introduced by the
  current change.
- Unit tests should cover invariants, parsing, scoring, validation, reducers,
  and typed error mapping. Integration tests should cover runtime event flow,
  artifact/ledger durability, provider boundaries, and public stream behavior.
- Provider-dependent tests must be isolated behind mocks, fixtures, features,
  or explicit opt-in environment variables. Live model behavior must never be
  the only evidence for a deterministic runtime contract.
- If behavior is important enough to encode in `AGENTS.md`, it is a candidate
  for a future test or lint.

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

### Python SDK Quality

- Public Python functions, methods, attributes, and cross-module interfaces
  must have complete annotations. Do not introduce unexplained `Any` into SDK,
  application, or domain contracts. `PyAny` is allowed only at the PyO3
  boundary and must be normalized immediately.
- Do not use unbounded dictionaries to represent runtime state, events,
  configuration, or errors. Use typed dataclasses, enums, Pydantic models,
  `TypedDict`, or explicit type aliases when they express the contract.
- Initialize every instance attribute in `__init__` or an explicit factory.
  Optional state starts as `None` and callers use explicit `is None` checks.
- Do not use `getattr`, `hasattr`, `setattr`, `__dict__`, `globals`, `locals`,
  reflection-style field lookup, or generic `get(name)` APIs to model runtime
  state. Dynamic access is allowed only in a small typed compatibility adapter
  for a third-party boundary, with validation and tests for available and
  unavailable cases.
- Do not use `value or default` when `False`, `0`, an empty value, or an
  explicitly missing value have different meanings. Do not use `cast`,
  `# type: ignore`, or `assert` to conceal a missing contract.
- Constructors may create in-memory configuration, but must not perform
  network IO, spawn background tasks or subprocesses, or register process-wide
  hooks. Expose explicit start/initialize and stop/close/shutdown operations
  for owned resources and workflows.
- Catch only failures that can be handled at that boundary. Do not use blanket
  `except Exception`, `except Exception: pass`, or convert an unknown failure
  into success. Preserve context and make cleanup failures observable.
- Do not use blocking IO or `time.sleep` on an event-loop or UI thread. Use an
  async API or an explicitly owned worker with timeout and cleanup behavior.
- Treat `asyncio.CancelledError` as control flow: release owned resources and
  re-raise unless the caller explicitly owns cancellation recovery. Keep task
  handles for callbacks, timers, `asyncio.create_task`, threads, and
  subprocesses, and provide cancellation, timeout, await, and shutdown paths.
- Use the repository logging mechanism instead of `print()` for production
  diagnostics. Represent failures with explicit errors or result values.
- Keep PyO3 wrappers responsible for conversion and lifecycle bridging only.
  Runtime state, ledger, artifact, permission, retry, and policy behavior stay
  in Rust.
- New or modified Python source files should normally stay within 500 lines.
  Files over 800 lines should be split by responsibility, or the change should
  document why that is not practical. Generated and third-party code is
  exempt. Do not split mechanically just to reduce line count.

## Cross-Language Anti-Patterns

The following Python patterns have Rust equivalents that are also prohibited:

- Python `Any` or an unbounded `dict` maps to Rust `serde_json::Value`,
  `HashMap<String, Value>`, `Box<dyn Any>`, or raw provider payloads. Keep these
  at an explicit boundary and normalize into typed structs/enums/newtypes.
- Python reflection or string-based dispatch maps to Rust `Any` downcasts,
  string handler names, dynamic imports, or hidden registries. Prefer explicit
  enums, maps, traits, and adapters.
- Python global mutable state maps to Rust global mutable registries or
  `Arc<Mutex<Everything>>`. Keep ownership in an explicit runtime/session or
  supervisor.
- Python fire-and-forget tasks map to unowned `tokio::spawn` tasks. Retain the
  handle, define cancellation and await behavior, and inspect the result.
- Python blanket exception handling maps to catch-all Rust errors that erase
  provider, validation, IO, protocol, or cancellation meaning. Preserve typed
  failure categories and source context.
- Python mutable default arguments map to shared mutable Rust defaults or
  reused buffers whose ownership is unclear. Construct per-call state unless
  shared ownership is explicit and tested.

## Security And External Input

- Never expose passwords, tokens, sessions, private keys, API keys, or other
  secrets in logs, tests, issues, pull requests, commits, artifacts, or error
  messages.
- Do not store sensitive data in ordinary configuration files, temporary files,
  or build artifacts.
- Do not bypass authentication, TLS, validation, sandbox, or permission checks
  for convenience.
- Validate every external input at its owning boundary. Treat URLs, file paths,
  redirects, image/content types, environment variables, and subprocess
  arguments as security boundaries.
- Pass subprocess arguments as explicit argument lists. Do not build shell
  strings or enable a shell unless the boundary is deliberate, validated,
  minimum-scoped, and documented.
- Apply timeouts, response-size limits, path scopes, and cancellation to
  network, file, process, and tool operations.
- Security behavior must be covered by automated positive and negative tests;
  callers must not be required to remember the safe calling convention.

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

- Treat `Cargo.toml`, `sdks/python/pyproject.toml`, CI workflows, and project
  documentation as the source of truth for supported runtimes and commands.
- Run the smallest relevant check set during development, then the required
  full checks before submission. A check that was not actually run is
  unverified, not passed.

For every change, also run `git diff --check` and inspect
`git status --short`. Treat a check that could not run as unverified and state
the command, failure, and remaining risk. Separate failures present at the
worker baseline from failures introduced by the change.

For Rust code, expected checks are:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

For Python bindings or SDK code, run the repository's actual package checks:

```bash
(cd sdks/python && uv sync)
(cd sdks/python && uv run --with pytest python -m pytest tests -q)
(cd sdks/python && uv build)
```

When changing dependencies, schemas, package metadata, or build configuration,
also run the relevant `cargo metadata`, `cargo package`, maturin, or CI check.

When changing public behavior, lifecycle management, or cross-module contracts,
run the full Rust and Python suites. When a live provider, local listener,
display server, sandbox, or other environment resource is unavailable, record
the exact limitation and remaining risk instead of weakening the test or
claiming success.

If a check cannot be run, state exactly why and what remains unverified.

## Git Workflow And Merge Policy

- Do not develop implementation work directly on `main` or another default
  branch. Use a dedicated branch and worktree. The coordinator may update the
  default branch only through reviewed fast-forward-only integration.
- Use branch names in the form `<type>/<short-description>`. When a task has
  an Issue, an optional `issue-<number>-` prefix may be included, for example
  `feat/add-runtime-stream` or `refactor/issue-124-split-subagent-lifecycle`.
  Keep one logical task on a branch and keep the branch based on the target
  commit recorded in the task contract.
- Before editing or integrating, inspect `git status --short --branch`,
  `git worktree list --porcelain`, the current branch, and the base commit.
- The preferred local integration is:

  ```bash
  git fetch origin
  git merge --ff-only origin/main
  git merge --ff-only <source-branch>
  ```

- Do not use `--no-ff`, merge commits, squash merges, rebase-and-merge, or
  force-pushes. If `--ff-only` cannot proceed, stop and report the divergence;
  do not automatically rewrite history or invent a merge commit.
- Do not reset, restore, clean, amend, rebase, or otherwise rewrite commits
  authored or signed by another person without explicit approval.
- Preserve existing GPG, SSH, or other commit signatures. Do not disable the
  repository's signing configuration, replace a signing key, or silently turn
  a signed integration into an unsigned rewritten commit. When signature
  verification matters, use `git log --show-signature` or `git verify-commit`
  and record the result.
- If a new commit cannot be signed according to the repository configuration,
  report that condition instead of silently changing the signing policy.
- Review the staged diff before committing. Do not commit caches, credentials,
  build artifacts, generated output, or temporary files.

## Commit Hygiene

- Keep commits focused.
- Use Conventional Commit subjects consistent with the repository history, such
  as `feat(runtime): add checkpoint compaction`, `fix(cli): reject invalid
  config`, `docs(plan): record validation notes`, or `refactor(runtime): name
  checkpoint context explicitly`.
- Use the commit body to explain why the change is needed, how behavior
  changed, and how it was verified. Use `Refs: #<number>` for ongoing work and
  `Closes #<number>` only when the commit or pull request actually completes
  the issue.
- Do not commit ignored private docs.
- Do not commit generated build artifacts.
- Do not include secrets, API keys, local machine paths, or unpublished product strategy in tracked files.
