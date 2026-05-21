# Merry

Merry is an early-stage Rust-first agent runtime project. APIs and crate boundaries are still unstable.

## Current Coding-Agent Boundary

Merry is not yet a full coding-agent runtime. The current workspace tool
surface is a read-only evidence and navigation foundation, not the main
side-effect path for coding-agent behavior.

The next coding-agent boundary is runtime-owned side-effect protocols: Action
Policy, Workspace Patch/Write, and Shell/Process Protocol. These protocols need
to make write and process actions explicit runtime actions with artifacts,
ledger/checkpoint awareness, cancellation boundaries, bounded execution, and
deterministic fake-runtime coverage before they are treated as normal agent
capabilities.

Shell/process support is not being rejected as out of scope. The direction is to
bring it into a runtime-owned reliable audit path: policy-gated,
artifact-backed, ledger/checkpoint-aware, cancellable, bounded,
deterministic-testable, and provider-neutral. LLM judgment may remain advisory
semantic input, but it is not the authorization gate for side-effectful runtime
actions.

M8 runtime/provider/tool execution hardening has shifted into maintenance and foundation work: structured runtime state, artifact-backed model output, provider step boundaries, pending tool calls, tool result resolution, tool continuations, registered tool execution, public runtime API contract cleanup/review/alignment, and opt-in OpenAI Responses debug/tool flows remain the base for later runtime work.

The first Runtime Agent Loop MVP slice is implemented in `merry-runtime` as a bounded serial loop over the existing `Runtime::step`, registered tool execution, and continuation step primitives. It returns ordered runtime events and typed completed/failed/cancelled/blocked outcomes without adding provider wire state or real filesystem/shell tools. Tool execution cancellation during the loop is reported as a cancelled loop status while leaving the pending tool call unresolved.

The `merry-tool-workspace` slice has moved from the read-file first slice into
read-only workspace navigation/search: it exposes registered
`workspace_read_file`, `workspace_list_dir`, and `workspace_search_text` tools
for UTF-8 reads, non-recursive directory listing, and bounded literal text
search under explicitly configured workspace roots. It is still not a shell,
write API, network API, or full coding-agent surface. Read-only
navigation/search is foundation and maintenance work; future write and
shell/process capabilities belong in runtime-owned action protocols rather than
provider-specific flows or bare shell escape paths.

The workspace path-safety contract assumes trusted, stable workspace roots. The
MVP prevents ordinary path traversal and ordinary symlink traversal before
read/list/search operations, and on Unix uses `O_NOFOLLOW` for file opens so a
symlink swapped into a file leaf path is rejected. It is not an OS sandbox and
does not claim complete hardening against malicious concurrent filesystem
mutation, such as replacing intermediate directories during validation/open;
that remains a residual TOCTOU risk.

The OpenAI provider targets the Responses API only. The provider request path is `/responses`; it keeps the Merry-owned `merry-llm` provider boundary intact, keeps OpenAI wire types private to `merry-provider-openai`, sets `store: false`, omits `previous_response_id`, avoids provider conversation state as Merry runtime state, and keeps `parallel_tool_calls: false` until runtime policy supports parallel tool calls. This provider work does not imply a live/OpenAI judgment path or public judgment API.

Memory Activation MVP work is internally integrated in `merry-runtime`. The default activation source is a session-owned in-memory stored source; external/default sessions have no candidate memories until runtime-owned state records them. There is still no public memory write API, external persistence, or stable activation contract.

M18F LLM-assisted judgment boundary work is closed out through M18F-I. It reserved an internal advisory judgment boundary, audit carrier, summary-draft promotion lifecycle, uncertainty review harness, and crate-private checked internal context append helper. Promotion still compiles the candidate context snapshot before mutating session context, while public direct context writes remain raw/manual.

The internal model-backed judgment foundation now includes a strict tool-risk review model-output parser, crate-private provider-neutral `ModelBackedJudgmentSource`, and deterministic fake-provider runtime harness proof through `Runtime::run_uncertainty_review`. It remains advisory and fake-provider deterministic only.

Public direct context writes remain raw/manual MVP context mutation helpers. `Runtime::record_context_entry` and `Runtime::record_context_summary` append direct context entries and rely on later context compilation to validate exact evidence readability, but they now share runtime active-step admission and return `StepAlreadyActive` while a step, agent loop, or tool execution owns the session. They are not summary-draft promotion, do not create promotion lifecycle records, and are not governed by promotion acceptance/replay rules. The summary-draft promotion lifecycle remains crate-internal.

Judgment remains advisory semantic input only: it is not a public API, public summary-draft API, public runtime event, ledger fact, tool execution gate, automatic provider-context inclusion, automatic context mutation or promotion, OpenAI/live-provider path, or builder/runtime configured judgment source.

Deterministic verification is based on fake providers and stored runtime state. Live provider flows are manual and opt-in, not required for normal tests.

See [ROADMAP.md](ROADMAP.md) for the current public status.

## Repository Notes

- Engineering rules for agents and contributors live in [AGENTS.md](AGENTS.md).
- Project lead operating rules live in [PROJECT_LEAD.md](PROJECT_LEAD.md).
- Development subagent workflow lives in [SUBAGENT_WORKFLOW.md](SUBAGENT_WORKFLOW.md).
- Local product and design notes are intentionally ignored by git.
- Do not commit private planning documents unless they have been explicitly reviewed for public exposure.
