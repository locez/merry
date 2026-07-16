# Slash Commands And Shared Input History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete two sub-items of ROADMAP "TUI Daily Coding Flow #1": a slash command registry that also serves slash completion, and workspace-scoped persisted plain-text input history that survives resume and is shared across new/resumed sessions in the same workspace.

**Architecture:** Slash commands are a static registry mapping each command name to an effect (`ControllerEffect` reuse or a timeline display), never a new execution subsystem. Slash completion plugs into the existing `CompletionSources` so the existing menu, navigation, and Tab-accept logic is reused. Input history stays workspace-scoped (keyed by a hash of the workspace root), persisted as JSONL under XDG state, and loaded into the existing in-memory `InputHistory` at startup. No runtime/core protocol changes; all work is in `merry-cli` TUI layer.

**Tech Stack:** Rust 2024, Crossterm 0.29, Tokio, XDG state dir, serde_json (JSONL), existing `TuiPreferencesStore` atomic write pattern.

---

### Task 1: Slash Command Registry

**Files:**
- Create: `crates/merry-cli/src/tui/slash_command.rs`
- Modify: `crates/merry-cli/src/tui/mod.rs`

- [ ] **Step 1: Write failing registry tests**

Assert `SLASH_COMMANDS` is non-empty, command names are unique, names carry no
leading `/`, `find_exact("save")` returns the save spec, `find_prefix("sa")`
returns only save, `find_prefix("")` returns all commands, and `find_exact`
returns `None` for unknown names.

- [ ] **Step 2: Add the registry module**

Define an immutable `SlashCommandSpec { name, description, effect }` and a
`SlashCommandEffect` enum with two variants: `ControllerEffect(&'static str)`
(for save/stop) and `DisplayInTimeline` (for help/status). Provide
`find_exact(name: &str) -> Option<&'static SlashCommandSpec>` and
`find_prefix(query: &str) -> Vec<&'static SlashCommandSpec>`. Seed four commands:
`help`, `status`, `save`, `stop`.

- [ ] **Step 3: Register the module**

Add `mod slash_command;` to `crates/merry-cli/src/tui/mod.rs` and re-export the
query functions needed by completion and controller.

- [ ] **Step 4: Run registry tests**

```bash
cargo test -p merry-cli --bin merry -- tui::slash_command -- --nocapture
```

Expected: PASS.

### Task 2: Slash Completion Source

**Files:**
- Modify: `crates/merry-cli/src/tui/completion.rs`
- Test: `crates/merry-cli/src/tui/tests.rs`

- [ ] **Step 1: Write failing completion tests**

Cover: typing `/sa` yields a menu containing `/save`; typing `/` yields all four
commands; typing `hello` (no leading slash) yields no slash items; typing
`/xyz` yields an empty slash menu; existing path/skill completion still works
for non-slash input.

- [ ] **Step 2: Extend CompletionSources with a slash source**

In `menu_for_input`, when the text starts with `/` and the cursor is on the
first line, parse the command prefix after `/` and call
`slash_command::find_prefix`. Map each result to a `CompletionItem` that renders
with the leading `/`. Reuse the existing fuzzy/refresh path so navigation and
Tab accept work unchanged.

- [ ] **Step 3: Run completion tests**

```bash
cargo test -p merry-cli --bin merry -- tui::tests::completion -- --nocapture
cargo test -p merry-cli --bin merry -- tui::tests::slash -- --nocapture
```

Expected: PASS.

### Task 3: Slash Command Dispatch

**Files:**
- Modify: `crates/merry-cli/src/tui/controller.rs`
- Test: `crates/merry-cli/src/tui/tests.rs`

- [ ] **Step 1: Write failing dispatch tests**

Cover: submitting `/save` produces `ControllerEffect::SaveSession` and does not
call `submit_next_message`; `/stop` produces `Interrupt`; `/help` pushes a
timeline item without submitting; `/status` pushes a status summary; `/unknown`
falls through to a normal `SubmitNext`; after any slash command the input box is
cleared.

- [ ] **Step 2: Intercept slash commands before submission**

In the `SubmitNext(submission)` handling path, before calling
`session.input.submit_next_message`, check whether `submission.text` exactly
matches a registered slash command name (with leading `/`). If it matches:
- for `ControllerEffect` variants, return the mapped effect and skip submission;
- for `DisplayInTimeline`, push the rendered text to `TuiState.timeline` and
  return `None`;
