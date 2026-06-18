# TUI Cockpit Layout Design

## Goal

Redesign Merry's TUI from a Codex-like single transcript into a Merry-owned
agent cockpit that uses wide terminals well.

The TUI should make the agent's workflow visible: conversation, current working
context, task/queue state, tool activity, and usage should each have an obvious
home. It must still keep the existing fast terminal workflow: type at the
bottom, submit with Enter, use configurable shortcuts, scroll the timeline with
the mouse wheel, jump to prior user turns with the review shortcut, and keep the
input area stable.

## Current Problems

The current layout is mostly one vertical transcript plus a bottom input/status
area. It works, but it has three product problems:

- Wide terminals waste the right side, making Merry feel like a Codex clone.
- Queue, run state, usage, and recent tool activity compete with transcript
  content instead of living in a stable status surface.
- High-signal artifacts such as patches, command output, file context, or MCP
  results appear only inline, so the user must scan the chat stream to find the
  thing the agent is currently acting on.

The redesign should solve those product problems without changing the runtime
event protocol in the first implementation slice.

## Non-Goals

- Do not move the transcript to terminal scrollback. Merry keeps app-owned
  timeline scrolling and app-owned mouse wheel handling.
- Do not try to make native terminal mouse text selection work in this spec.
  That remains a separate copy/review-mode problem.
- Do not require a new runtime plan-event protocol for the first slice.
- Do not change model prompts, provider behavior, tool execution, or MCP
  registration.
- Do not introduce a session manager, landing page, or a command hierarchy.

## Layout Model

The bottom interaction area stays full width. The content above it becomes a
responsive cockpit.

```text
Wide terminal, 170 columns or wider:

+-----------------------+-----------------------+----------------+
| CHAT                  | FOCUS                 | PLAN           |
| conversation stream   | current artifact      | task + queue   |
| user/assistant/tools  | patch/output/file/MCP | usage + tools  |
+-----------------------+-----------------------+----------------+
| run state / animation / elapsed time                            |
| input box                                                       |
| cwd / model / reasoning / usage                                 |
+-----------------------------------------------------------------+
```

### Wide Mode

Use wide mode when the terminal is at least 170 columns.

Default column ratios:

- Chat: 40%
- Focus: 38%
- Plan: 22%

Minimum useful widths:

- Chat: 48 columns
- Focus: 50 columns
- Plan: 28 columns

If those minimums cannot all be met, fall back to medium mode rather than
producing cramped columns.

### Medium Mode

Use medium mode from 120 to 169 columns.

Default layout:

- Left column: Chat, about 62% width.
- Right column: Work rail, about 38% width.
- The right column is split vertically into Focus above Plan.

This keeps the transcript readable while still giving the current artifact and
task state a visible place.

### Narrow Mode

Use narrow mode below 120 columns.

Default layout:

- Single content column: Chat.
- Queue preview falls back to the existing bottom queue area above input.
- Focus and Plan are represented by compact status summaries only.

This keeps small terminals and remote sessions usable instead of forcing a
three-column mental model into a narrow screen.

## Pane Responsibilities

### Chat Pane

The Chat pane owns the chronological conversation stream.

It shows:

- User turns.
- Assistant prose.
- Compact tool call rows.
- Tool output previews.
- Inline patches when no Focus pane is available.
- Assistant-turn separators.

It does not own:

- Persistent queue display in wide or medium mode.
- Detailed current artifact display when a Focus pane is visible.
- Usage dashboards or task checklist state.

In wide and medium mode, inline timeline entries should remain compact when the
same content is promoted into Focus. A patch should still be visible in Chat as
a meaningful event, but the full diff can live in Focus.

### Focus Pane

The Focus pane is the current work surface. It answers "what is the agent
working with right now?"

Initial focus selection is derived from existing TUI timeline state:

1. Latest patch diff.
2. Latest expanded or failed tool output.
3. Latest command output preview.
4. Latest file/context/MCP result preview.
5. Empty state if none exists.

