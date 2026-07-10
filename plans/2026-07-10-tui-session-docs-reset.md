# TUI, Session, And Documentation Reset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the redundant three-panel cockpit with a modern magenta-led timeline UI, complete resume-safe session behavior, make sandbox startup automatic, and reset user documentation.

**Architecture:** TUI state separates input, timeline review, and detail modes. Layout always reserves one timeline plus input/status, adding detail only when explicitly open. Existing uncommitted session picker/resume work is preserved; runtime construction attaches the session store so stable boundaries save automatically.

**Tech Stack:** Rust 2024, Ratatui 0.30, Crossterm 0.29, Tokio, XDG session store, Clap.

---

### Task 1: Protect Existing Resume Work With Focused Tests

**Files:**
- Modify: `crates/merry-cli/src/tui/tests.rs`
- Modify: `crates/merry-cli/src/tui/session_list.rs`
- Modify: `crates/merry-cli/src/tui/session_picker.rs`
- Modify: `crates/merry-runtime/src/runtime/tests/session_resume.rs`

- [ ] **Step 1: Inventory the current dirty diff**

Before editing, review only the existing session/resume changes and record which
functions are user-authored. Do not restore or replace those files wholesale.

```bash
git diff -- crates/merry-cli/src/tui crates/merry-runtime/src/runtime/builder.rs crates/merry-runtime/src/runtime/session_access.rs crates/merry-runtime/src/session_projection.rs
```

- [ ] **Step 2: Add resume projection tests**

Cover session list ordering, malformed entry isolation, selected-session
metadata, restored transcript ordering, missing optional reasoning effort, and
picker behavior at 50x20 and 80x24.

- [ ] **Step 3: Run the focused baseline**

```bash
cargo test -p merry-cli tui::session -- --nocapture
cargo test -p merry-runtime session_resume -- --nocapture
```

Expected: existing resume behavior remains green before layout changes.

### Task 2: Replace Cockpit Layout With Timeline And Detail Layout

**Files:**
- Rewrite: `crates/merry-cli/src/tui/layout.rs`
- Remove runtime use of: `crates/merry-cli/src/tui/panels.rs`
- Modify: `crates/merry-cli/src/tui/mod.rs`
- Test: layout module tests

- [ ] **Step 1: Write failing viewport layout tests**

Define expected rectangles for 50x20, 80x24, and 140x40 with detail closed and
open. Assert one header row, timeline, optional status row, bounded input, and
one footer/status row never overlap. Detail is right-side only at width >= 120;
otherwise it occupies the timeline content area.

- [ ] **Step 2: Add the new layout types**

Replace `CockpitLayoutMode` and `CockpitRects` with:

```rust
pub enum TimelineLayoutMode { Narrow, Standard, Wide }

pub struct TimelineRects {
    pub header: Rect,
    pub timeline: Rect,
    pub detail: Option<Rect>,
    pub input: Rect,
    pub status: Rect,
}
```

Inputs include `detail_open`, dynamic input height, and whether a completion
menu is visible. Empty queues do not receive a rectangle.

- [ ] **Step 3: Stop building permanent focus/plan views**

Remove renderer/controller dependencies on `focus_panel_view` and
`plan_panel_view`. Reuse their source/diff/command formatting helpers in a new
on-demand detail module in Task 4; delete `panels.rs` after all callers move.

- [ ] **Step 4: Run layout tests**

```bash
cargo test -p merry-cli tui::layout -- --nocapture
```

Expected: PASS.

### Task 3: Introduce Explicit Interaction Modes

**Files:**
- Modify: `crates/merry-cli/src/tui/state.rs`
- Modify: `crates/merry-cli/src/tui/input.rs`
- Modify: `crates/merry-cli/src/tui/keymap.rs`
- Modify: `crates/merry-cli/src/tui/controller.rs`
- Test: focused state/controller tests split from `tui/tests.rs`

- [ ] **Step 1: Write transition-table tests**

Cover idle input, active generation, timeline review, detail open, completion
menu, history navigation, interrupt, and queue actions. Assert `Esc` priority is
interrupt active run, close detail, leave review, then clear input. Assert Up
and Down edit history only in input mode.

- [ ] **Step 2: Add explicit mode state**

```rust
pub enum InteractionMode {
    Input,
    TimelineReview { selected: usize },
    Detail { timeline_index: usize },
}
```

Keep follow-tail as state derived from mode and scroll position. Remove
separate artifact-follow/review booleans that can contradict each other.

- [ ] **Step 3: Unify keyboard and mouse behavior**