- clear the input box in both cases.

Only exact matches are intercepted; `save` without `/` is not intercepted.

- [ ] **Step 3: Render help and status output**

`/help` lists all slash commands plus a compact summary of keymap actions.
`/status` assembles model label, usage, plan phase, run state, and workspace
from existing `TuiState` fields; no new runtime queries.

- [ ] **Step 4: Run dispatch tests**

```bash
cargo test -p merry-cli --bin merry -- tui::tests::slash_command -- --nocapture
```

Expected: PASS.

### Task 4: Workspace-Scoped Input History Store

**Files:**
- Create: `crates/merry-cli/src/tui/input_history_store.rs`
- Modify: `crates/merry-cli/src/tui/mod.rs`

- [ ] **Step 1: Write failing store tests**

Cover: round-trip of three entries; empty file loads as empty Vec; a corrupted
line is skipped while remaining lines load; a fully corrupted file loads as
empty Vec without panic; two different workspace roots map to different paths
and do not cross-contaminate; save is atomic (temp file plus rename).

- [ ] **Step 2: Add the store module**

Define `InputHistoryStore { path: PathBuf }` with:
- `for_workspace(state_dir, workspace_root) -> Self`, where the file name is a
  hex hash of the workspace root (sha2 first 16 bytes); never store the raw
  path;
- `load() -> Vec<String>` reading JSONL, skipping unparseable lines, returning
  empty on missing/corrupt with `tracing::warn!`;
- `save(&[String]) -> Result<(), InputHistoryError>` using the atomic temp +
  rename pattern from `TuiPreferencesStore`.

Path layout: `<state_dir>/merry/input-history/<hash>.jsonl`.

- [ ] **Step 3: Register and run store tests**

```bash
cargo test -p merry-cli --bin merry -- tui::input_history_store -- --nocapture
```

Expected: PASS.

### Task 5: Wire History Into TuiState And Lifecycle

**Files:**
- Modify: `crates/merry-cli/src/tui/state.rs`
- Modify: `crates/merry-cli/src/tui/input.rs`
- Modify: `crates/merry-cli/src/tui/controller.rs`
- Modify: `crates/merry-cli/src/tui/mod.rs`

- [ ] **Step 1: Add a save effect and store field**

Add `ControllerEffect::SaveInputHistory` and an `input_history_store:
Option<InputHistoryStore>` field on `TuiState`. Keep the existing in-memory
`InputHistory` navigation logic unchanged.

- [ ] **Step 2: Load history at startup**

In `tui::run`, after constructing `TuiState`, build the store for the current
workspace and load entries, then call `state.set_input_history(entries)`. Both
new and resume paths load, because history is workspace-scoped.

- [ ] **Step 3: Save only on successful submission**

After a successful `SubmitNext` or `SubmitBacklog` in `dispatch_effect`, if the
store is present, emit `SaveInputHistory` and persist `history.entries()`. Never
save on keystroke or on slash command dispatch. Failures log a warning and do
not block the user.

- [ ] **Step 4: Expose entries for persistence**

Add `InputHistory::entries() -> &[String]` so the controller can read the
current history without disturbing navigation state.

- [ ] **Step 5: Run lifecycle tests**

```bash
cargo test -p merry-cli --bin merry -- tui::tests::input_history -- --nocapture
```

Expected: existing `previous`/`next` tests still pass; new persistence tests
pass.

### Task 6: Full Verification

**Files:**
- Verify all touched files

- [ ] **Step 1: Run targeted checks**

```bash
cargo fmt --all --check
cargo test -p merry-cli --bin merry -- tui::slash_command tui::input_history_store -- --nocapture
cargo test -p merry-cli --bin merry -- tui::tests -- --nocapture
```

Expected: PASS.

- [ ] **Step 2: Run the full repository checks**

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
git diff --check
```

Expected: zero new failures beyond the three known pre-existing baseline
failures (`subagent_with_narrow_tools_keeps_read_only_profile`, and the two
`debug_shell` tests that require `rg`).

- [ ] **Step 3: Manual interaction check**

At 120x24 and 80x16: type `/`, confirm the menu lists four commands, accept a
completion, run `/save` and `/stop`, confirm the input clears, and confirm the
history file exists under the XDG state dir without leaking the raw workspace
path.