The first implementation slice should not add independent Focus scrolling or
selection. It should render a clipped, high-signal view of the latest selected
artifact. Later slices can add pane focus, pinned artifacts, and independent
scroll.

Focus titles should be explicit:

- `FOCUS patch hello_world.py`
- `FOCUS command python3 hello_world.py`
- `FOCUS MCP openaiDeveloperDocs/search_openai_docs`
- `FOCUS file crates/merry-cli/src/tui/render.rs`

### Plan Pane

The Plan pane is the stable agent-status rail. It answers "where is this run
going, and what is queued?"

It shows:

- Current run state and elapsed time.
- Current task, derived from the latest submitted user input.
- Queue lanes: Next, Suspended, Backlog.
- Recent tool activity with compact names.
- Model, reasoning effort, and usage.
- MCP/tool status summaries when visible in current state.

Plan is intentionally rightmost in wide mode because it is stable navigation and
status, not the primary reading surface. The middle pane is reserved for the
volatile thing the agent is actively using.

The first slice may derive the task line from the latest user message rather
than requiring explicit model-authored plan steps. Future runtime plan events
can replace or enrich that derived view.

## Bottom Interaction Area

The bottom area remains full width in all modes.

Order from top to bottom:

1. Run state line: `Ready`, animated `Running`, `WaitingForInput`, elapsed time,
   and last-run duration.
2. Completion menu, when active.
3. Input box.
4. Status line: cwd, model, reasoning effort, last input/output tokens, total
   token usage.

The input box must keep existing behavior:

- Enter submits.
- Configured newline shortcut inserts a newline.
- Up and Down use input history when completion is not active.
- Tab accepts completion when completion is active.
- Large pasted text can stay compact in the editor while expanding on submit.
- Ctrl-C clears input first and exits on the configured repeated action.

The completion menu should remain visually attached to the input area. It should
not resize the Chat, Focus, or Plan panes differently between frames.

## Mouse And Review Behavior

Merry keeps app-owned timeline scrolling.

- Mouse capture stays enabled.
- Mouse wheel events scroll the Chat timeline, not the input widget.
- Wheel speed should remain faster than single-line scrolling.
- Ctrl-U jumps to the previous user turn in the Chat timeline.
- Repeated Ctrl-U continues moving to earlier user turns.
- In review mode, Enter returns to the bottom before accepting new input.

This spec deliberately prioritizes predictable timeline navigation over native
terminal text selection. A later copy/review mode can support selecting rendered
content inside the app.

## Visual Direction

Merry should look like a restrained pink terminal cockpit, not a generic dark
dashboard.

Rules:

- Use pink as the dominant accent for borders, headings, and active elements.
- Assistant text remains readable white/gray, not all pink.
- Inline code uses pink text without heavy background blocks.
- Patches keep green/red diff backgrounds and line numbers.
- Pane borders are single-line terminal regions, not nested cards.
- No yellow-dominant UI.
- No rounded card metaphors.
- Avoid filling every pane with decoration. Empty space is acceptable when it
  preserves readability.

Pane titles use short uppercase labels:

- `CHAT`
- `FOCUS`
- `PLAN`

## Data Model

The first implementation should add render-only view models under the TUI
module. Runtime state remains unchanged.

Recommended view types:

- `CockpitLayoutMode`: `Wide`, `Medium`, `Narrow`.
- `CockpitRects`: calculated terminal regions for chat, focus, plan, run line,
  completion, input, and status.
- `FocusPanelView`: derived latest artifact selection and display lines.
- `PlanPanelView`: derived run state, current task, queue lanes, usage, and
  recent activity.

These view models should be pure functions over `TuiState` plus terminal size.
They should be unit-testable without crossterm or a live runtime.

If `render.rs` grows further during implementation, split rendering by real
ownership boundaries:

- `layout.rs`: responsive region calculation.
- `panels/chat.rs`: transcript rendering.
- `panels/focus.rs`: focus artifact rendering.
- `panels/plan.rs`: status rail rendering.
- `panels/input.rs`: input, completion, and status strip rendering.

Do not split mechanically before the new boundaries exist.

## Content Derivation

The first implementation should avoid new runtime events and derive panels from
existing state:

- Current task: most recent submitted user timeline item.
- Queue lanes: existing `QueuePreviewState`.
- Usage: existing `UsageSummary` state.
- Run state: existing `RunState` and elapsed timing fields.
- Recent tools: latest tool-like timeline items, compacted to one line each.
- Focus artifact: latest patch/tool/file/MCP output candidate.

If derivation has insufficient data, render a compact empty state instead of
inventing model-authored plan text.

## Edge Cases

- Very short terminals must preserve input and status first, then Chat. Focus
  and Plan can disappear.
- Wide but shallow terminals may use wide columns with clipped panels, as long
  as input remains usable.
- Long task text in Plan truncates with an ellipsis.
- Long queue items in Plan show the direct content prefix, not only counts.
- Long command output in Focus clips to the available height and keeps the first
  high-signal lines.
- A terminal resize recomputes layout from the current size every frame.
- No pane may draw outside its assigned region.

## Acceptance Criteria

Observable behavior:

- At 180 columns, the top content area renders Chat, Focus, and Plan side by
  side.
- At 140 columns, Chat renders beside a stacked work rail containing Focus and
  Plan.
- At 100 columns, the UI renders a single Chat column and preserves the current
  bottom interaction behavior.
- In wide and medium mode, queue lanes are visible in the Plan pane.
- In narrow mode, queue lanes fall back to the bottom queue preview.
- Mouse wheel scrolls the Chat timeline in a real TUI session.
- Ctrl-U review still jumps between user turns.
- Enter in review mode returns to the bottom before normal submission.
- Input cursor placement remains correct with ASCII, CJK text, and compacted
  paste placeholders.
- No rendered text overlaps the input border or status line.

Verification commands:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p merry-cli tui
cargo test --all
```

Focused tests should cover:

- Layout mode selection at 180, 140, and 100 columns.
- Region math with shallow terminal heights.
- Plan panel queue rendering with real content prefixes.
- Focus panel selection priority.
- Mouse wheel events route to timeline scrolling.
- Ctrl-U review behavior after the layout refactor.
- Input cursor placement after completion, paste placeholder, and multiline
  input rendering.

## Implementation Slices

### Slice 1: Responsive Cockpit Skeleton

Add layout calculation and panel view models. Render wide, medium, and narrow
layouts using existing timeline/input rendering where possible.

Deliverables:

- `CockpitLayoutMode` and tested region calculation.
- Wide three-pane layout.
- Medium Chat plus stacked work rail.
- Narrow fallback to existing single-column behavior.
- Plan pane with run state, current task, queue lanes, usage, and recent tools.
- Focus pane with latest artifact summary.

### Slice 2: Focus Content Quality

Improve Focus renderers for real high-signal artifacts.

Deliverables:

- Patch diff renderer reused in Focus with line numbers and clipped height.
- Command output renderer with `Ran <command>` style and indented stdout/stderr.
- File/MCP result previews with stable titles.
- Compact Chat entries when Focus contains the detailed view.

### Slice 3: Pane Interaction

Add interaction only after the passive layout feels correct.

Deliverables:

- Configurable pane focus shortcuts.
- Focus pane scroll when an artifact exceeds available height.
- Optional pin current Focus artifact.
- Internal copy/review mode that does not depend on terminal native selection.

## Completion Definition

This spec is complete when Merry can run in a wide terminal and visibly differs
from Codex by presenting a three-surface cockpit: Chat for conversation, Focus
for current work, and Plan for task/queue/status. The redesign must preserve the
existing terminal workflow rather than trading usability for visual novelty.
