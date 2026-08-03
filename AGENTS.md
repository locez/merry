# AGENTS.md

This is the root working contract for Merry. It applies to the repository unless
a more-specific `AGENTS.md` governs a descendant path.

## Scope And Authority

- Keep this file limited to stable, repeatable repository rules. Put issue
  lists, temporary migration plans, technical-debt inventories, and design
  drafts in issues, `ROADMAP.md`, or ignored local documentation.
- A nested `AGENTS.md` may add rules for its own subtree when that subtree has
  a genuinely different workflow. It must state its scope and must not copy the
  root rules. When rules conflict, the more-specific file governs its subtree.
- Treat these sources as authoritative for facts that change:
  - `Cargo.toml` defines workspace members, edition, resolver, features, and
    workspace lints.
  - Current source and tests define implemented behavior; this file defines
    intended layer ownership and workflow constraints.
  - `ROADMAP.md` defines the delivery sequence; linked issues define detailed
    scope, acceptance, evidence, and progress.
  - `sdks/python/pyproject.toml`, CI workflows, and package metadata define
    supported Python commands and packaging behavior.
- Verify a path, crate, capability, or command against those sources before
  relying on this file. Report stale instructions instead of silently
  broadening the task.
- `docs/`, `local_docs/`, and `merry-raw-docs/` are ignored local material.
  Keep private notes, plans, specs, and raw source evidence there as
  appropriate; do not commit them or move them into tracked files unless the
  user explicitly asks for that.
- Do not commit private planning material, product strategy, market notes, or
  design drafts unless the user explicitly asks for them.
- Keep implementation changes scoped to the requested task, prefer small
  reviewable changes, and preserve existing changes in a dirty checkout.

## Operating Loop

Use this loop for every substantial task:

1. **Orient.** Read this file, the relevant roadmap section, and the owning
   module and tests. Run `git status --short --branch` before editing.
2. **State the contract.** Name the active gap, observable acceptance target,
   base commit, and allowed write paths.
3. **Inspect.** Search for sibling paths, existing tests, current interfaces,
   and user changes before choosing an owner.
4. **Implement the smallest slice.** Keep one owner per file and preserve the
   runtime, provider, facade, CLI, and binding boundaries in this document.
5. **Verify.** Run deterministic checks for the touched area first, then the
   required repository checks. Record blocked or skipped checks with the exact
   cause.
6. **Report.** Summarize changed files, verification, known limits, and
   whether the active delivery focus advanced.

## Delivery Focus And Roadmap

- The active delivery focus comes from the current `ROADMAP.md` or the user's
  explicit instruction. This file defines how to avoid drift; it must not make
  a temporary milestone the permanent product goal.
- Before substantial implementation, state the capability or gap being
  advanced and at least one command, test, event, artifact, or observable API
  behavior that will prove progress.
- Prefer implementation, tests, and executable verification when the next
  implementation step is known. Research, policy, taxonomy, classifier,
  profile, roadmap, and guardrail work is supporting work unless the user
  explicitly requests it or it unblocks the named acceptance target.
- A documentation, status, planning, or research-only request has its own
  valid acceptance target and is not drift merely because it does not add a
  runtime capability.
- Before reporting completion, state whether the active focus advanced and
  name the evidence. If it did not, identify the resolved blocker or report
  drift.

`ROADMAP.md` is a controlled planning artifact, not a scratchpad:

- Contributors may update implementation status, completed evidence, validation
  notes, or a user-approved correction.
- Do not change product priority, reorder major milestones, or promote a
  supporting architecture thread into the primary track without explicit user
  approval.
- Before an authorized priority change, state the current goal, proposed
  replacement, and authorizing instruction or tracked request. Record the
  reason in `ROADMAP.md` or a tracked spec.
- Keep supporting architecture work subordinate to a runnable or testable
  capability unless the user explicitly asks for planning or design only.

## Ownership And Dependency Boundaries

Merry is runtime-first. Ownership is a design constraint for new work; it does
not require mechanically moving every existing module in one change.

### Layer ownership

- `crates/merry-core` is the contract layer. It owns shared IDs, errors,
  schemas, artifacts, journal/event contracts, and provider-neutral data that
  crosses boundaries.
- `crates/merry-llm` is the model boundary. It owns provider traits,
  normalized model requests, responses, events, capabilities, and
  provider-neutral tool continuations.
- `crates/merry-runtime` is the runtime layer. It owns sessions, task ledger,
  artifacts, context compilation, memory activation, checkpoints, permission
  admission, tool execution, cancellation, and lifecycle state. Runtime state
  is the source of truth for those concerns.
