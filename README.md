# Merry

Merry is an early-stage Rust-first agent runtime project. APIs and crate boundaries are still unstable.

M8 runtime/provider/tool execution hardening has shifted into maintenance and foundation work: structured runtime state, artifact-backed model output, provider step boundaries, pending tool calls, tool result resolution, tool continuations, registered tool execution, public runtime API contract cleanup/review/alignment, and opt-in OpenAI debug/tool flows remain the base for later runtime work.

Memory Activation MVP work has started inside `merry-runtime`. Internal activation shapes, deterministic projection/evidence validation, and the provider-step timing skeleton are in place. A production memory store, public API, external persistence, and stable activation contract are not complete yet; the default production memory source remains noop.

Deterministic verification is based on fake providers and stored runtime state. Live provider flows are manual and opt-in, not required for normal tests.

See [ROADMAP.md](ROADMAP.md) for the current public status.

## Repository Notes

- Engineering rules for agents and contributors live in [AGENTS.md](AGENTS.md).
- Project lead operating rules live in [PROJECT_LEAD.md](PROJECT_LEAD.md).
- Development subagent workflow lives in [SUBAGENT_WORKFLOW.md](SUBAGENT_WORKFLOW.md).
- Local product and design notes are intentionally ignored by git.
- Do not commit private planning documents unless they have been explicitly reviewed for public exposure.