`PageUp` enters review, `PageDown` approaches live tail, `Enter` opens a
selectable detail in review, and mouse wheel affects the surface under the
pointer. Existing configurable next/backlog and suspended actions remain.
Delete artifact-only previous/next/follow commands from the default keymap and
config example.

- [ ] **Step 4: Split controller tests by responsibility**

Move new transition tests into `crates/merry-cli/src/tui/controller/tests.rs`
or an equivalent focused sibling module. Do not add more test cases to the
already oversized `tui/tests.rs` block.

- [ ] **Step 5: Run interaction tests**

```bash
cargo test -p merry-cli tui::controller -- --nocapture
```

Expected: PASS.

### Task 4: Build Timeline And On-Demand Detail Rendering

**Files:**
- Create: `crates/merry-cli/src/tui/render/header.rs`
- Create: `crates/merry-cli/src/tui/render/timeline.rs`
- Create: `crates/merry-cli/src/tui/render/detail.rs`
- Create: `crates/merry-cli/src/tui/render/input.rs`
- Create: `crates/merry-cli/src/tui/render/status.rs`
- Rewrite: `crates/merry-cli/src/tui/render.rs` as a small router
- Modify: `crates/merry-cli/src/tui/projector.rs`
- Modify: `crates/merry-cli/src/tui/state.rs`

- [ ] **Step 1: Write renderer acceptance tests first**

At each viewport assert no `FOCUS`, `RUN`, empty queue title, duplicate current
task, duplicate status, or overlap marker appears. Cover long unbroken paths,
long model/provider labels, live assistant growth, two simultaneous tools,
failed tools, patch, permission, queues, and detail open/closed.

- [ ] **Step 2: Keep one chronological timeline**

Render user, assistant, tool start/result, patch, permission, diagnostic, and
artifact items in event order. Routine successful tool output collapses to one
row; failure, permission, and patch retain relevant expanded content. Preserve
the existing delta append/replace behavior in `TuiProjector`.

- [ ] **Step 3: Build on-demand detail**

Move source, directory, command, patch, JSON, and text detail formatting out of
`panels.rs`. Detail takes one selected timeline item and renders it in the
layout-provided area; absent selection means no area or empty-detail frame.

- [ ] **Step 4: Add compact header and one status line**

Header includes truncated project, provider/model, session, and run state.
Status includes the current phase, elapsed time when active, usage, non-empty
queue counts, and the highest-priority error. Remove separate interaction and
plan status lines.

- [ ] **Step 5: Run viewport tests**

```bash
cargo test -p merry-cli tui::render -- --nocapture
```

Expected: PASS for 50x20, 80x24, and 140x40.

### Task 5: Refine The Modern Magenta Theme

**Files:**
- Modify: `crates/merry-cli/src/tui/theme.rs`
- Modify: `examples/config.toml`
- Test: theme parsing tests

- [ ] **Step 1: Add semantic role tests**

Assert distinct defaults for brand/focus/live generation, tool/info, success,
warning/permission, failure, primary text, and muted metadata. Assert custom
theme TOML still validates.

- [ ] **Step 2: Apply the balanced palette**

Use magenta/light-magenta for Merry identity, focus, selection, and live
generation; charcoal/black and soft white for the surface; cyan for tools and
information; green for success; yellow/amber for warnings and permission; red
only for errors; gray for metadata. Avoid filling panel borders and routine
text with magenta.

- [ ] **Step 3: Run theme tests**

```bash
cargo test -p merry-cli tui::theme -- --nocapture
```

Expected: PASS.

### Task 6: Make Rendering Event-Driven

**Files:**
- Modify: `crates/merry-cli/src/tui/controller.rs`
- Modify: `crates/merry-cli/src/tui/state.rs`
- Test: controller timing tests using paused Tokio time

- [ ] **Step 1: Write paused-time redraw tests**

Assert idle operation has no periodic redraw, a terminal/runtime event redraws
immediately, and active elapsed/spinner state schedules no faster than 100 ms.

- [ ] **Step 2: Remove the unconditional 33 ms interval**

Select directly on terminal and runtime events. Add an optional 100 ms sleep
branch only while `state.needs_animation_tick()` is true. Use one simple
spinner sequence; remove custom character-motion rendering.

- [ ] **Step 3: Run timing tests**

```bash
cargo test -p merry-cli tui::controller redraw -- --nocapture
```

Expected: PASS without wall-clock sleeps.

### Task 7: Complete Resume-Safe Session Lifecycle

