# Runtime Builder Completion Design

Date: 2026-06-05

## Purpose

Merry's coding-loop path is real enough that debug commands should stop being
the place where product runtime assembly is discovered. The previous
`SessionRunner` direction was wrong: it wrapped `Runtime` execution even though
`Runtime` already owns session state, event streams, agent loops, artifacts,
context, tool continuations, compaction, and terminal loop results.

The corrected milestone is to make runtime construction profile-driven while
keeping `RuntimeBuilder` as the only construction owner.

This slice landed in this order:

```text
1. Rename the current low-level RuntimeProfile to RuntimeCapabilities
2. Introduce a higher-level RuntimeProfile with a builder
3. Make RuntimeBuilder apply a complete RuntimeProfile through with_profile
4. Move debug-owned coding-agent assembly into profile construction
5. Leave a thin `merry run` consumer as the next product entry
```

Do not introduce a `SessionRunner`, `RuntimeFactory`, or generic
`RuntimeBuilderInstaller`.

## Current Evidence

The repository already has the runtime execution contract:

- `Runtime::run_agent_loop` and `Runtime::run_agent_loop_stream`.
- Provider-neutral `RuntimeEvent` streams.
- Artifact-backed model output, process output, tool results, and structured
  final output.
- Runtime-owned session state, ledger, context, checkpoint compaction, pending
  tool calls, and bridge-tool continuation.
- `AgentLoopConfig` and the coding-agent model-turn default
  `DEFAULT_CODING_AGENT_MAX_MODEL_TURNS`.
- `RuntimeBuilder` with provider roles, compaction, trust, permission review,
  action runner lanes, permissioned process runner factories, subagents,
  skills, project rules, task anchor, and registered tools.
- The old low-level `RuntimeProfile` carried only network and path access
  policy. That shape is now named `RuntimeCapabilities`.
- `WorkspaceCodingLoopProfile` acts as a tool/profile bundle and now composes
  into `RuntimeProfileBuilder` through
  `WorkspaceRuntimeProfileBuilderExt::with_workspace_coding_loop`.
- CLI `bwrap` outer sandbox plus runtime-owned inner action sandbox runners are
  already available.

The problem is not missing execution ownership. The problem is that the coding
agent runtime shape is split across low-level capabilities, workspace tool
registration, process lanes, and debug-owned assembly functions.

## Decision

Use these ownership boundaries:

```text
Runtime
  owns session execution, state, events, tools, artifacts, context, checkpoints,
  continuation, and agent-loop results.

RuntimeBuilder
  owns construction-time session invariants, generic runtime knobs, and applying
  a completed RuntimeProfile.

RuntimeCapabilities
  owns low-level Merry-managed network and path-access grants.

RuntimeProfile
  owns one complete runtime capability shape: capabilities, startup context,
  registered tools, tool action lanes, process runners, permission review
  defaults, and other runtime-owned policy chosen before build.

Profile builders/extensions
  own domain-specific composition, such as workspace tools and coding-agent
  process lanes, and produce a RuntimeProfile.

CLI/config code
  resolves host inputs such as XDG config, workspace root, sandbox handoff,
  provider credentials, process backend, and display format.

Debug commands
  own fixtures, scripted providers, smoke assertions, and reports only.
```

No new long-lived object should own "running a session" or "building a
runtime." If a helper is needed, it should produce `RuntimeProfile` or plain
builder inputs. It must not own `run_agent_loop`, event streaming, terminal
result semantics, or session lifecycle.

## Rejected Abstractions

`SessionRunner` is rejected because it duplicates the `Runtime` execution
facade. A meaningful session abstraction would require resume/persistence
semantics, which this milestone does not implement.

`RuntimeFactory` is rejected because it would compete with `RuntimeBuilder`.
If builder cannot express a construction path well, the fix is to improve
profile construction and `RuntimeBuilder::with_profile`, not add another object
that performs hidden construction.

`RuntimeBuilderInstaller` is rejected as a public concept because it is a
generic mechanism, not a Merry runtime concept. The product API should speak in
profiles and capabilities, not in installers.

`CodingRuntimeFactory` is also rejected. "Coding agent" is a preset or profile
shape, not the identity of a factory type.

## Capabilities And Profile

The current `RuntimeProfile` should be renamed to `RuntimeCapabilities`.

