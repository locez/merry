# Crate Boundary Cleanup Report

Date: 2026-06-06

## Purpose

This report records the current crate-boundary and module-boundary cleanup
backlog after the `merry-cli` routing cleanup. It is an engineering-quality
track, not a replacement for the active product roadmap.

The goal is to make future work easier to land without growing hidden
"everything files" or letting test/debug/SDK/product concerns collapse into the
same module.

## Scope

This is a quick-scan report over the current Rust crates:

- `merry-core`
- `merry-llm`
- `merry-runtime`
- `merry-provider-openai`
- `merry-tool-workspace`
- `merry-cli`
- `merry-py`

No code behavior is changed by this report.

## Evidence Snapshot

Observed file sizes from the repository snapshot:

```text
10706 crates/merry-runtime/src/runtime.rs
 5839 crates/merry-tool-workspace/src/lib.rs
 5062 crates/merry-runtime/tests/provider_boundary.rs
 3796 crates/merry-runtime/src/session.rs
 3726 crates/merry-runtime/src/judgment.rs
 3214 crates/merry-runtime/src/subagent.rs
 2319 crates/merry-runtime/src/context.rs
 2066 crates/merry-runtime/src/process.rs
 1520 crates/merry-runtime/src/tool.rs
 1404 crates/merry-py/src/runtime.rs
 1098 crates/merry-runtime/src/agent_loop.rs
 1044 crates/merry-runtime/src/checkpoint.rs
  926 crates/merry-provider-openai/src/provider.rs
  900 crates/merry-cli/src/run.rs
```

Observed crate structure:

```text
crates/merry-core
crates/merry-llm
crates/merry-runtime
crates/merry-provider-openai
crates/merry-tool-workspace
crates/merry-cli
crates/merry-py
```

The largest remaining boundary problems are now in `merry-runtime` and
`merry-tool-workspace`, not in `merry-cli`.

## Priority Ranking

### P0: `merry-runtime/src/runtime.rs`

Observed:

- `runtime.rs` is 10706 lines.
- It contains `Runtime`, `RuntimeBuilder`, provider-step execution, event
  emission, retry-event mapping, memory activation, context compilation,
  automatic compaction, assistant output recording, tool-call pending/bridge
  events, process artifact/ledger helpers, diagnostics, cancellation, and
  event-permit helpers.

Derived:

- This file is the highest-risk engineering hotspot. It is not just long; it
  mixes runtime facade, construction, provider driving, context/compaction,
  memory, event durability, and action/process glue.
- Future work such as session resume, UI event display, SDK stream semantics,
  retry bugs, compaction bugs, and tool/result lifecycle fixes will keep
  colliding in this file.

Recommended cleanup sequence:

1. Extract provider-step pipeline from `runtime.rs` without changing behavior.
2. Extract runtime event emission helpers, including durable event reservation
   and failure/cancellation send paths.
3. Extract assistant output and tool-call normalization helpers.
4. Extract automatic-compaction orchestration from the provider-step path.
5. Move process-action artifact/ledger helpers closer to process modules if the
   process boundary remains stable.

Scope guard:

- Do not change `Runtime` public execution semantics during the first cleanup.
- Do not introduce a new session runner/factory abstraction.
- Keep `RuntimeBuilder` as the construction owner.

### P1: `merry-tool-workspace/src/lib.rs`

Observed:

- `lib.rs` is 5839 lines.
- It contains workspace profile composition, tool config/limits, read/list/search
  executors, patch executor, patch parser, patch planner, patch execution,
  path validation, symlink/open safety, output envelopes, guidance text,
  tracing, and tests.

Derived:

- The crate boundary is conceptually right: workspace tools are outside
  `merry-runtime`.
- The internal module boundary is not right yet. `lib.rs` is effectively a
  whole workspace-tool runtime.

Recommended cleanup shape:

```text
src/lib.rs
src/config.rs
src/profile.rs
src/state.rs
src/read.rs
src/list.rs
src/search.rs
src/patch/mod.rs
src/patch/parse.rs
src/patch/plan.rs
src/patch/execute.rs
src/path.rs
src/envelope.rs
src/trace.rs
src/guidance.rs
```

Recommended cleanup sequence:

1. Move pure config/profile/state types first.
2. Move read/list/search tools one by one.
3. Move path validation into a dedicated module with focused tests.
4. Move patch parser/planner/executor last because it is the most coupled.

Scope guard:

- Preserve the current path-safety behavior and residual TOCTOU disclaimer.
- Do not change tool schemas or output envelopes while splitting modules unless
  a separate product change explicitly asks for it.

### P2: `merry-py/src/runtime.rs`

Observed:

- `runtime.rs` is 1404 lines.
- It contains production OpenAI-compatible construction and bridge-tool
  streaming.
- It also contains `_with_fake_response`, `_with_scripted_tool_call`,
  `_with_scripted_tool_calls`, `FakeModelProvider`, `ScriptedModelProvider`,
  test-scenario parsing, Python event conversion, stream handling, schema
  parsing, retry parsing, and result conversion.

