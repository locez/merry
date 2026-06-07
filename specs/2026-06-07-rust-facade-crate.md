# Rust Facade Crate

Date: 2026-06-07

## Purpose

Add a Rust facade crate named `merry` before 0.1.0 external testing.

The facade is the public interface layer for applications embedding Merry. It
must expose small, composable building blocks, not product-shaped wrappers or
provider-specific runtimes.

The current low-level crate graph is directionally correct:

- `merry-core` owns shared ids, errors, schemas, and event contracts.
- `merry-llm` owns provider traits and normalized model request/event types.
- `merry-runtime` owns runtime state, sessions, events, tools, profiles,
  permissions, final output, compaction, and agent-loop semantics.
- `merry-provider-openai` adapts OpenAI-compatible APIs into `merry-llm`.
- `merry-tool-workspace` adapts workspace filesystem tools into
  `merry-runtime`.

The missing layer is a clean public API that lets users assemble those pieces
without learning every internal crate on day one.

## Design Principle

There is only one Merry runtime.

Providers are components. Profiles are components. Tools are components.
Runtime construction is composition of components.

This must never become:

```rust
merry::OpenAiRuntime::builder(...)
merry::GeminiRuntime::builder(...)
merry::ClaudeRuntime::builder(...)
```

That shape incorrectly makes provider choice look like a runtime subtype and
forces every future provider to grow a parallel runtime API.

The intended shape is:

```rust
let provider = merry::providers::openai_compatible()
    .api_key(api_key)?
    .base_url(base_url)?
    .model("gpt-4.1-mini")?
    .retry_policy(retry)
    .build()?;

let profile = merry::profiles::workspace_coding(root)
    .patch_tool()
    .process_runner(runner)
    .build()?;

let runtime = merry::Runtime::builder(session_id)
    .with_provider(provider)
    .with_profile(profile)?
    .build()?;
```

Later providers should be symmetrical:

```rust
let provider = merry::providers::gemini()
    .api_key(api_key)?
    .model("gemini-...")
    .build()?;
```

Both examples produce the same provider component type and feed the same
`RuntimeBuilder`.

## Decision

Create `crates/merry` as the public Rust facade crate.

The facade may depend on:

- `merry-core`
- `merry-llm`
- `merry-runtime`
- provider crates such as `merry-provider-openai`
- tool/profile crates such as `merry-tool-workspace`

Lower-level crates must not depend on `merry`.

The facade should make the right composition path obvious while keeping each
component's owner clear. It should not replace `RuntimeBuilder`; it may add
small extension methods or component builders that feed `RuntimeBuilder`.

## Non-Goals

- Do not introduce a new session runner abstraction.
- Do not introduce provider-specific runtime types.
- Do not move runtime state, event protocol, artifact storage, ledger,
  compaction, agent loop, tool execution, permission review, or final-output
  semantics out of `merry-runtime`.
- Do not parse CLI config, XDG config, env vars, or sandbox bootstrap flags in
  the facade.
- Do not own platform sandbox implementation details. CLI may still build
  process runners and pass them in.
- Do not expose provider wire structs.
- Do not hide runtime control flow behind macros or global registration.
- Do not make Python callbacks enter deep Rust runtime code.
- Do not make the facade a catch-all `pub use` dump of every unstable internal
  type.

## Target Crate Graph

The desired long-term graph is:

```text
merry
  -> merry-core
  -> merry-llm
  -> merry-runtime
  -> merry-provider-openai
  -> merry-tool-workspace

merry-cli
  -> merry
  -> CLI-only config/sandbox/terminal modules

merry-py
  -> TBD after dependency audit
```

Lower-level crates stay acyclic:

```text
merry-core              -/-> merry
merry-llm               -/-> merry
merry-runtime           -/-> merry
merry-provider-openai   -/-> merry
merry-tool-workspace    -/-> merry
```

## Public API Shape

Root exports should stay small:

```rust
merry::Runtime
merry::RuntimeBuilder
merry::SessionId
merry::ModelName
merry::providers
merry::profiles
merry::tools
merry::events
merry::errors
```

More specialized items should live under named modules instead of the crate
root.

### `providers`

Provider builders construct a normalized provider component, not a runtime.

Proposed type:

```rust
pub struct ConfiguredModelProvider {
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    retry_policy: Option<ModelRetryPolicy>,
}
```

The exact name can change, but the role must stay provider-neutral.

`RuntimeBuilder` can then accept this component:

```rust
impl RuntimeBuilder {
    pub fn with_provider(self, provider: ConfiguredModelProvider) -> Self;
}
```

If adding an inherent method in `merry-runtime` is too broad for the first
slice, `merry` may provide a facade extension trait:

```rust
pub trait RuntimeBuilderProviderExt {
    fn with_provider(self, provider: ConfiguredModelProvider) -> Self;
}
```

Provider-specific builders live under `providers`:

```rust
let provider = merry::providers::openai_compatible()
    .api_key(api_key)?
    .base_url(base_url)?
    .model(model)?
    .retry_policy(retry_policy)
    .build()?;
```

This builder may validate `OpenAiProviderConfig` and `ModelName`, and it may
instantiate `OpenAiProvider`, but it must return a provider component rather
than a runtime.

Forbidden names:

- `OpenAiRuntime`
- `OpenAiRuntimeBuilder`
- `GeminiRuntime`
- `ClaudeRuntime`

Acceptable names:

- `OpenAiCompatibleProviderBuilder`
- `ConfiguredModelProvider`
- `ProviderConfig`
- `providers::openai_compatible()`

### `profiles`

Profile builders construct `RuntimeProfile` or a narrow profile component that
is immediately convertible into `RuntimeProfile`.

Example:

```rust
let profile = merry::profiles::workspace_coding(root)
    .allow_hidden(false)
    .limits(limits)
    .patch_tool()
    .read_only_process_runner(runner)
    .build()?;
```

This should delegate to `merry-tool-workspace` and `merry-runtime` profile
builders. It must not duplicate workspace tool registration logic.

Profile names should say profile, not runtime:

- Good: `WorkspaceCodingProfileBuilder`
- Bad: `WorkspaceCodingRuntimeProfile`

### `tools`

Expose tool registration/execution contracts needed by SDKs and Rust users:

- `RegisteredTool`
- `ToolExecutor`
- `ToolExecutionContext`
- `ToolExecutionOutcome`
- `ToolExecutionError`
- `ToolRunner`
- tool specs and names from `merry-core`

Do not introduce global tool registration or hidden side effects.

### `events`

Expose event protocol types:

- `RuntimeEvent`
- `RuntimeEventKind`
- `PendingToolCall`
- `ToolCallId`
- artifact references needed to consume events

Do not expose provider wire events.

### `errors`

Expose stable error/info types:

- `ErrorInfo`
- `MerryErrorInfo`
- `MerryRetryability`
- `RuntimeError`
- provider config errors that callers must handle

## Re-Export Policy

Re-exports must be explicit and categorized.

The facade should not promote implementation-facing internals such as raw
ledger/checkpoint/context compiler types unless there is a concrete public API
use case.

The root module should avoid huge lists. Prefer module namespaces:

```rust
pub mod providers;
pub mod profiles;
pub mod tools;
pub mod events;
pub mod errors;

pub use merry_runtime::{Runtime, RuntimeBuilder};
pub use merry_core::SessionId;
pub use merry_llm::ModelName;
```

## Agent Loop Defaults

Expose default config helpers:

```rust
merry::generic_agent_loop_config() -> Result<AgentLoopConfig, ...>
merry::coding_agent_loop_config() -> Result<AgentLoopConfig, ...>
```

These should use runtime constants:

- generic SDK default: 128 model turns
- coding-agent default: 1024 model turns

No new semantics; just centralize the public default choice.

## Python SDK Dependency Audit

Do not assume `merry-py` should depend on `merry` just because `merry` is the
facade crate.

`merry-py` has two separate responsibilities:

1. PyO3/native binding layer around Rust runtime internals.
2. Ergonomic Python SDK layer that exposes Python-native configuration, tools,
   async event streams, and Pydantic handling.

That means the dependency decision needs a boundary audit.

### Current Direction

`merry-py` should use `merry` for user-facing SDK construction by default.

This includes Python config paths that mirror public Rust composition:

- provider configuration
- model selection
- retry policy
- runtime profile construction once Python workspace support is real
- agent-loop default choices

This does not mean `merry-py` must hide every lower-level type behind the
facade. PyO3/native binding internals may keep direct dependencies on lower
crates when they need exact Rust types for event streams, tool bridge
continuations, native error conversion, fake/scripted test providers, or other
binding mechanics.

The boundary is:

- If Python is assembling the same production runtime component a Rust user
  would assemble, prefer the `merry` facade.
- If Python is translating between Python objects and Rust runtime internals,
  direct lower-level crate access is allowed and should stay narrow.

The facade should grow only the public component APIs needed to keep SDK
construction ergonomic. It should not expose every low-level runtime structure
just to make bindings easier.

### Reasons `merry-py -> merry` may be right

- Python users are external consumers. They should see the same public
  composition semantics as Rust users.
- Provider construction should not be repeated independently in Python binding
  code if `merry` already owns provider component builders.
- Python `RuntimeConfig` should map to the same provider/profile components as
  Rust facade users.
