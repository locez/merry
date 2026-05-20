# Merry

Merry is an early-stage Rust-first agent runtime project. APIs and crate boundaries are still unstable.

M8 runtime/provider/tool execution hardening has shifted into maintenance and foundation work: structured runtime state, artifact-backed model output, provider step boundaries, pending tool calls, tool result resolution, tool continuations, registered tool execution, public runtime API contract cleanup/review/alignment, and opt-in OpenAI debug/tool flows remain the base for later runtime work.

Memory Activation MVP work is internally integrated in `merry-runtime`. The default activation source is a session-owned in-memory stored source; external/default sessions have no candidate memories until runtime-owned state records them. There is still no public memory write API, external persistence, or stable activation contract.

M18F-E LLM-assisted judgment boundary work has added an internal completed-judgment audit registry, exact internal request/outcome carriers, crate-private summary-draft audit recording, and a session-owned summary-draft promotion lifecycle with exact replay idempotency. Judgment remains advisory semantic input only: it is not connected to a live LLM, public API, tool execution gate, public runtime event, ledger fact, or automatic context promotion.

Deterministic verification is based on fake providers and stored runtime state. Live provider flows are manual and opt-in, not required for normal tests.

See [ROADMAP.md](ROADMAP.md) for the current public status.

## Repository Notes

- Engineering rules for agents and contributors live in [AGENTS.md](AGENTS.md).
- Project lead operating rules live in [PROJECT_LEAD.md](PROJECT_LEAD.md).
- Development subagent workflow lives in [SUBAGENT_WORKFLOW.md](SUBAGENT_WORKFLOW.md).
- Local product and design notes are intentionally ignored by git.
- Do not commit private planning documents unless they have been explicitly reviewed for public exposure.
