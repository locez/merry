use super::state::{PatchChangeView, TimelineItem, TuiState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusPanelView {
    pub(crate) title: String,
    pub(crate) body: FocusPanelBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FocusPanelBody {
    Empty,
    Patch { changes: Vec<PatchChangeView> },
    Source { path: String, content: String },
    DirectoryListing { entries: Vec<DirectoryEntryView> },
    CommandOutput { lines: Vec<String> },
    Text { lines: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectoryEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryEntryView {
    pub(crate) path: String,
    pub(crate) kind: DirectoryEntryKind,
}

pub(crate) fn focus_panel_view(state: &TuiState) -> FocusPanelView {
    if let Some(index) = state.selected_artifact_timeline_index()
        && let Some(item) = state.timeline().get(index)
    {
        return focus_view_for_item(item);
    }

    FocusPanelView {
        title: "FOCUS".to_owned(),
        body: FocusPanelBody::Empty,
    }
}

fn focus_view_for_item(item: &TimelineItem) -> FocusPanelView {
    match item {
        TimelineItem::Patch { changes } => {
            let path = changes
                .first()
                .map(|change| change.path.as_str())
                .unwrap_or("patch");
            FocusPanelView {
                title: format!("FOCUS patch {path}"),
                body: FocusPanelBody::Patch {
                    changes: changes.clone(),
                },
            }
        }
        TimelineItem::Expanded { title, body } | TimelineItem::Diagnostic { title, body } => {
            FocusPanelView {
                title: focus_title_for_text_item(title),
                body: focus_body_for_expanded_item(title, body),
            }
        }
        TimelineItem::ExpandedDetail {
            title, focus_body, ..
        } => FocusPanelView {
            title: focus_title_for_text_item(title),
            body: focus_body_for_expanded_item(title, focus_body),
        },
        TimelineItem::Muted { title, detail } if !detail.is_empty() => FocusPanelView {
            title: format!("FOCUS {}", title.to_lowercase()),
            body: FocusPanelBody::Text {
                lines: vec![format!("{title} {detail}")],
            },
        },
        TimelineItem::Muted { title, .. } => FocusPanelView {
            title: format!("FOCUS {}", title.to_lowercase()),
            body: FocusPanelBody::Text {
                lines: vec![title.clone()],
            },
        },
        TimelineItem::User { .. } | TimelineItem::Assistant { .. } => FocusPanelView {
            title: "FOCUS".to_owned(),
            body: FocusPanelBody::Empty,
        },
    }
}

fn focus_title_for_text_item(title: &str) -> String {
    if let Some(command) = title
        .strip_prefix("Ran ")
        .or_else(|| title.strip_prefix("Ran: "))
    {
        let command = command
            .split_once(" (cwd: ")
            .map_or(command, |(command, _)| command);
        return format!("FOCUS command {command}");
    }
    if let Some(detail) = title.strip_prefix("MCP ") {
        return format!("FOCUS MCP {detail}");
    }
    format!("FOCUS {title}")
}

fn focus_body_for_expanded_item(title: &str, body: &str) -> FocusPanelBody {
    if let Some(path) = title.strip_prefix("Read ") {
        return FocusPanelBody::Source {
            path: path.to_owned(),
            content: body.to_owned(),
        };
    }
    if title.strip_prefix("Listed ").is_some() {
        return FocusPanelBody::DirectoryListing {
            entries: body.lines().map(directory_entry_view).collect(),
        };
    }
    if title
        .strip_prefix("Ran ")
        .or_else(|| title.strip_prefix("Ran: "))
        .is_some()
    {
        return FocusPanelBody::CommandOutput {
            lines: body.lines().map(str::to_owned).collect(),
        };
    }
    FocusPanelBody::Text {
        lines: body.lines().map(str::to_owned).collect(),
    }
}

fn directory_entry_view(line: &str) -> DirectoryEntryView {
    let path = line.to_owned();
    let kind = if line.ends_with('/') {
        DirectoryEntryKind::Directory
    } else {
        DirectoryEntryKind::File
    };
    DirectoryEntryView { path, kind }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{keymap::Keymap, state::PatchLineView, theme::TuiTheme};
    use std::path::PathBuf;

    fn state() -> TuiState {
        TuiState::new(
            PathBuf::from("/repo"),
            "gpt-test".to_owned(),
            Keymap::default(),
            TuiTheme::default(),
        )
    }

    #[test]
    fn focus_panel_selects_latest_high_signal_item() {
        let mut state = state();
        state.push_timeline_item(TimelineItem::Expanded {
            title: "Ran cargo test".to_owned(),
            body: "  stdout: ok".to_owned(),
        });
        state.push_timeline_item(TimelineItem::Patch {
            changes: vec![PatchChangeView {
                path: "hello.py".to_owned(),
                added: 1,
                removed: 1,
                hunks: 1,
                bytes_before: Some(10),
                bytes_after: Some(11),
                lines: vec![
                    PatchLineView::remove("print('old')", Some(1)),
                    PatchLineView::add("print('new')", Some(1)),
                ],
            }],
        });

        let view = focus_panel_view(&state);

        assert_eq!(view.title, "FOCUS patch hello.py");
        assert!(matches!(view.body, FocusPanelBody::Patch { .. }));
    }

    #[test]
    fn focus_panel_uses_command_output_when_no_patch_exists() {
        let mut state = state();
        state.push_timeline_item(TimelineItem::Expanded {
            title: "Ran python3 hello_world.py (cwd: .)".to_owned(),
            body: "  stdout: hello world".to_owned(),
        });

        let view = focus_panel_view(&state);

        assert_eq!(view.title, "FOCUS command python3 hello_world.py");
        assert_eq!(
            view.body,
            FocusPanelBody::CommandOutput {
                lines: vec!["  stdout: hello world".to_owned()]
            }
        );
    }

    #[test]
    fn focus_panel_stays_on_reviewed_artifact_when_new_artifacts_arrive() {
        let mut state = state();
        state.push_timeline_item(TimelineItem::Expanded {
            title: "Ran first command".to_owned(),
            body: "first output".to_owned(),
        });
        state.push_timeline_item(TimelineItem::Expanded {
            title: "Ran second command".to_owned(),
            body: "second output".to_owned(),
        });
        state.select_previous_artifact();
        state.push_timeline_item(TimelineItem::Expanded {
            title: "Ran third command".to_owned(),
            body: "third output".to_owned(),
        });

        let view = focus_panel_view(&state);

        assert_eq!(view.title, "FOCUS command second command");
        assert_eq!(
            view.body,
            FocusPanelBody::CommandOutput {
                lines: vec!["second output".to_owned()]
            }
        );
    }

    #[test]
    fn focus_panel_has_explicit_empty_state() {
        let view = focus_panel_view(&state());

        assert_eq!(view.title, "FOCUS");
        assert_eq!(view.body, FocusPanelBody::Empty);
    }
}
