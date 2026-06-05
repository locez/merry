# Session Runner Design

Date: 2026-06-05

## Purpose

Merry's coding-loop capability is now real enough to stop treating debug
commands as the architectural owner. The next milestone is a reusable
`SessionRunner`: one session-running contract that can be consumed by a thin
`merry run` command, future TUI surfaces, SDK bindings, and existing debug
smokes.

Debug commands should remain useful validation tools. They should not continue
to own runtime construction, workspace tool registration, provider/profile
wiring, action sandbox setup, loop budget defaults, or result semantics.

The implementation order is:

```text
1. Library/runtime SessionRunner first
2. Thin headless `merry run` wrapper second
3. Python SDK runner exposure later
```

## Current Evidence

The current repository already has the runtime pieces needed by a runner:

- `Runtime::run_agent_loop` and `Runtime::run_agent_loop_stream`.
- Provider-neutral `RuntimeEvent` streams.
- Artifact-backed model output, tool results, and structured final output.
- Python SDK event streaming and bridge-tool continuation.
- Workspace read/list/search tools and an opt-in workspace patch tool.
- Process action intent/evidence and injected process runner boundaries.
- CLI outer `bwrap` sandbox and profile-backed inner action sandbox.
- XDG TOML provider, observability, permission, and model-role configuration.
- Context budget and checkpoint compaction machinery.
- Subagent runtime/control tools and parent-visible child result reporting.
- Coding-loop debug smokes that prove the inspect/read/patch/verify/final path.

The problem is assembly ownership. Today, much of the product path is assembled
inside `merry-cli` debug commands. That makes debug behavior easy to extend and
the real product contract easy to postpone.

## Non-Goals

- Do not build a full-screen TUI in this milestone.
- Do not make `merry run` the owner of runtime semantics.
- Do not expose a broad raw shell authorization model.
- Do not add approval/session UX beyond what the runner must surface as
  blocked/failed diagnostics.
- Do not wire live model-backed judgment to public authorization.
- Do not implement provider conversation state or OpenAI `previous_response_id`.
- Do not make Python SDK workspace/profile configuration the first consumer.
- Do not move private notes into this spec or tracked docs.

## Core Boundary

`SessionRunner` owns reusable session execution. It is not a debug command, not
a TUI controller, and not a Python-specific facade.

It should own:

- accepting a top-level user task
- resolving one session run's model-turn budget
- constructing or receiving a configured runtime/tool/profile/action backend
- starting the runtime agent loop
- streaming product-relevant events
- collecting terminal status and final output
- returning a compact result

It should not own:

- disposable fixture creation
- smoke-specific scripted provider assertions
- terminal UI layout
- Python callback execution internals
- provider wire formats
- broad approval UX

## Proposed API Shape

Public names should keep the `SessionRunner` prefix unless implementation
evidence shows a narrower module-local name is clearer.

```rust
pub struct SessionRunner {
    // Private fields own resolved session construction state.
}

pub struct SessionRunInput {
    pub task: String,
    pub workspace_root: PathBuf,
    pub max_model_turns: Option<usize>,
}

pub struct SessionRunnerConfig {
    pub provider: Arc<dyn ModelProvider>,
    pub model: ModelName,
    pub process_backend: SessionProcessBackend,
    pub automatic_compaction: AutomaticCompactionConfig,
    pub context_compaction_provider: Option<RuntimeRoleProviderConfig>,
    pub approval_review_provider: Option<RuntimeRoleProviderConfig>,
    pub subagents: SubagentConfig,
    pub skill_roots: Vec<PathBuf>,
    pub allow_hidden_workspace_paths: bool,
}

pub enum SessionRunEvent {
    Runtime(RuntimeEvent),
}

pub struct SessionRunResult {
    pub status: SessionRunStatus,
    pub model_turns_run: usize,
    pub final_output: Option<String>,
    pub final_output_json: Option<FinalOutput>,
    pub events: Vec<RuntimeEvent>,
    pub diagnostic: Option<ErrorInfo>,
}
```