- Keeping Python on the facade catches public API gaps early.

### Reasons `merry-py -> merry` may be wrong or premature

- PyO3 bindings sometimes need lower-level exact types for event conversion,
  stream driving, bridge tool result submission, fake provider tests, and
  native error mapping.
- If `merry` is intended as a Rust user-facing facade only, making `merry-py`
  depend on it may blur binding internals with external Rust ergonomics.
- Python-specific behavior such as Pydantic schema extraction and Python
  callback bridging should not be forced into `merry`.
- If `merry-py` uses `merry` only to avoid one import from
  `merry-provider-openai`, that may be dependency churn rather than a real
  boundary improvement.

### Required Audit Before Migrating `merry-py`

Before changing `merry-py` dependencies, answer these questions in the spec or
implementation note:

- Which `merry-py` code paths are binding internals and should stay on
  low-level crates?
- Which code paths are user-facing SDK construction and should use facade
  components?
- Does the facade expose provider/profile components at the right granularity
  for Python config, or does Python still need provider-specific lower-level
  APIs?
- Can Python bridge tools remain event-mediated without `merry` knowing PyO3
  types?
- Does using `merry` reduce duplicated production construction logic without
  hiding runtime control flow?

Expected split if migration is justified:

- Production provider/profile construction may use `merry` components.
- Event stream conversion, PyO3 classes, Python bridge callback execution,
  fake/scripted provider tests, and native error conversion may keep direct
  low-level dependencies.

If the audit does not show a real boundary improvement, leave `merry-py` on
low-level crates for now and revisit after the Rust facade is stable.

## Python `WorkspaceConfig`

Python `RuntimeConfig.workspace` must not be silently ignored.

Valid first-slice options:

- Make it real by mapping it to facade `profiles::workspace_coding(...)`, if
  the facade profile component is ready and the Python semantics are clear.
- Explicitly reject it with a typed `MerryConfigError` until native workspace
  support is wired.
- Remove it from the public Python config until it can work.

Silent no-op configuration is not allowed.

## CLI Migration

The CLI should keep ownership of:

- config file parsing
- env var lookup
- XDG path rules
- sandbox/bwrap planning and process backend construction
- terminal rendering
- interactive command confirmation

The CLI should gradually stop owning:

- generic provider component construction after config is parsed
- generic workspace profile composition
- agent-loop default construction

The first CLI migration should happen only after the facade component API is
stable enough to avoid another round of provider-specific runtime naming.

## Proposed Module Shape

```text
crates/merry/
  Cargo.toml
  src/lib.rs
  src/providers/mod.rs
  src/providers/openai_compatible.rs
  src/profiles/mod.rs
  src/profiles/workspace_coding.rs
  src/tools.rs
  src/events.rs
  src/errors.rs
  src/agent_loop.rs
```

This module shape is a guideline, not an excuse to create empty files. Start
with the smallest modules that keep names clear.

## Acceptance

Implementation is complete when:

- `crates/merry` exists and is included in the workspace.
- There are no provider-specific runtime types in the public API.
- OpenAI-compatible construction returns a provider component consumed by the
  generic runtime builder.
- Future providers can fit the same provider component shape without adding a
  new runtime type.
- Workspace coding construction returns a profile component or `RuntimeProfile`,
  not a runtime.
- Root re-exports are small and clear; specialized APIs live under named
  modules.
- Python `RuntimeConfig.workspace` either works through a real profile path,
  is explicitly rejected, or is removed.
- `merry-py` dependency on `merry` is decided by the dependency audit above,
  not assumed.
- No lower-level crate depends on `merry`.
- `cargo test -p merry` passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.

## Risks

- A too-large facade becomes another dumping ground. Keep the root small and
  module boundaries clear.
- Provider-specific runtime names will permanently bias the API in the wrong
  direction. Reject them early.
- Re-exporting too much may accidentally freeze unstable internals. Use
  explicit, categorized re-exports.
- Migrating Python or CLI before the component shape is right can spread the
  wrong abstraction into more surfaces.
- Workspace/profile convenience must not bypass runtime policy. The facade
  should build `RuntimeProfile`; `RuntimeBuilder::with_profile` remains the
  application point.

## Recommended Implementation Order

1. Remove provider-specific runtime API from the first facade draft.
2. Add provider-neutral `ConfiguredModelProvider` or equivalent.
3. Add `providers::openai_compatible()` returning that component.
4. Add `RuntimeBuilder::with_provider(...)` or a facade extension trait.
5. Add agent-loop default helpers.
6. Add workspace coding profile construction helper under `profiles`.
7. Audit `merry-py` dependency boundaries before moving production paths to the
   facade.
8. Only then migrate CLI/Python construction paths that clearly benefit.