- `crates/merry-provider-openai` and `crates/merry-provider-anthropic` are
  provider adapters. They translate external protocols at the `merry-llm`
  boundary; wire structs and protocol details stay private to those crates.
- `crates/merry-tool-workspace` and `crates/merry-mcp` are resource and tool
  adapters. They connect workspace or MCP capabilities to runtime contracts
  and do not own session, ledger, artifact, or policy state.
- Coding composition is a logical owner for coding prompts, project rules,
  tool catalog, permission policy, validation, retry/recovery, and final
  reporting. A dedicated `merry-coding` crate is not a current workspace
  member; do not create another parallel composition path until that boundary
  is deliberately established.
- `crates/merry` is the Rust facade. It owns application-facing construction
  and event surfaces without exposing provider wire types or CLI details.
- `crates/merry-cli` is the product surface. It owns CLI/TUI interaction,
  configuration, presentation, debug fixtures, and host process/sandbox
  adaptation. It may select providers and pass runtime inputs, but it must not
  own a second runtime state model or coding policy.
- `crates/merry-py` and `sdks/python` are binding surfaces. They expose
  Rust-owned behavior; runtime state, ledger, artifacts, permissions, retry,
  and policy remain in Rust.
- Evaluation protocols, harnesses, and benchmark adapters are upper-layer
  consumers. They may normalize external task formats and consume public
  runtime or facade contracts, but evaluation models must not become runtime
  state or provider wire types.

### Dependency direction

- Production dependencies point from surfaces and adapters toward
  provider-neutral contracts and runtime services. Lower layers must not
  import CLI, PyO3, Python SDK, evaluation, or product-specific composition
  code.
- `merry-core` is independent of `merry-llm`, runtime, providers, composition,
  CLI, and bindings. `merry-llm` may depend on `merry-core`, but not on runtime
  or a concrete provider.
- `merry-runtime` may depend on `merry-core` and `merry-llm`. It must not take
  a production dependency on a provider, CLI, facade, PyO3, or provider wire
  type. Test-only provider fixtures may be dev-dependencies when they stay out
  of production targets.
- Provider crates may depend on `merry-core` and `merry-llm`, but not on
  runtime, CLI, facade, bindings, or another provider's wire protocol.
- Tool adapters, coding composition, facade, CLI, and bindings may depend on
  runtime contracts as appropriate; the reverse direction is forbidden.
  Bindings bridge runtime APIs but do not reimplement runtime ownership or
  policy.
- Provider-specific request and response formats must not cross into
  `merry-core`, `merry-llm`, runtime, composition, facade, CLI, evaluation
  records, or Python public types.
- When a change crosses layers, put the shared contract in the lower owning
  layer and translation, presentation, or orchestration in the higher layer.
  Do not bypass the owner through raw JSON, provider structs, PyO3 objects, or
  string dispatch.
- Add a crate only when the ownership boundary, dependency direction, or
  public compatibility boundary is genuinely different. Record the reason and
  intended dependency direction before adding it to `Cargo.toml`.

### Workspace rules

- Treat the workspace members in `Cargo.toml` as authoritative; do not copy a
  second member list into this file.
- Use the existing Cargo virtual workspace, Rust 2024 edition, and resolver 3.
- Keep features additive. Do not use mutually exclusive feature sets unless
  there is no practical alternative.
- Provider integrations belong in provider crates, not runtime feature flags.
- `unsafe` is forbidden by the workspace lint unless the repository contract
  is intentionally changed.

## Runtime, Context, And Provider Contracts

- Runtime state is structured. Raw chat history is not the source of truth.
- Summaries are navigation; exact evidence remains available through artifacts
  or source reads. Tool output becomes artifacts and compact ledger updates,
  not permanent prompt text.
- Memory activation records why memory entered context. Context compilation is
  deterministic and reproducible from stored runtime state.
- Any change to provider-visible request composition must assess prompt/KV cache
  reuse. This includes system or developer text, ordered context segments,
  tools, tool names, descriptions, input schemas, tool order, output schemas,
  and phase-dependent availability.
- Prefer a stable request prefix throughout a session. Keep provider-visible
  tools and their order stable when runtime admission or typed execution
  validation can enforce behavior without changing the request.
- Do not dynamically add, remove, rename, rewrite, or reorder tools merely to
  represent phase, role, permission, or UI state. Treat those as runtime
  admission policy unless a genuinely different model contract requires a
  separate request surface.
