# Merry

Merry is an early-stage Rust-first agent runtime project. APIs and crate boundaries are still unstable.

M8 runtime/provider/tool execution hardening has shifted into maintenance and foundation work: structured runtime state, artifact-backed model output, provider step boundaries, pending tool calls, tool result resolution, tool continuations, registered tool execution, public runtime API contract cleanup/review/alignment, and opt-in OpenAI debug/tool flows remain the base for later runtime work.

Memory Activation MVP work is internally integrated in `merry-runtime`. The default activation source is a session-owned in-memory stored source; external/default sessions have no candidate memories until runtime-owned state records them. There is still no public memory write API, external persistence, or stable activation contract.

M18F-H LLM-assisted judgment boundary work has added a crate-private checked internal context append helper used by summary-draft promotion. Promotion still compiles the candidate context snapshot before mutating session context, while public direct context writes remain raw/manual.

Public direct context writes remain unchanged. `Runtime::record_context_entry` and `Runtime::record_context_summary` are still raw/manual MVP context mutation helpers: they append direct context entries and rely on later context compilation to validate exact evidence readability. They are not summary-draft promotion, do not create promotion lifecycle records, and are not governed by promotion acceptance/replay rules. The summary-draft promotion lifecycle remains crate-internal.

Judgment remains advisory semantic input only: it is not connected to a live LLM, public judgment API, tool execution gate, public runtime event, ledger fact, or automatic context promotion.

Deterministic verification is based on fake providers and stored runtime state. Live provider flows are manual and opt-in, not required for normal tests.

See [ROADMAP.md](ROADMAP.md) for the current public status.

## Repository Notes

- Engineering rules for agents and contributors live in [AGENTS.md](AGENTS.md).
- Project lead operating rules live in [PROJECT_LEAD.md](PROJECT_LEAD.md).
- Development subagent workflow lives in [SUBAGENT_WORKFLOW.md](SUBAGENT_WORKFLOW.md).
- Local product and design notes are intentionally ignored by git.
- Do not commit private planning documents unless they have been explicitly reviewed for public exposure.