The first version may use direct `RuntimeEvent` passthrough instead of creating
a large product-level event protocol. A `SessionRunEvent::Runtime` wrapper is
enough to leave room for future runner-level events without forcing them now.
Additional config fields should be added only when an accepted runner test
needs them; avoid opaque catch-all option bags.

The coding-agent default budget is `DEFAULT_CODING_AGENT_MAX_MODEL_TURNS`
(`1024`) for one top-level session run. Context compaction may happen inside
the run, but it does not reset this model-turn budget.

## Runtime Construction

The runner should extract the reusable construction path currently embedded in
debug coding-loop commands:

- provider/model application
- context compaction role provider application
- approval review role provider application when already configured
- workspace tool/profile registration
- opt-in patch tool registration
- process runner/action sandbox wiring
- subagent tool registration when enabled
- skill catalog loading when configured
- model-turn budget selection

The first implementation can live in `merry-cli` if that is the smallest safe
step, but its module boundary must be reusable by both debug smokes and the
future thin `merry run` wrapper. It should not remain embedded inside individual
debug subcommand functions.

If the boundary starts in `merry-cli`, the migration path should remain clear:
move it later into a library crate or Rust facade once the runner API stabilizes.

## Debug Command Relationship

Debug commands remain validation consumers.

They may own:

- creating disposable fixture repositories
- selecting deterministic scripted providers for smoke tests
- formatting smoke reports
- asserting smoke-specific tool sequences and fixture contents
- handling `--with-sandbox` bootstrap checks

They must not own:

- generic coding runtime construction
- tool/profile/action sandbox composition
- model-turn default policy
- generic run result shape
- future `merry run` behavior

Existing debug smokes should move toward calling `SessionRunner` with
smoke-specific inputs instead of assembling an equivalent runtime path by hand.

## Thin `merry run`

After the library/runtime runner has deterministic coverage, add a thin
headless command:

```bash
merry run "fix the task"
```

This command should only:

- load XDG config
- resolve the current workspace root
- build `SessionRunnerConfig`
- call `SessionRunner`
- stream a compact event/result view

It must not duplicate debug smoke construction logic.

## Python SDK Follow-Up

Python should consume the stabilized runner contract later. The SDK should not
become the place where workspace tools, action sandboxing, profile rules, or
coding-loop construction are reinvented.

Possible later shape:

```python
result = await merry.run_coding_task("fix this task")
```

or a method on a runtime/session facade. The exact Python API should wait until
the Rust runner contract and thin CLI wrapper prove the boundary.

## Error And Result Semantics

`SessionRunner` should preserve runtime status rather than collapsing failures
into strings:

- `completed`
- `failed`
- `blocked`
- `cancelled`

Blocked results should carry the runtime diagnostic or blocked reason. Examples
include model-turn budget exhausted, pending bridge tool call with no bridge
runner, permission denial, final-output contract mismatch, or action sandbox
denial.

The result should include compact final output fields and artifact references,
but it should not inline raw process stdout/stderr, source body, provider wire
payloads, or secrets.

## Context And Artifacts

The runner should respect the existing context model:

- exact data remains in artifacts or source reads
- summaries and ledger facts are navigation
- context compaction may checkpoint dynamic body state
- ordinary tool results do not enter prompt context unless selected by explicit
  runtime context policy

The runner milestone should not become a new context-taxonomy project. Context
changes are in scope only when needed to make runner behavior deterministic and
diagnosable.

## Acceptance

The first slice is complete when deterministic tests prove:

- `SessionRunner` can run a fake-provider/fake-runner coding task through
  inspect/read/patch/verify/final.
- Runtime events stream in the expected order.
- Completed, failed, blocked, cancelled, and structured final-output outcomes
  map to `SessionRunResult`.
- `DEFAULT_CODING_AGENT_MAX_MODEL_TURNS` is applied when no explicit budget is
  supplied.
- Profile/action sandbox settings are passed through to process execution.
- Process output is artifact-backed and not leaked through runner result
  fields.
- Provider wire payloads and secrets are not included in runner events/results.
- Existing debug smokes can either call the runner directly or have a clearly
  identified migration path to do so.

After that slice, a thin `merry run` command can be added as a consumer test of
the runner contract.
