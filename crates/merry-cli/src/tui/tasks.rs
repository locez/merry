//! Read-only `/tasks` text for retained processes and child agents.

use merry_runtime::{BackgroundProcessSummary, SubagentStatusView};

/// Renders the session's background processes and subagents.
#[must_use]
pub(crate) fn tasks_body(
    processes: &[BackgroundProcessSummary],
    subagents: Option<&[SubagentStatusView]>,
) -> String {
    let mut lines = Vec::new();
    lines.push("Background processes:".to_owned());
    if processes.is_empty() {
        lines.push("(none)".to_owned());
    } else {
        for process in processes {
            lines.push(format!(
                "{}  {}  {}",
                process.id(),
                process.state().label(),
                process.command()
            ));
        }
    }
    lines.push(String::new());
    lines.push("Subagents:".to_owned());
    match subagents {
        Some(subagents) if !subagents.is_empty() => {
            for subagent in subagents {
                let label = subagent
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|name| format!("{name} ({})", subagent.agent_id.as_str()))
                    .unwrap_or_else(|| subagent.agent_id.as_str().to_owned());
                lines.push(format!(
                    "{}  {}  {}  {}",
                    label,
                    subagent.task_id.as_str(),
                    subagent.status.as_str(),
                    subagent.summary
                ));
            }
        }
        _ => lines.push("(none)".to_owned()),
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::tasks_body;
    use merry_runtime::{
        BackgroundProcessId, BackgroundProcessState, BackgroundProcessSummary, SubagentStatusLabel,
        SubagentStatusView,
    };

    #[test]
    fn tasks_body_lists_processes_and_says_when_both_are_empty() {
        let empty = tasks_body(&[], None);
        assert!(empty.contains("Background processes:\n(none)"));
        assert!(empty.contains("Subagents:\n(none)"));

        let processes = [BackgroundProcessSummary::new(
            BackgroundProcessId::new("bg-1").expect("id"),
            "sleep 30",
            None,
            BackgroundProcessState::Running { pid: 7 },
        )];
        let body = tasks_body(&processes, Some(&[]));
        assert!(body.contains("bg-1  running pid 7  sleep 30"));
        assert!(body.contains("Subagents:\n(none)"));
    }

    #[test]
    fn tasks_body_lists_subagents() {
        let subagents = [SubagentStatusView::completed(
            merry_core::SubagentId::new("agent-1").expect("agent"),
            merry_core::SubagentTaskId::new("task-1").expect("task"),
            "done",
            Vec::new(),
            Vec::new(),
        )];
        assert_eq!(subagents[0].status, SubagentStatusLabel::Completed);
        let body = tasks_body(&[], Some(&subagents));
        assert!(body.contains("agent-1  task-1  completed  done"));
    }
}
