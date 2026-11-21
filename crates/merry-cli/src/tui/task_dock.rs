//! Always-visible, bounded Background Tasks & Subagents dock state and
//! interactive background-process output inspector overlay.
//!
//! Runtime owns background processes and subagent lifecycles; this module only
//! projects live snapshots into a compact 1-row summary bar that expands into
//! a bounded, scrollable drawer (scaling cleanly to 30+ concurrent tasks).

use crate::tui::subagent_view::{SubagentEntry, SubagentPhase};
use crossterm::event::{KeyCode, KeyEvent};
use merry_runtime::{BackgroundProcessOutput, BackgroundProcessState, BackgroundProcessSummary};

/// Maximum visible list rows inside the expanded inline dock drawer so 30+
/// background tasks or subagents never take over the terminal viewport.
pub(crate) const MAX_DOCK_VISIBLE_ROWS: usize = 5;

/// Active filter tab for the expanded task dock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TaskDockFilter {
    #[default]
    All,
    Running,
    Background,
    Subagents,
}

/// Unified dock row representing either a background process or a subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskDockEntry {
    Background(BackgroundProcessSummary),
    Subagent {
        agent_id: String,
        display_name: Option<String>,
        task_id: String,
        phase: SubagentPhase,
        summary: String,
    },
}

impl TaskDockEntry {
    pub(crate) fn is_running(&self) -> bool {
        match self {
            Self::Background(summary) => {
                matches!(summary.state(), BackgroundProcessState::Running { .. })
            }
            Self::Subagent { phase, .. } => phase.is_active(),
        }
    }

    pub(crate) fn is_background(&self) -> bool {
        matches!(self, Self::Background(_))
    }

    pub(crate) fn is_subagent(&self) -> bool {
        matches!(self, Self::Subagent { .. })
    }
}

/// Counts shown in the 1-row dock summary and filter chips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TaskDockCounts {
    pub(crate) total: usize,
    pub(crate) running: usize,
    pub(crate) background_total: usize,
    pub(crate) background_running: usize,
    pub(crate) subagents_total: usize,
    pub(crate) subagents_running: usize,
}

/// Persistent dock state stored on [`crate::tui::state::TuiState`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TaskDockState {
    background_processes: Vec<BackgroundProcessSummary>,
    expanded: bool,
    filter: TaskDockFilter,
    scroll: usize,
}

impl TaskDockState {
    pub(crate) fn set_background_processes(
        &mut self,
        processes: Vec<BackgroundProcessSummary>,
    ) -> bool {
        // Exited and killed one-shot processes are not resumable and should not
        // linger in the top Tasks dock; runtime still retains them for read_background_process.
        let running: Vec<BackgroundProcessSummary> = processes
            .into_iter()
            .filter(|process| matches!(process.state(), BackgroundProcessState::Running { .. }))
            .collect();
        if self.background_processes == running {
            return false;
        }
        self.background_processes = running;
        if self.background_processes.is_empty() {
            self.scroll = 0;
        }
        true
    }

    pub(crate) fn has_running_background_processes(&self) -> bool {
        self.background_processes
            .iter()
            .any(|process| matches!(process.state(), BackgroundProcessState::Running { .. }))
    }

