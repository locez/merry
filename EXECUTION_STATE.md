# Execution State

Lease status: complete

## Source Of Truth

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `ROADMAP.md`
- `README.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`
- `docs/design/mvp-design.md` (ignored private local design source)
- `docs/design/global-design.md` (ignored private local design source)
- `docs/product/product-strategy.md` (ignored private local product source)
- `merry-raw-docs/` (ignored original local source material; do not commit)

## Planning Maturity

Level: implementation-in-progress

Current planning artifact:

- `ROADMAP.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- M1 Structured Process Boundary MVP.

Session milestone:

- Context/cache instrumentation slice: make the provider-neutral request record
  a stable prefix boundary that includes runtime-owned base instructions and
  tool profile metadata, with dynamic context hashed separately.

Goal:

- Prove that Merry can distinguish the cacheable provider-neutral request
  prefix from dynamic context: base instructions and tool specs affect the
  stable prefix hash, while compiled context, user input, and tool
  continuations affect the dynamic context hash.

Task queue status:

- Added provider-neutral request content hash type and `ModelRequest` metadata:
  `stable_prefix_message_count`, `stable_prefix_hash`, and
  `dynamic_context_hash`.
- Preserved and validated existing `tool_profile_hash`.
- Added an explicit stable-prefix constructor for requests that know their
  runtime-owned prefix boundary.
- Runtime provider request compilation now emits a minimal stable base system
  message before dynamic compiled context and user input.
- Runtime request tracing now includes `stable_prefix_message_count`,
  `tool_profile_hash`, `stable_prefix_hash`, and `dynamic_context_hash`
  without prompt text or provider wire payloads.
- Tests updated to account for the stable base message and to prove dynamic
  context remains outside the stable prefix hash.

Allowed expansion:

- Provider-neutral request metadata needed for cache-boundary observability.
- Runtime provider request compilation and trace metadata needed to expose the
  stable/dynamic split.
- Focused test updates in runtime, LLM, workspace integration, and continuity
  status files.

Done condition:

- `merry-llm` tests prove stable prefix hash changes for base instructions or
  tool profile changes and dynamic hash changes for dynamic context changes.
- Runtime provider-boundary tests prove dynamic compiled context does not
  perturb the stable prefix hash while changing the dynamic context hash.
- Provider adapters do not receive runtime cache metadata as provider wire
  state.
- Focused and full default validation pass.
- Handoff updated and lease committed.

Drift boundary:

- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.
- Do not make live provider behavior part of default tests.
- Do not expand this slice into full prompt/personality design.

Task type: implementation/docs

Acceptance criteria:

- `model_request_stable_prefix_hash_tracks_base_instructions_and_tools`
  demonstrates base instructions and tools are part of the stable prefix hash.
- `model_request_rejects_non_system_stable_prefix_message` rejects accidental
  user/dynamic content inside the stable prefix.
- `model_request_rejects_mismatched_context_hashes` validates serialized hash
  metadata.
- `compiled_provider_request_stable_prefix_hash_tracks_base_instructions_and_tools_only`
  proves runtime dynamic context changes only the dynamic hash.
- `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  and `cargo test --all` pass.

## Scope

Allowed edits:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `crates/merry-llm/src/lib.rs`
- `crates/merry-llm/src/request.rs`
- `crates/merry-llm/tests/protocol.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/step.rs`
- `crates/merry-runtime/tests/agent_loop.rs`
- `crates/merry-runtime/tests/provider_boundary.rs`
- `crates/merry-tool-workspace/tests/runtime_integration.rs`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content
- full-screen TUI, REPL, or multi-turn UI scope

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo test -p merry-llm --test protocol stable_prefix`
- `cargo test -p merry-runtime --test provider_boundary stable_prefix`
- `cargo test -p merry-runtime --test provider_boundary`
- `cargo test -p merry-runtime --test agent_loop`
- `cargo test -p merry-llm --test protocol`
- `cargo test -p merry-runtime --lib`
- `cargo test -p merry-tool-workspace --test runtime_integration coding_loop_harness_inspects_patches_verifies_and_completes`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `git diff --check`
- `git status --short --untracked-files=all`

Validation notes:

- Initial full `cargo test --all` failed only on tests that assumed the first
  runtime provider message was user/dynamic context; those assertions now
  account for the stable base system message at index 0.
- Full default validation passed after those test updates.
- Live provider and real bwrap smoke lanes were not run; they remain explicit
  opt-in lanes and are not required for this provider-neutral metadata slice.

## Research

Research required: yes

Research reason:

- The user asked whether Codex has default system/base instructions and whether
  that should be considered in the prefix cache design.

Research artifact:

- Local ignored Codex source under `.merry/codex` showed Codex resolves
  `base_instructions`, includes model instruction templates, and sends those
  instructions as part of model requests. This informed the decision to make
  Merry's base instructions part of the stable prefix boundary.

## Next Action

Next exact action:

- Continue M1 from `ROADMAP.md`: extend the structured process boundary by
  routing classified process intents through read-only and workspace-write /
  sandbox permission profiles, starting with known read-only inspection
  commands and local workspace verification commands.

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before reusable runtime/process/tool profiles are
  clearer.
- Do not move private Codex/raw-doc findings into tracked source text.
- Do not treat base prompt wording as finalized; the completed contract is the
  stable-prefix boundary and metadata, not a long-term prompt/personality.