`RuntimeCapabilities` owns only low-level grants consumed by Merry-managed
backends:

- network allowed or denied
- platform-neutral path access rules

It must not register tools, choose providers, run processes, or describe a
product mode.

The new `RuntimeProfile` is the complete runtime shape applied to a builder.
It may contain:

- `RuntimeCapabilities`
- startup context summaries such as project capability hints
- registered tools
- bridge-tool admission choice
- low-risk workspace patch lane opt-in
- low-risk process runner
- read-only shell process runner
- accepted local workspace process runner and admission evidence
- permissioned process runner factory
- runtime trust level and permission review mode
- host permission admission source when explicitly configured
- skill catalog, project rules, task anchor, and subagent manager when those
  have already been resolved by the host layer

It must not contain:

- model provider credentials
- provider wire state
- `run_agent_loop` behavior
- event rendering
- terminal result shape
- Python callback execution
- CLI argument parsing
- physical sandbox bootstrap
- session persistence or resume

## User-Facing Shape

Runtime construction should keep the builder as the subject:

```rust
let capabilities = RuntimeCapabilities::default()
    .with_path_rules(path_rules)
    .deny_network();

let workspace_profile = WorkspaceCodingLoopProfile::new(workspace_config)?
    .with_patch_tool()
    .with_cli_bwrap_permissioned_process_runner(admission, runner, factory);

let profile = RuntimeProfile::builder()
    .capabilities(capabilities)
    .with_workspace_coding_loop(workspace_profile)?
    .build()?;

let runtime = Runtime::builder(session_id)
    .automatic_compaction(compaction)
    .model_provider(provider, model)
    .with_profile(profile)?
    .build()?;
```

`with_workspace_coding_loop` is an extension method supplied by
`merry-tool-workspace`; `merry-runtime` does not depend on the workspace tool
crate. The visible API reads as profile construction, not profile registration
on a builder.

Use `with_profile(profile)` rather than a generic `with(profile)`. The explicit
name prevents the builder from becoming a catch-all extension host while still
making the runtime profile the single high-level assembly object.

## RuntimeBuilder Completion

`RuntimeBuilder` should remain boring and explicit. It should expose generic
runtime construction knobs that belong in `merry-runtime`:

- primary and role-specific model providers
- automatic compaction policy
- runtime trust level and permission review mode for callers that do not use a
  profile
- permission admission source
- permissioned process runner factory
- action runner lanes
- initial project rules, skill catalog, task anchor, and checkpoint context
- runtime-owned subagent manager
- registered tools and bridge-tool admission
- `with_profile(profile)` as the canonical way to apply a completed profile

It should not depend on workspace tools, provider crates, CLI config, Python
callbacks, or debug fixture logic.

When a construction path feels awkward, first check whether the missing piece
is one of these:

- a real runtime invariant that belongs as a `RuntimeBuilder` method
- a profile field or profile-builder method
- a domain extension that helps build `RuntimeProfile` outside `merry-runtime`
- host config parsing that belongs in CLI or a future facade crate
- smoke-only setup that should stay in debug

Only the first category belongs directly on `RuntimeBuilder`.

## Workspace Tool Profile Composition

Workspace tooling has moved from `WorkspaceCodingLoopProfile::register_on` to
profile builder composition.

The workspace crate exposes `WorkspaceRuntimeProfileBuilderExt` around
`RuntimeProfileBuilder` because `merry-runtime` cannot depend on
`merry-tool-workspace`. That extension owns the workspace-specific temporary
state needed to decide whether to register patch and process tools before the
caller finally builds a `RuntimeProfile`.

The important user-facing outcome is:

- users build one `RuntimeProfile`
- workspace tools are part of that profile
- `RuntimeBuilder` applies that profile with `with_profile(profile)`
- no public `profile.register_on(builder)` remains

## Coding-Agent Assembly

The reusable coding-agent assembly path is now profile construction, not a new
architecture owner.

It may own:

- resolving a coding-agent model-turn default for the caller's loop config
- resolving `RuntimeCapabilities` from trusted global config
- loading skill catalog metadata from already resolved roots
- configuring subagent tools when enabled
- configuring workspace read/search/patch/process tools through profile
  construction
- applying action sandbox process backends selected by the host

It must not own:

- `Runtime::run_agent_loop`
- event stream protocol
- final result shape
- terminal UI
- Python callback execution
- provider wire formats
- session persistence or resume

The first implementation remains in `merry-cli` as module-local profile
composition through `with_workspace_coding_loop_profile`. Individual debug
subcommands no longer call a workspace profile `register_on(builder)` path.

A future Rust facade crate named `merry` may expose convenience profile
builders after the runtime/profile boundary stabilizes. That later facade
should still call `RuntimeBuilder`; it should not replace it.

## Debug Command Relationship

Debug commands remain validation consumers.

They may own:

- creating disposable fixture repositories
- selecting deterministic scripted providers for smoke tests
- live smoke opt-in checks
- formatting smoke reports
- asserting smoke-specific tool sequences and fixture contents
- handling `--with-sandbox` bootstrap evidence

They must not own:

- generic coding-agent runtime construction
- generic workspace/profile/action sandbox composition
- model-turn default policy
- generic result semantics
- future `merry run` behavior

## Thin `merry run`

`merry run` should come after profile-driven builder assembly is proven.

It should only:

- load XDG config
- resolve workspace root and configured path/network policy
- build process backends from the selected platform sandbox backend
- construct a `RuntimeProfile`
- call `Runtime::builder(...).with_profile(profile)?.build()?`
- call `Runtime::run_agent_loop_stream`
- render runtime events and the final `AgentLoopResult`

It should not introduce a runner abstraction, duplicate debug smoke assembly,
or redefine runtime status.

## Python SDK Follow-Up

Python bindings remain thin wrappers around Rust-owned behavior.

The SDK should continue to expose direct runtime construction for business
agents and bridge tools. When coding-agent convenience is added, Python should
call the same Rust profile/builder path rather than reimplementing workspace
tools, profile rules, action sandboxing, or loop semantics in Python.

## Session Resume

Session resume is a real future feature, but it does not justify adding
`SessionRunner` now.

A meaningful resume design needs separate runtime primitives:

- persistent session snapshots or logs
- artifact store restoration
- checkpoint/context restoration
- pending tool-call restore policy
- provider/tool reattachment rules
- a `RuntimeBuilder` entry point that can build from restored session state

Until those exist, adding a runner only gives the name "session" without the
resume capability. Resume should be designed as a runtime persistence feature,
then surfaced through CLI/TUI/SDK consumers.

## Acceptance

This milestone is complete. Deterministic tests and smoke-path migration prove:

- The low-level path/network type is named `RuntimeCapabilities`, not
  `RuntimeProfile`.
- `RuntimeProfile` represents a complete builder-applied runtime shape.
- `RuntimeBuilder::with_profile(profile)` applies capabilities, startup
  context, tool registrations, action lanes, process runners, trust/review
  settings, skills, and subagents where present.
- Debug coding-loop commands build a `RuntimeProfile` and pass it to
  `RuntimeBuilder` instead of duplicating equivalent construction logic.
- The shared path still constructs a runtime with provider roles, compaction,
  workspace tools, patch tool, process runner, permissioned process runner
  factory, skills, and subagents where configured.
- `Runtime` remains the only owner of agent-loop execution and result
  semantics.
- `RuntimeBuilder` remains the only public runtime construction owner.
- No `SessionRunner`, `RuntimeFactory`, `RuntimeBuilderInstaller`, or
  `CodingRuntimeFactory` type is introduced.
- No public `profile.register_on(builder)` path remains.
- The coding-agent model-turn default is applied by the run/config layer, not
  hidden inside debug command bodies.
- Process output remains artifact-backed and is not leaked through new product
  result fields.
- Provider wire payloads and secrets are not exposed through profile helpers.

## Verification

Completed verification for the implementation slice:

```bash
cargo test -p merry-runtime
cargo clippy -p merry-runtime --all-targets --all-features -- -D warnings
cargo test -p merry-tool-workspace
cargo clippy -p merry-tool-workspace --all-targets --all-features -- -D warnings
cargo test -p merry-cli
cargo clippy -p merry-cli --all-targets --all-features -- -D warnings
cargo fmt --all --check
git diff --check
```

After this slice, `merry run` can be added as a consumer of the same profile
construction path. A TUI should wait until `merry run` proves the headless
product entry.