- Place dynamic state after stable instructions and tool definitions when the
  context contract permits it. Cache reuse is an optimization, never a
  correctness requirement. If a cache-breaking request change is necessary,
  state its scope and reason in the change report and add deterministic
  request/schema/order coverage when accidental churn would be costly.
- Merry-owned provider traits and normalized request, response, and event types
  live in `merry-llm`. Tool schemas come from Merry-owned types; provider
  crates render them into provider-specific formats.
- MVP OpenAI-compatible support uses direct `reqwest` in the provider crate
  unless a later plan justifies another wrapper. Do not add a multiprovider
  abstraction that competes with Merry's boundary.
- Provider conversation state is not Merry runtime state. A future OpenAI
  Responses adapter uses `store = false` by default and does not use
  `previous_response_id` as the task ledger.
- Disable parallel tool calls by default until runtime policy supports more
  than one pending tool call.

## Repository Contracts

### Configuration and private material

- Keep the tracked user-facing example at `examples/config.toml`.
- When a `config.toml` key accepted by `merry-cli` is added, removed, renamed,
  or changed, update `examples/config.toml` in the same change unless the
  omission is deliberate and recorded.
- Never put real API keys, host-specific secrets, private endpoints, or local
  machine paths in the example config.
- CLI config tests must continue to parse `examples/config.toml`; schema drift
  should fail deterministically before reaching users.

### Compatibility and serialization

- Serialized structs are explicit and versionable. Use stable field names and
  treat renames as compatibility changes.
- Use `#[serde(deny_unknown_fields)]` for strict external input when forward
  compatibility is not required. Do not serialize internal-only details.
- Schema-generating types are small, documented, and covered by snapshot-style
  checks once their surface stabilizes.

## Git And Change Safety

- Use branch names in this form:

  ```text
  <type>/<short-description>
  ```

  Examples: `feat/add-runtime-stream`, `fix/handle-timeout`.
- Keep one logical task per branch and do not develop directly on the default
  branch.
- Before editing or integrating, inspect `git status --short --branch`, the
  current branch, and the base commit.
- Prefer fast-forward-only branch integration. Fetch the target branch before
  merging and use `git merge --ff-only <source>`.
- Do not use `--no-ff`, squash, rebase-and-merge, force-pushes, or automatic
  history rewrites unless explicitly requested. If fast-forward is impossible,
  stop and report the reason.
- Do not reset, restore, clean, amend, rebase, or otherwise rewrite commits
  authored or signed by another person without explicit approval.
- Do not use destructive Git commands to overwrite existing changes.
- Preserve existing GPG or SSH commit-signing configuration. When signature
  verification matters, use `git log --show-signature` or `git verify-commit`.
  If a new commit cannot be signed according to repository configuration,
  report it instead of changing signing policy.
- Review the staged diff before committing. Do not commit caches, credentials,
  build artifacts, generated output, temporary files, ignored private docs,
  secrets, local machine paths, or unpublished product strategy.
- Use focused Conventional Commit subjects, such as
  `feat(runtime): add checkpoint compaction` or
  `docs(plan): record validation notes`. Explain why, behavior changes, and
  verification in the body. Use `Refs: #<number>` for ongoing work and
  `Closes #<number>` only when the commit or pull request completes the issue.

## Rust Engineering Standards

Use stable Rust unless a feature is explicitly justified. Prefer typed data
structures over stringly typed protocols, `serde` at serialization boundaries,
`schemars` for new tool/provider schemas, `thiserror` for library errors, and
`tracing` for structured diagnostics. Avoid global mutable state and hidden
registration side effects in macros.

### Contracts, types, and dynamic access

- Initialize every public or cross-layer struct through a constructor or
  explicit builder. Represent optional state with `Option` and check it
  explicitly when false, zero, empty, and missing have different meanings.
- Normalize external JSON, provider payloads, configuration, and PyO3 values
  at their boundary. Do not let `serde_json::Value`,
  `HashMap<String, Value>`, `Box<dyn Any>`, raw Python objects, or provider
  wire structs become domain state.
- Do not use reflection, string dispatch, `Any` downcasts, dynamic imports, or
  global registries for ordinary control flow. Use typed enums, explicit maps,
  traits, protocols, or adapters with a documented plugin boundary.
- Pass dependencies explicitly through constructors or functions. Do not find
  services through hidden factories, global state, or cross-layer traversal.