    pub(crate) fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub(crate) fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
        if !self.expanded {
            self.scroll = 0;
        }
    }

    pub(crate) fn filter(&self) -> TaskDockFilter {
        self.filter
    }

    pub(crate) fn set_filter(&mut self, filter: TaskDockFilter) {
        if self.filter != filter {
            self.filter = filter;
            self.scroll = 0;
        }
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn scroll_by(&mut self, delta: isize, total_items: usize, visible_rows: usize) {
        let max_scroll = total_items.saturating_sub(visible_rows);
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs());
        } else {
            self.scroll = self
                .scroll
                .saturating_add(delta.unsigned_abs())
                .min(max_scroll);
        }
    }

    pub(crate) fn counts(&self, subagents: &[SubagentEntry]) -> TaskDockCounts {
        let background_total = self.background_processes.len();
        let background_running = self
            .background_processes
            .iter()
            .filter(|process| matches!(process.state(), BackgroundProcessState::Running { .. }))
            .count();
        let subagents_total = subagents.iter().filter(|entry| !entry.closed).count();
        let subagents_running = subagents
            .iter()
            .filter(|entry| !entry.closed && entry.phase.is_active())
            .count();
        TaskDockCounts {
            total: background_total + subagents_total,
            running: background_running + subagents_running,
            background_total,
            background_running,
            subagents_total,
            subagents_running,
        }
    }

    /// Returns filtered entries with active/running tasks prioritized first.
    pub(crate) fn entries(&self, subagents: &[SubagentEntry]) -> Vec<TaskDockEntry> {
        let mut all = Vec::with_capacity(self.background_processes.len() + subagents.len());
        for process in &self.background_processes {
            all.push(TaskDockEntry::Background(process.clone()));
        }
        for subagent in subagents.iter().filter(|entry| !entry.closed) {
            all.push(TaskDockEntry::Subagent {
                agent_id: subagent.agent_id.clone(),
                display_name: subagent.display_name.clone(),
                task_id: subagent.task_id.clone(),
                phase: subagent.phase,
                summary: subagent.summary.clone(),
            });
        }
        all.retain(|entry| match self.filter {
            TaskDockFilter::All => true,
            TaskDockFilter::Running => entry.is_running(),
            TaskDockFilter::Background => entry.is_background(),
            TaskDockFilter::Subagents => entry.is_subagent(),
        });
        // Active/running items always bubble to the top so users immediately
        // see live work even among 30+ historical tasks.
        all.sort_by_key(|entry| (!entry.is_running(), entry.is_subagent()));
        all
    }
}

