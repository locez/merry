use super::input::TextInput;
use super::keymap::{KeyAction, KeyBinding, Keymap};
use super::projector::TuiProjector;
use super::render::render_to_text;
use super::state::{QueuePreview, TimelineItem, TuiState};
use super::theme::{SemanticColor, TuiTheme};
use crossterm::event::{KeyCode, KeyModifiers};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ContextWindowSource, ModelUsage, PendingToolCall,
    QueuedInputLane, QueuedInputView, RuntimeEvent, RuntimeEventSource, SessionId, SessionUsage,
    ToolCallArguments, ToolCallId, ToolCallResult, ToolName, ToolOutput, UsageContextWindow,
};
use merry_tool_workspace::WORKSPACE_PATCH_TOOL;

fn source() -> RuntimeEventSource {
    RuntimeEventSource::new(SessionId::new("tui-test").unwrap(), 1)
}

fn text_artifact(id: &str) -> ArtifactRef {
    ArtifactRef::new(ArtifactId::new(id).unwrap(), ArtifactKind::Text)
}

fn pending_call(id: &str, tool_name: &str) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(id).unwrap(),
        ToolName::new(tool_name).unwrap(),
        ToolCallArguments::new(Default::default()),
    )
}

#[test]
fn text_input_inserts_deletes_and_takes_trimmed_text() {
    let mut input = TextInput::default();

    input.insert_char('h');
    input.insert_char('i');
    input.backspace();
    input.insert_char('!');

    assert_eq!(input.text(), "h!");
    assert_eq!(input.take_trimmed(), Some("h!".to_owned()));
    assert_eq!(input.text(), "");
    assert_eq!(input.take_trimmed(), None);
}

#[test]
fn default_keymap_maps_enter_to_submit_next_and_ctrl_b_to_backlog() {
    let keymap = Keymap::default();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(KeyAction::SubmitNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL,)),
        Some(KeyAction::SubmitBacklog)
    );
}

#[test]
fn queue_preview_keeps_actual_text_and_truncates_for_display() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "short next".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "a very long backlog item that should be truncated".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });

    assert_eq!(state.queue_preview().next[0].text, "short next");
    assert_eq!(
        state.queue_preview().backlog[0].display_text(18),
        "a very long bac..."
    );
}

#[test]
fn theme_has_required_semantic_color_slots() {
    let theme = TuiTheme::default();

    for slot in [
        SemanticColor::Status,
        SemanticColor::Muted,
        SemanticColor::Focus,
        SemanticColor::Selection,
        SemanticColor::DiffAdd,
        SemanticColor::DiffDelete,
        SemanticColor::Warning,
        SemanticColor::Error,
        SemanticColor::Risk,
        SemanticColor::Success,
    ] {
        assert!(theme.color(slot).is_some());
    }
}

#[test]
fn projector_renders_assistant_text_as_primary_timeline_item() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::AssistantMessage {
            text: "hello from assistant".to_owned(),
            artifact: text_artifact("assistant-1"),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        &[TimelineItem::Assistant {
            text: "hello from assistant".to_owned()
        }]
    );
}

#[test]
fn projector_collapses_read_tool_and_expands_patch_tool() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-read", "workspace_read_file"),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-read").unwrap(),
                text_artifact("read-output"),
            ),
            output: Some(ToolOutput::Text {
                text: "file contents".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-patch", WORKSPACE_PATCH_TOOL),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-patch").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Text {
                text: "--- a/src/lib.rs\n+++ b/src/lib.rs\n+new line".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert!(matches!(state.timeline()[0], TimelineItem::Muted { .. }));
    assert!(matches!(state.timeline()[1], TimelineItem::Muted { .. }));
    assert!(matches!(state.timeline()[2], TimelineItem::Muted { .. }));
    assert!(matches!(state.timeline()[3], TimelineItem::Expanded { .. }));
}

#[test]
fn projector_expands_workspace_patch_by_tool_name() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-patch", WORKSPACE_PATCH_TOOL),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-patch").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"tool":"workspace_patch","status":"applied"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert!(matches!(state.timeline()[1], TimelineItem::Expanded { .. }));
}

#[test]
fn projector_keeps_diff_like_non_patch_output_muted() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-read", "workspace_read_file"),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-read").unwrap(),
                text_artifact("read-output"),
            ),
            output: Some(ToolOutput::Text {
                text: "--- a/src/lib.rs\n+++ b/src/lib.rs\n+not a patch result".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert!(matches!(state.timeline()[1], TimelineItem::Muted { .. }));
}

#[test]
fn projector_updates_queue_preview_and_usage_without_timeline_noise() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::QueuedInputsChanged {
            inputs: merry_core::QueuedInputsView {
                next: vec![merry_core::QueuedInputView {
                    text: "urgent".to_owned(),
                    lane: merry_core::QueuedInputLane::Next,
                    position: 0,
                }],
                suspended: vec![],
                backlog: vec![],
            },
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::UsageUpdated {
            usage: SessionUsage {
                total: ModelUsage::new(10, 3),
                last: ModelUsage::new(10, 3),
                context: Some(UsageContextWindow {
                    resolved_model_window_tokens: 128000,
                    effective_window_tokens: 128000,
                    source: ContextWindowSource::ProviderCapabilities,
                }),
                compaction: None,
            },
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.queue_preview().next[0].text, "urgent");
    assert!(state.timeline().is_empty());
    assert!(state.status_text().contains("13 tok"));
}

#[test]
fn renderer_shows_status_timeline_queue_and_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "assistant says hello".to_owned(),
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });
    state.input_mut().insert_char('h');
    state.input_mut().insert_char('i');

    let text = render_to_text(&state, 80, 24);

    assert!(text.contains("gpt-test"));
    assert!(text.contains("assistant says hello"));
    assert!(text.contains("Next"));
    assert!(text.contains("next item"));
    assert!(text.contains("Backlog"));
    assert!(text.contains("backlog item"));
    assert!(text.contains("hi"));
}
