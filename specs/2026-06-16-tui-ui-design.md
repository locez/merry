# TUI UI Design

Date: 2026-06-16

## Purpose

Define the first full-screen TUI product shape for Merry.

The TUI is the default product entrypoint for interactive coding sessions. It
must consume runtime-owned sessions, event streams, tools, queues, artifacts,
permission results, and usage state. It must not become a second runtime,
session manager, approval system, artifact store, or global project dashboard.

## Entry Model

`merry` with no subcommand opens the TUI in the current directory:

```text
merry             # TUI
merry run ...     # headless task runner
merry cmd ...     # headless command planner
merry debug ...   # diagnostics and smoke tools
```

There is no `merry tui`, no `--continue`, and no TUI startup flag surface in the
first version.

Each TUI launch creates a new session by default. The current directory is the
workspace root. Provider, model, profile, sandbox, skills, subagents,
compaction, and observability configuration come from the same construction
paths used by the headless coding runtime.

The first TUI is not a global session manager. Session id, usage details, and
runtime diagnostics are available through details or the command panel, but the
primary visual identity is the current workspace session.

## Technology Stack

The first implementation should live in `merry-cli` as a focused `tui` module.
Do not add a new public runtime or facade crate for the TUI. If the module grows
large or becomes reusable outside the CLI, split it into a dedicated crate later
around real ownership boundaries.

Use this stack:

- `ratatui` for terminal layout, widgets, styling, and rendering.
- `crossterm` as the terminal backend for raw mode, alternate screen, resize,
  keyboard, bracketed paste, event stream, and terminal control.
- Existing Tokio runtime orchestration for the TUI event loop.
- Existing `Runtime::start_interactive_agent_run` for runtime execution.
- Existing `RuntimeEvent` projection for UI-visible runtime activity.
- Existing XDG TOML configuration path for TUI keymap/theme/display settings
  when those settings become accepted config keys.

Crossterm is an explicit first-version choice, not a backend abstraction point.
Ratatui defaults to the Crossterm backend, and Codex's current TUI is useful
prior art for this stack: it uses Ratatui plus Crossterm with bracketed paste
and event-stream support. Merry should use crates.io releases first unless a
specific terminal bug justifies pinning a fork.

Keep Crossterm-specific code concentrated in the terminal/event/input boundary.
It is acceptable for key event conversion code to use `crossterm::event`
types, but rendering models, runtime event projection, queue state, theme
state, and timeline state should remain independent of Crossterm.

The TUI event loop should `tokio::select!` over:

- terminal input events
- runtime interactive events
- redraw ticks
- internal UI commands

Do not block the async runtime on terminal input. Prefer Crossterm's event
stream integration. If a terminal operation must be blocking, keep it isolated
inside the TUI terminal boundary and out of runtime/tool execution paths.

Keep UI state separate from runtime state:

- `TuiController` owns terminal setup, event loop, and command dispatch.
- `TuiState` owns selected view, scroll offsets, expansion state, keymap,
  theme, queue preview state, and detail-view state.
- `TuiProjector` converts `RuntimeEvent` values into UI timeline items and
  status updates.
- `TuiRenderer` renders `TuiState` with Ratatui widgets.
- Runtime session state, artifacts, queues, tools, permission review, and
  provider execution remain owned by `merry-runtime`.

The first version should use a small internal text input model instead of
immediately depending on a textarea widget crate. Merry's `Enter` behavior,
`next`/`backlog` actions, and configurable keymap are product-specific enough
that the input semantics should be explicit. A third-party textarea widget can
be evaluated later if it fits the chosen Ratatui/Crossterm versions and does
not fight Merry's queue semantics.

Patch, process, artifact, and permission displays should use internal display
models derived from runtime events and artifacts. Do not add a generic diff or
syntax-highlighting dependency in the first slice unless the runtime/tool
payloads prove insufficient for the file-level diff view.

## Main Layout

The first screen is a Codex-style single-session workbench with three
persistent regions:

```text
state  cwd  model  sandbox/profile  usage
------------------------------------------------------------
assistant text

muted activity: read/search/list
expanded diff: patch
muted or expanded result: process/test/permission/subagent
------------------------------------------------------------
Next
  1. add tests for config parsing
  2. then simplify the error text
Backlog
  1. update README example
Suspended
  1. previous interrupted instruction...
input ...
```

The conversation/activity stream is the main surface. Assistant messages are
visually dominant. Tool and runtime events are inline activity blocks with lower
visual weight unless the event changes files, reports failure, records a
permission result, or carries subagent failure diagnostics.

The bottom region owns active input and queue awareness. It shows actual
pending input text for `next`, `backlog`, and `suspended`. Long items are
single-line truncated with ellipsis, and the full text is available through
queue management. On short terminals, the queue preview uses bounded height and
prioritizes `next`, then `suspended`, then `backlog`.

## Event Display

Assistant output is the primary reading path. Runtime and tool events are
evidence and progress.

Default display rules:

- `workspace_read_file`, list, and search events are collapsed muted blocks by
  default, showing the tool name, path or query summary, hit count or byte/line
  count, and artifact id when useful.
- Patch events are expanded by default as file-level diffs, close to `git
  diff`. Long patches may fold by file or hunk, but the first view must show
  what changed.
- Process and test events show command, cwd, status, duration, and exit code.
  Failed commands show the key stdout/stderr excerpt expanded. Long output
  opens in the detail viewer.
- Permission requests do not trigger human approval in the first version.
  `request_permissions` results are rendered inline as automatic
  review/blocked/denied/executed cards, including requested capability, exact
  action, review source, risk, and rationale when available.
- Subagent spawned/started/status events are muted. Completed subagents show
  summary, changed paths, and output paths. Failed or cancelled subagents render
  as diagnostic blocks.
- Usage events update the status bar and usage detail view. They should not
  become noisy main timeline entries.
- Final output renders as assistant text. Terminal status appears in the status
  bar.

## Input And Queues

The TUI uses the existing runtime interactive lanes.

- The `submit_next` action submits the current input to `next`.
- If the runtime is waiting, `next` starts immediately.
- If the runtime is running, `next` waits for the current model/tool boundary
  and then preempts backlog.
- Multiple `next` inputs submitted before the boundary are accepted together as
  one burst in the next provider request.
- A separate configurable action submits to `backlog`.
- `backlog` is the normal automatic serial queue. After the current work and
  any higher-priority `next` burst, the runtime accepts one backlog item and
  continues.
- Interrupt moves pending `next` into `suspended`; backlog remains ordered.
- `suspended` can be resumed or discarded from the command panel.

All keybindings are configurable. The design defines action names and default
suggestions only; no specific key is a permanent product contract.

Queue management supports viewing, editing, deleting, and reordering pending
`next`, `backlog`, and `suspended` entries. Accepted entries are immutable
history.

## Permissions

The first TUI does not implement a human approval admission source.

This is intentional. Current runtime permission support already has
model-backed approval review and host admission source injection, but the public
event stream does not yet have first-class pending human approval events. A TUI
human-review bridge should be designed later as a runtime-visible admission
flow, not as an unobservable side channel hidden inside the UI.

For the first version:

- Automatic review approval or denial is displayed inline.
- Blocked review or missing permissioned execution is displayed inline with
  actionable diagnostics.
- The UI does not enter approve/deny mode.
- The UI does not write persistent permission grants.
- The UI does not remember approvals for a session or globally.

## Detail Viewer

The TUI has a lightweight detail viewer, not a full multi-tab workspace.

From a selected event or command panel action, the user can open details for:

- artifact content
- full diff
- long stdout/stderr
- source excerpts
- permission review payload
- subagent output paths
- session and usage details

The detail viewer is temporary and returns to the conversation without losing
scroll position.

## Command Panel

The command panel is a lightweight current-session action surface. It supports:

- searching/selecting commands
- opening selected event details
- queue management
- recovering or discarding suspended inputs
- viewing keymap
- showing or copying session id
- showing usage/session details
- saving and quitting
- toggling event expansion policy

It is not a global session browser and not a file explorer.

## Theme And Keymap

The default visual style is a restrained engineering-tool theme:

- low visual noise
- assistant text as the primary reading layer
- muted activity blocks
- explicit semantic colors for risk and status
- clear diff add/delete colors
- focused selection states for keyboard navigation

Colors must be configured through semantic theme names rather than hard-coded in
components. First-version semantic color slots should include at least:

- `status`
- `muted`
- `focus`
- `selection`
- `diff_add`
- `diff_delete`
- `warning`
- `error`
- `risk`
- `success`

Keyboard interaction is first-class. Mouse support is not a first-version
acceptance target. If the selected TUI backend provides simple mouse scrolling
without architectural cost, it can be treated as a non-contractual enhancement.

## Testing And Acceptance

Tests should protect stable contracts without pretending to validate subjective
interaction quality.

Expected first-version checks:

- `merry` with no subcommand routes to the TUI entrypoint.
- Existing `merry run`, `merry cmd`, and `merry debug` parsing and routing do
  not regress.
- Queue view-model behavior preserves runtime lane semantics for `next`,
  `backlog`, `suspended`, and accepted immutability.
- Event display defaults are covered at the view-model level for patch,
  process/test, permission result, final output, and usage status updates.

Avoid broad terminal golden snapshot suites in the first version. Real UX
validation should come from interactive long-session use and user feedback.

## Non-Goals

- No global session manager.
- No `merry tui` subcommand.
- No `--continue` startup behavior.
- No human approval admission source.
- No persistent permission grant management.
- No full file explorer.
- No dashboard-style multi-pane monitoring UI.
- No large terminal golden snapshot suite as a substitute for UX feedback.

## References

- Ratatui: https://ratatui.rs/
- Crossterm: https://docs.rs/crossterm/latest/crossterm/
- tui-textarea, considered but not selected for the first input model:
  https://docs.rs/tui-textarea/latest/tui_textarea/