/// Interactive overlay inspecting live captured stdout/stderr of one background process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundTaskOverlay {
    process_id: String,
    output: Option<BackgroundProcessOutput>,
    error: Option<String>,
    scroll: usize,
    follow_tail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackgroundTaskOverlayAction {
    Consumed,
    Close,
    Refresh(String),
    Stop(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundTaskPointer {
    Close,
    Refresh,
    Stop,
}

impl BackgroundTaskOverlay {
    pub(crate) fn loading(process_id: String) -> Self {
        Self {
            process_id,
            output: None,
            error: None,
            scroll: 0,
            follow_tail: true,
        }
    }

    pub(crate) fn process_id(&self) -> &str {
        &self.process_id
    }

    pub(crate) fn output(&self) -> Option<&BackgroundProcessOutput> {
        self.output.as_ref()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.output.as_ref().is_some_and(|out| {
            matches!(
                out.summary().state(),
                BackgroundProcessState::Running { .. }
            )
        })
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn follow_tail(&self) -> bool {
        self.follow_tail
    }

    pub(crate) fn set_output(&mut self, output: BackgroundProcessOutput) {
        self.output = Some(output);
        self.error = None;
    }

    pub(crate) fn set_error(&mut self, message: String) {
        self.error = Some(message);
    }

    pub(crate) fn scroll_up(&mut self, lines: usize) {
        self.follow_tail = false;
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub(crate) fn scroll_down(&mut self, lines: usize, max_scroll: usize) {
        self.scroll = self.scroll.saturating_add(lines).min(max_scroll);
        if self.scroll >= max_scroll {
            self.follow_tail = true;
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> BackgroundTaskOverlayAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => BackgroundTaskOverlayAction::Close,
            KeyCode::Char('r') | KeyCode::F(5) => {
                BackgroundTaskOverlayAction::Refresh(self.process_id.clone())
            }
            KeyCode::Char('x') | KeyCode::Char('k') if self.is_running() => {
                BackgroundTaskOverlayAction::Stop(self.process_id.clone())
            }
            KeyCode::Up => {
                self.scroll_up(1);
                BackgroundTaskOverlayAction::Consumed
            }
            KeyCode::PageUp => {
                self.scroll_up(10);
                BackgroundTaskOverlayAction::Consumed
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
                self.follow_tail = false;
                BackgroundTaskOverlayAction::Consumed
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
                self.follow_tail = false;
                BackgroundTaskOverlayAction::Consumed
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.follow_tail = true;
                BackgroundTaskOverlayAction::Consumed
            }
            KeyCode::Home => {
                self.follow_tail = false;
                self.scroll = 0;
                BackgroundTaskOverlayAction::Consumed
            }
            _ => BackgroundTaskOverlayAction::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_runtime::BackgroundProcessId;

    fn bg(id: &str, running: bool) -> BackgroundProcessSummary {
        BackgroundProcessSummary::new(
            BackgroundProcessId::new(id).unwrap(),
            format!("cmd {id}"),
            Some(".".to_owned()),
            if running {
                BackgroundProcessState::Running { pid: 1000 }
            } else {
                BackgroundProcessState::Exited { code: 0 }
            },
        )
    }

    #[test]
    fn dock_hides_exited_background_processes_and_keeps_unclosed_subagents() {
        let mut dock = TaskDockState::default();
        let processes: Vec<_> = (0..10).map(|i| bg(&format!("bg-{i}"), i < 4)).collect();
        assert!(dock.set_background_processes(processes));

        let mut subagents = vec![
            SubagentEntry::new(
                "agent-run".to_owned(),
                "task-1".to_owned(),
                SubagentPhase::Running,
                "working".to_owned(),
            ),
            SubagentEntry::new(
                "agent-done".to_owned(),
                "task-2".to_owned(),
                SubagentPhase::Completed,
                "finished".to_owned(),
            ),
        ];

        // Only the 4 running background processes + 2 open subagents appear.
        let counts = dock.counts(&subagents);
        assert_eq!(counts.background_total, 4);
        assert_eq!(counts.background_running, 4);
        assert_eq!(counts.subagents_total, 2);
        assert_eq!(counts.total, 6);
        assert_eq!(counts.running, 5);

        let entries = dock.entries(&subagents);
        assert_eq!(entries.len(), 6);
        assert!(entries[..5].iter().all(TaskDockEntry::is_running));
        assert!(!entries[5].is_running());

        // Closing the completed subagent removes it from the top dock.
        subagents[1].closed = true;
        assert_eq!(dock.counts(&subagents).total, 5);
        assert_eq!(dock.entries(&subagents).len(), 5);

        // Exited/killed processes drop out completely, collapsing the dock when empty.
        assert!(dock.set_background_processes(vec![bg("bg-dead", false)]));
        assert_eq!(dock.counts(&[]).total, 0);

        // Scroll stays bounded for 30+ running processes.
        let many: Vec<_> = (0..30).map(|i| bg(&format!("bg-{i}"), true)).collect();
        assert!(dock.set_background_processes(many));
        dock.scroll_by(100, 30, MAX_DOCK_VISIBLE_ROWS);
        assert_eq!(dock.scroll(), 25);
        dock.scroll_by(-10, 30, MAX_DOCK_VISIBLE_ROWS);
        assert_eq!(dock.scroll(), 15);
    }

    #[test]
    fn task_dock_renders_below_user_box_and_scales_to_thirty_tasks_with_scroll_and_inspect() {
        use crate::tui::{
            controller::{
                ControllerEffect, handle_mouse_input, handle_mouse_scroll_down_by,
                handle_mouse_scroll_up_by,
            },
            keymap::Keymap,
            overlay::Overlay,
            render::{render_to_buffer, render_to_text},
            state::{SubagentUpdate, TimelineItem, TuiState},
            subagent_view::SubagentPhase,
            text_interaction::MouseInput,
            theme::TuiTheme,
        };
        use ratatui::layout::{Position, Size};

        fn find_text_position(
            buffer: &ratatui::buffer::Buffer,
            needle: &str,
        ) -> Option<(u16, u16)> {
            let area = buffer.area;
            for y in area.y..area.y + area.height {
                let mut row = String::new();
                let mut cols = Vec::new();
                for x in area.x..area.x + area.width {
                    let sym = buffer[(x, y)].symbol();
                    let start = row.len();
                    row.push_str(sym);
                    cols.push((start, x));
                }
                if let Some(byte_idx) = row.find(needle)
                    && let Some(&(_, col)) = cols.iter().find(|&&(start, _)| start == byte_idx)
                {
                    return Some((col, y));
                }
            }
            None
        }

        let mut state = TuiState::new(
            "/repo".into(),
            "gpt-test".to_owned(),
            Keymap::default(),
            TuiTheme::default(),
        );
        state.push_timeline_item(TimelineItem::User {
            text: "USER_ANCHOR".to_owned(),
        });
        let reply: String = (0..40)
            .map(|i| format!("reply-line-{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.push_timeline_item(TimelineItem::Assistant { text: reply });

        let size = Size::new(110, 28);
        let empty = render_to_text(&state, size.width, size.height);
        assert!(!empty.contains("[Tasks"), "{empty}");

        // Populate 5 running + 10 exited background processes (only 5 running appear)
        // plus 15 subagents (5 running + 10 completed) = 20 dock tasks.
        let processes: Vec<_> = (0..15)
            .map(|i| {
                let summary = bg(&format!("bg-{i:02}"), i < 5);
                if i == 0 {
                    summary.with_display_name(Some("api server".to_owned()))
                } else {
                    summary
                }
            })
            .collect();
        state.sync_background_processes(processes);
        for i in 0..15 {
            state.upsert_subagent(SubagentUpdate {
                agent_id: format!("agent-{i:02}"),
                task_id: format!("task-{i:02}"),
                display_name: None,
                phase: if i < 5 {
                    SubagentPhase::Running
                } else {
                    SubagentPhase::Completed
                },
                summary: format!("subagent summary {i:02}"),
                changed_paths: None,
                output_paths: None,
                diagnostic: None,
            });
        }

        let collapsed_buf = render_to_buffer(&state, size.width, size.height);
        let collapsed_text = render_to_text(&state, size.width, size.height);
        assert!(
            collapsed_text.contains("10 running (5 bg · 5 agents)"),
            "{collapsed_text}"
        );
        assert!(
            collapsed_text.contains("api server: cmd bg-00"),
            "{collapsed_text}"
        );
        assert!(collapsed_text.contains("[Tasks 20 ▸]"), "{collapsed_text}");
        let (_, user_row) = find_text_position(&collapsed_buf, "USER_ANCHOR").expect("user");
        let (dock_x, dock_y) = find_text_position(&collapsed_buf, "10 running").expect("dock");
        assert!(
            dock_y > user_row,
            "dock ({dock_y}) should sit below user box ({user_row})"
        );
        // Timeline body remains visible underneath the 1-row collapsed dock.
        assert!(collapsed_text.contains("agent-14"), "{collapsed_text}");

        // Click collapsed dock to expand bounded 5-row drawer.
        assert_eq!(
            handle_mouse_input(
                MouseInput::Down(Position::new(dock_x, dock_y)),
                size,
                &mut state
            ),
            ControllerEffect::None
        );
        assert!(state.task_dock().is_expanded());
        let expanded_buf = render_to_buffer(&state, size.width, size.height);
        let expanded_text = render_to_text(&state, size.width, size.height);
        assert!(expanded_text.contains("1–5/20"), "{expanded_text}");
        assert!(expanded_text.contains("api server"), "{expanded_text}");
        assert!(expanded_text.contains("bg-04"), "{expanded_text}");
        assert!(
            !expanded_text.contains("bg-05"),
            "exited processes must not appear in the top Tasks dock:\n{expanded_text}"
        );

        // Scroll inside expanded drawer scrolls tasks without moving timeline.
        let (bg0_x, bg0_y) = find_text_position(&expanded_buf, "api server").expect("api server");
        assert_eq!(
            handle_mouse_scroll_down_by(Position::new(bg0_x, bg0_y), size, 1, &mut state),
            ControllerEffect::None
        );
        assert_eq!(state.task_dock().scroll(), 1);
        assert_eq!(
            handle_mouse_scroll_up_by(Position::new(bg0_x, bg0_y), size, 1, &mut state),
            ControllerEffect::None
        );
        assert_eq!(state.task_dock().scroll(), 0);

        // Clicking a background process row opens the live output inspector overlay.
        assert_eq!(
            handle_mouse_input(
                MouseInput::Down(Position::new(bg0_x, bg0_y)),
                size,
                &mut state
            ),
            ControllerEffect::InspectBackgroundProcess("bg-00".to_owned())
        );
        assert!(matches!(
            state.overlay(),
            Some(Overlay::BackgroundTaskInspect(_))
        ));
    }
}