- Use `TryFrom`, validated constructors, and typed errors for external values.
  External failures return actionable errors or typed failure results. Do not
  use unchecked casts, `unwrap`, `expect`, or panic to hide missing validation
  contracts in core, runtime, provider, binding, or other library production
  paths.
- Library crates use typed errors with `thiserror`. `anyhow` is acceptable in
  binaries, examples, tests, and thin CLI layers, but not as the main error
  type for core library APIs. Preserve source errors with `#[source]` or
  transparent variants, include actionable context, and do not collapse
  provider, validation, IO, and protocol failures into one generic error.
- Assertions are for internal invariants that external input cannot violate and
  for tests; document the invariant when it is non-obvious.
- Search for an existing capability before adding a helper. Extend the owner
  when responsibility is the same; add an abstraction only for a genuinely
  different ownership, coupling, or testability boundary.
- Use newtypes for important IDs and protocol names, such as `SessionId`,
  `ArtifactId`, `ToolName`, `SkillId`, and `ProviderName`. Avoid raw `String`
  values for concepts that need validation or stable identity.

### Ownership, allocation, and APIs

- Prefer borrowed read-only inputs: `&str` over `String`, `&Path` over
  `PathBuf`, `&[T]` over `Vec<T>`, and `&T` over `T` when ownership is not
  needed. Store owned data only when the struct retains it.
- Do not clone to appease the borrow checker. A clone should be justified by
  ownership transfer, a task boundary, or a cheap handle such as an `Arc` or
  immutable config snapshot.
- Avoid repeated allocations and large intermediate strings or vectors in
  obvious loops. Use `Cow` only when it clarifies a real borrow-or-own boundary;
  measure before optimizing.
- Keep public APIs typed, stable, and small. Validate invariants in
  constructors, use explicit builders for complex configuration, avoid
  exposing storage layout, and start module visibility at `pub(crate)` unless
  external use is intended.
- Prefer concrete types or generics until dynamic dispatch solves a real
  provider, tool, or plugin boundary. Keep object-safe traits small and use
  capability structs or enums when policy depends on capabilities.

### Async and concurrency

- Use Tokio for the MVP runtime surface. Do not introduce a runtime-agnostic
  executor abstraction yet.
- Runtime and provider event APIs are stream-first. Public dyn/plugin async
  boundaries use explicit `BoxFuture` or `BoxStream`; private traits may use
  native async syntax when dyn compatibility is not needed.
- Use bounded `tokio::sync::mpsc` channels for event production unless another
  source is explicitly justified. Dropping a stream or cancelling its token
  stops producers and prevents new side effects.
- Long-running loops include cancellation checkpoints. Use cooperative
  cancellation and `tokio::select!` where appropriate.
- No observable `RuntimeEvent` claims an artifact, ledger update, or checkpoint
  before that state is durably written.
- Every spawned task has a clear owner, retained handle, cancellation path,
  await path, and result inspection at the owner lifecycle boundary.
- Treat blocking work, subprocesses, file handles, network sessions, and
  streams as owned resources. `spawn_blocking` is for bounded blocking work,
  not a general escape hatch. Define timeout, cleanup, and shutdown behavior.
- Do not perform blocking IO in async paths or hold `std::sync` lock guards or
  mutable borrows across `.await`. Prefer message/event boundaries over broad
  shared state and avoid `Arc<Mutex<Everything>>`.
- Cancellation releases owned resources, preserves the cancellation signal,
  and re-propagates it unless the caller owns recovery. Start, stop, retry, and
  shutdown operations should be predictable and idempotent where practical;
  do not rely on destruction or process exit for critical cleanup.
- Test cancellation, timeout, repeated-call, and exceptional-shutdown paths.

### Modules, unsafe, documentation, and macros

- Keep source files focused and normally under 1000 lines. Treat files near
  that limit as a design signal; extract real ownership boundaries such as
  command handlers, configuration, provider setup, presentation, fixtures, or
  focused test support. Keep entrypoints small and route work to modules.
- Large test blocks do not justify growing production files indefinitely. Move
  related tests with extracted code when a split is warranted. If a file must
  exceed 1000 lines temporarily, explain why and name the follow-up split in
  the final report.
- `unsafe` is forbidden by default. If truly required, isolate it, add a
  `SAFETY:` comment for every block, expose a safe API, test boundary behavior,
  and justify it with evidence rather than using it for unmeasured speed.
- Public modules, types, traits, functions, and methods have concise Rustdoc
  covering responsibility, inputs/outputs, side effects, ownership, and
  failure behavior. Document non-obvious fields and private helpers that
  enforce invariants, normalize external data, handle secrets, or coordinate
  cleanup. Comments explain intent and constraints and stay synchronized.