**Files:**
- Modify: `crates/merry-cli/src/coding_runtime/builder.rs`
- Modify: `crates/merry-cli/src/tui/runtime.rs`
- Modify: `crates/merry-runtime/src/runtime/builder.rs`
- Modify: `crates/merry-runtime/src/runtime/session_access.rs`
- Modify: `crates/merry-runtime/src/runtime/tests/session_resume.rs`
- Modify: `crates/merry-cli/src/tui/session_list.rs`

- [ ] **Step 1: Add savepoint lifecycle tests**

Cover new session construction, resume construction, completed model turn,
resolved tool batch, interrupted tool cleanup, clean exit, and simulated TUI
receiver closure. Assert no save occurs while a batch is pending and the latest
safe state survives reopening.

- [ ] **Step 2: Attach stores during construction**

Use the runtime builder's store-aware new/resume paths instead of loading then
adding explicit exit-only saves. Keep exit save as an idempotent final flush.

- [ ] **Step 3: Refresh picker metadata after safe saves**

Metadata derives from stored session projection and optional provider/model
labels. Missing or malformed metadata degrades one entry, not the entire
picker.

- [ ] **Step 4: Run lifecycle tests**

```bash
cargo test -p merry-runtime session_resume -- --nocapture
cargo test -p merry-cli tui::session -- --nocapture
```

Expected: PASS.

### Task 8: Make Sandbox Startup Automatic

**Files:**
- Modify: `crates/merry-cli/src/cli.rs`
- Modify: `crates/merry-cli/src/cli_route.rs`
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/src/sandbox.rs`
- Modify: `crates/merry-cli/src/sandbox/tests.rs`
- Modify: `crates/merry-cli/tests/tui_cli.rs`

- [ ] **Step 1: Write route/bootstrap tests**

Assert default TUI, `run`, and `resume` request outer sandbox handoff when not
already inside it; debug/config/help/version commands retain their appropriate
behavior; recursion is impossible; missing `bwrap` errors before raw mode.

- [ ] **Step 2: Make the handoff an internal decision**

Route product commands through sandbox planning automatically. Keep an internal
child marker for recursion avoidance. Hide or deprecate `--with-sandbox` from
normal help while accepting it during the 0.1 transition.

- [ ] **Step 3: Run CLI bootstrap tests**

```bash
cargo test -p merry-cli sandbox -- --nocapture
cargo test -p merry-cli --test tui_cli -- --nocapture
```

Expected: PASS.

### Task 9: Rewrite README And ROADMAP From Scratch

**Files:**
- Rewrite: `README.md`
- Rewrite: `ROADMAP.md`
- Modify: `examples/config.toml` if commands/config changed during TUI work

- [ ] **Step 1: Replace README content**

Write a concise user guide containing project purpose, prerequisites, build,
OpenAI Chat config, Anthropic config, `merry`/`run`/`resume`, Python usage,
provider matrix, sandbox/session behavior, verification, and current limits.
Do not copy the old implementation-history narrative.

- [ ] **Step 2: Replace ROADMAP content**

Record the current goal and only these controlled milestones: real streaming,
multi-tool batches, providers/config/SDK, TUI/session, and release hardening.
For each include observable acceptance commands and evidence status. Record the
2026-07-10 user instruction authorizing the priority reset.

- [ ] **Step 3: Validate tracked examples and links**

```bash
cargo test -p merry-cli example_config -- --nocapture
rg -n "openai-compatible|Responses API only|parallel_tool_calls: false|--with-sandbox" README.md ROADMAP.md examples/config.toml
```

Expected: config test passes; search finds only intentional compatibility or
migration references.

### Task 10: Add CI And Run Full Verification

**Files:**
- Create: `.github/workflows/ci.yml`
- Verify all touched files

- [ ] **Step 1: Add deterministic CI jobs**

Run Rust fmt, clippy, and all tests. Run Python tests after building/installing
the local Maturin package in a `uv` environment. Do not require provider keys,
live endpoints, or `bwrap` smoke.

- [ ] **Step 2: Run the full local verification**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cd sdks/python && uv run --with pytest python -m pytest tests -q
git diff --check
```

Expected: all commands exit zero.

- [ ] **Step 3: Inspect final scope and file sizes**

```bash
git status --short
wc -l crates/merry-cli/src/tui/*.rs crates/merry-cli/src/tui/render/*.rs
```

Confirm pre-existing worktree changes were preserved and no focused production
file newly exceeds 1000 lines. If an existing oversized file remains, report
the ownership-based split completed or still required.