Derived:

- The Python SDK has production and test/scenario support mixed in the same
  binding file.
- This is risky because the SDK is supposed to be a thin ergonomic wrapper
  around the Rust runtime, not a second runtime scenario system.

Recommended cleanup sequence:

1. Split production `PyRuntime` construction and methods from conversion
   helpers.
2. Move bridge-tool stream handling into a dedicated module.
3. Move Python serde/event/result conversion into dedicated modules.
4. Move fake/scripted providers into test-only support if possible, or at least
   into a clearly internal testing module.

Scope guard:

- Do not remove the deterministic Python test support until replacement tests
  exist.
- Keep public Python APIs ergonomic, but avoid exposing fake provider concepts
  as normal SDK surface.

### P3: `merry-runtime/src/judgment.rs`

Observed:

- `judgment.rs` is 3726 lines.
- It contains judgment domain types, records, registry, model-backed judgment,
  prompt rendering, validation, payload rendering, and summary-draft promotion
  support.

Derived:

- This is a coherent subsystem, but it is becoming a second large runtime
  island.
- It will likely grow as permission review, summary promotion, tool-risk
  review, and model-backed advisory features mature.

Recommended cleanup shape:

```text
src/judgment/mod.rs
src/judgment/types.rs
src/judgment/registry.rs
src/judgment/model_source.rs
src/judgment/prompt.rs
src/judgment/render.rs
src/judgment/validation.rs
```

Scope guard:

- Keep judgment crate-private until the public contract is intentionally
  designed.
- Do not let judgment-policy work displace executable runtime/product
  acceptance targets.

### P4: `merry-cli/src/run.rs` and `merry-cli/src/cmd.rs`

Observed:

- `main.rs` is 70 lines after cleanup.
- `run.rs` is 900 lines.
- `cmd.rs` is 572 lines.
- `merry-cli` now has separated routing, config, sandbox, debug, and coding
  runtime modules.

Derived:

- CLI is no longer the worst boundary problem.
- `run.rs` is close to the 1000-line soft limit and contains runtime assembly,
  human output rendering, JSONL output, and tests.
- `cmd.rs` is acceptable for now but may need splitting if command-generation
  behavior grows.

Recommended cleanup sequence:

1. Do not split immediately unless new `run` behavior is added.
2. If `run.rs` grows again, extract human output rendering first.
3. If `cmd.rs` grows again, extract command plan rendering/execution from
   runtime construction and prompting.

Scope guard:

- Do not over-focus on CLI cleanup while `runtime.rs` and workspace tools remain
  much larger architecture risks.

### P5: `merry-provider-openai`

Observed:

- The provider crate already has `config.rs`, `error.rs`, `parse.rs`,
  `provider.rs`, `render.rs`, and `wire.rs`.
- `provider.rs` is 926 lines.
- `lib.rs` includes substantial tests.

Derived:

- This crate has the healthiest nontrivial internal boundary among the
  product-facing crates.
- It can be improved, but it is not the next bottleneck.

Recommended cleanup:

- Move large `lib.rs` tests into focused test modules if they grow further.
- Keep provider wire types private.
- Keep retry policy outside provider-specific implementation.

### P6: `merry-core` and `merry-llm`

Observed:

- `merry-core` is split by artifact/error/event/evidence/id/schema/tool.
- `merry-llm` is split by capability/error/event/provider/request/response/
  retry/testing/tool/usage.

Derived:

- These are comparatively healthy foundational crates.
- They are not priority cleanup targets right now.

Recommended cleanup:

- Preserve their provider-neutral shape.
- Avoid pushing runtime policy, provider wire structs, or CLI/SDK concerns down
  into these crates.

## Suggested Execution Order

Recommended near-term order:

```text
1. Runtime provider-step extraction
2. Runtime event-emission extraction
3. Workspace tool config/profile/state extraction
4. Workspace read/list/search extraction
5. Python SDK runtime binding split
6. Runtime judgment module split
7. CLI run renderer split only if run grows again
```

The first two runtime extractions should be no-behavior-change commits with
deterministic runtime tests. They should not attempt session resume, new
profile policy, or event protocol changes.

## Acceptance For Cleanup Work

Each cleanup slice should satisfy:

- No public behavior change unless explicitly requested.
- No provider wire leakage into runtime/core/llm.
- No new runner/factory/session wrapper competing with `Runtime`.
- Existing deterministic tests pass for the touched crate.
- Large file line count moves in the intended direction.
- New modules have names that explain ownership, not vague names such as
  `helpers`, `utils`, or `misc`.

Expected verification for Rust cleanup slices:

```bash
cargo fmt --all --check
cargo clippy -p <touched-crate> --all-targets --all-features -- -D warnings
cargo test -p <touched-crate>
```

For runtime cleanup that changes shared behavior paths, prefer:

```bash
cargo test -p merry-runtime
cargo test -p merry-cli
```