- Compatibility shims name their removal condition and tracking issue in a
  `TODO`; every fallback has an owner and removal path. Preserve protocol
  mappings, return codes, invariants, lifecycle constraints, security
  requirements, and operational guidance when editing existing docs.
- Macros remove boilerplate only. Expansion must be obvious at the call site;
  macros must not register global state, choose providers, alter context
  compilation, or change execution policy. Prefer explicit registration.

## Python Binding Standards

- Use PyO3/maturin for Python bindings and keep PyO3 and
  `pyo3-async-runtimes` dependencies only in `merry-py`. Keep wrappers thin and
  responsible for conversion and lifecycle bridging, not runtime ownership or
  policy.
- The mixed maturin layout exposes Rust as `merry._merry` with ergonomic
  wrappers in `sdks/python/merry`. Python APIs wrap Rust-owned behavior and
  expose async event iteration as the primary interface.
- Do not call arbitrary Python callbacks from deep runtime code in early
  implementations. Prefer event bridging: Rust emits a tool call, Python
  executes it, and Python returns the result.
- PyO3 types such as `Python<'py>` and `Py<PyAny>` never cross into core,
  runtime, LLM, or provider traits. Rust must not hold the GIL while awaiting,
  blocking, or locking Rust mutexes.

### Python SDK quality

- Treat all first-party Python under `sdks/python` as strongly typed, including
  public APIs, internal helpers, tests, examples, and probes. Public functions,
  methods, attributes, callbacks, and cross-module interfaces must have
  complete parameter and return annotations.
- Model runtime state, events, configuration, errors, and callback protocols
  with concrete dataclasses, enums, Pydantic models, `TypedDict`, `Literal`,
  `Protocol`, or explicit type aliases. Do not expose unbounded dictionaries,
  `dict[str, Any]`, or loosely typed collections as domain contracts.
- `Any` is forbidden in SDK, application, and domain contracts. It is allowed
  only inside a small PyO3/native compatibility adapter and must be normalized
  immediately into a named type. Do not let `Any`, `PyAny`, raw JSON, or
  provider payloads escape that boundary.
- Use precise generic parameters and explicit unions instead of `object`, raw
  containers, or unchecked casts. Do not use `cast`, `# type: ignore`,
  `# ty: ignore`, or broad lint suppressions to conceal a missing type contract;
  a narrow third-party suppression must document its reason and scope.
- Initialize instance attributes in `__init__` or an explicit factory. Optional
  state starts as `None` and uses explicit `is None` checks.
- Do not use `getattr`, `hasattr`, `setattr`, `__dict__`, `globals`, `locals`,
  reflection-style lookup, or generic `get(name)` for runtime state. A small
  typed compatibility adapter for a third-party boundary is allowed only with
  validation and tests for available and unavailable cases.
- Do not use `value or default` when false, zero, empty, or explicitly missing
  values differ. Do not use `assert` to conceal a missing contract.
- Constructors may create in-memory configuration, but must not perform
  network IO, spawn background tasks or subprocesses, or register process-wide
  hooks. Provide explicit start/initialize and stop/close/shutdown operations.
- Catch only failures handled at that boundary. Do not blanket-catch,
  suppress, or convert unknown failures into success; preserve context and
  make cleanup failures observable.
- Do not use blocking IO or `time.sleep` on an event-loop or UI thread. Treat
  `asyncio.CancelledError` as control flow, retain task handles, and provide
  cancellation, timeout, await, and shutdown paths.
- Use the repository logging mechanism instead of `print()` for production
  diagnostics. Represent failures with explicit errors or result values.
- Ruff and ty are required quality gates for every Python SDK change. Both must
  finish with zero diagnostics; do not weaken their configuration or exclude
  first-party code to make a change pass.
- New or modified Python source files should normally stay within 500 lines.
  Split files over 800 lines by responsibility or document why that is not
  practical. Generated and third-party code is exempt.

### Cross-language equivalents

- Python `Any` and unbounded `dict` correspond to Rust `serde_json::Value`,
  `HashMap<String, Value>`, `Box<dyn Any>`, and raw provider payloads. Keep
  them at explicit boundaries and normalize into typed structs, enums, or
  newtypes.
- Python reflection, string dispatch, and hidden global state correspond to
  Rust `Any` downcasts, string handler names, dynamic imports, registries, and
  broad `Arc<Mutex<...>>`. Use explicit enums, maps, traits, adapters, and
  runtime/session ownership.
- Python fire-and-forget tasks correspond to unowned `tokio::spawn` tasks.
  Retain handles and define cancellation, await, cleanup, and result behavior.
- Blanket Python exception handling corresponds to catch-all Rust errors that
  erase provider, validation, IO, protocol, or cancellation meaning. Preserve
  typed failure categories and source context.
- Mutable Python defaults correspond to shared mutable Rust defaults or reused
  buffers with unclear ownership. Construct per-call state unless sharing is
  explicit and tested.

## Security And External Input

- Never expose passwords, tokens, sessions, private keys, API keys, or other
  secrets in logs, tests, issues, pull requests, commits, artifacts, or errors.
- Do not store sensitive data in ordinary config files, temporary files, or
  build artifacts. Do not bypass authentication, TLS, validation, sandbox, or
  permission checks for convenience.
- Validate every external input at its owning boundary. Treat URLs, file paths,
  redirects, image/content types, environment variables, and subprocess
  arguments as security boundaries.
- Pass subprocess arguments as explicit argument lists. Do not build shell
  strings or enable a shell unless the boundary is deliberate, validated,
  minimum-scoped, and documented.
- Apply timeouts, response-size limits, path scopes, and cancellation to
  network, file, process, and tool operations.
- Cover security behavior with automated positive and negative tests; callers
  must not have to remember a safe calling convention.

## Testing And Verification

Tests are executable evidence for behavior, contracts, invariants, and failure
paths. They are not a way to count changed lines or restate implementation.

### Test design

- Before writing a test, name the observable behavior or contract it protects
  and the regression that would make it fail. Do not add tests only for
  coverage, mechanical branch mirroring, or an expected source line.
- Prefer behavior, interface, integration, and failure-path tests that survive
  internal refactoring. Exercise public constructors, builders, and interfaces
  rather than invalid private state or test-only escape hatches.
- Do not use raw source reads, substring checks, import order, private fields,
  private call ordering, or other implementation details as primary assertions.
  Exact literals are appropriate when they are part of a stable user-visible,
  protocol, or serialized-data contract.
- Use fakes, stubs, fixtures, or adapters for external services. Cover normal,
  failure, cancellation, timeout, repeated-call, and exceptional-shutdown
  paths.
- Synchronize async tests with events, futures, cancellation tokens, paused
  Tokio time, or task handles. Do not rely on arbitrary sleeps or wall-clock
  timing except for an explicit timeout contract.
- Test structural rules with dependency-graph checks, parsers, `cargo check`,
  package/build validation, or CI checks, not hand-written literal searches.
  For metadata, lockfile, formatting, config, or workflow changes, prefer the
  real consumer.
- Unit tests cover invariants, parsing, scoring, validation, reducers, and
  typed error mapping. Integration tests cover runtime event flow,
  artifact/ledger durability, provider boundaries, and public streams.
- Isolate provider-dependent tests behind mocks, fixtures, features, or
  explicit opt-in environment variables. Live model behavior is never the only
  evidence for a deterministic runtime contract.
- Never fix a failing test by deleting coverage, weakening assertions,
  expanding exclusions, or converting failure into success without documenting
  the reason and remaining risk. Behavior important enough for this file is a
  candidate for a future test or lint.

### Verification before completion

- Run the smallest relevant checks during development, then the required full
  checks before submission. An unrun check is unverified, not passed.
- For every change, run `git diff --check` and inspect `git status --short`.
  Separate baseline failures from regressions and record environment limits and
  remaining risk for blocked checks.
- Rust changes require, when applicable:

  ```bash
  cargo fmt --all --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all
  ```

- Python binding or SDK changes require the repository package checks:

  ```bash
  (cd sdks/python && uv sync)
  (cd sdks/python && uv run --with ruff --with ty ruff check .)
  (cd sdks/python && uv run --with ruff --with ty ty check)
  (cd sdks/python && uv run --with pytest python -m pytest tests -q)
  (cd sdks/python && uv build)
  ```

- Dependency, schema, package metadata, or build changes also require the
  relevant `cargo metadata`, `cargo package`, maturin, or CI check.
- Public behavior, lifecycle management, or cross-module contract changes
  require the full applicable Rust and Python suites.
  When a live provider, listener, display server, sandbox, or other resource
  is unavailable, record the exact limitation instead of weakening the test or
  claiming success.
- Re-run acceptance checks after branch integration and run relevant repository
  checks before reporting the result.
