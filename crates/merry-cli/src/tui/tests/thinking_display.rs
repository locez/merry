use crate::tui::{
    controller::{ControllerEffect, handle_mouse_input},
    keymap::Keymap,
    projector::TuiProjector,
    render::{render_to_buffer, render_to_text},
    state::TuiState,
    tests::{find_cell_style, find_text_position, pending_call_with_args, source},
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use merry_core::{ErrorInfo, RuntimeEvent, ToolCallId, ToolCallResult, ToolOutput};
use ratatui::{
    layout::{Position, Size},
    style::Color,
};
use serde_json::json;
use std::time::Duration;

fn state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn reasoning(delta: &str) -> RuntimeEvent {
    RuntimeEvent::ReasoningDelta {
        delta: delta.to_owned(),
        source: source(),
    }
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn point(buffer: &ratatui::buffer::Buffer, text: &str) -> Position {
    let (column, row) =
        find_text_position(buffer, text).unwrap_or_else(|| panic!("missing {text}"));
    Position::new(column, row)
}

fn click(state: &mut TuiState, size: Size, position: Position) {
    assert_eq!(
        handle_mouse_input(MouseInput::Down(position), size, state),
        ControllerEffect::None
    );
}

#[tokio::test(start_paused = true)]
async fn reasoning_stays_open_with_a_rail_then_collapses_for_the_reply() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    projector.apply(reasoning("first thought\n"), &mut state);
    projector.apply(reasoning("second thought"), &mut state);

    let size = Size::new(40, 24);
    let running = render_to_buffer(&state, size.width, size.height);
    let running_text = buffer_text(&running);
    assert!(running_text.contains("Thinking…"), "{running_text}");
    assert!(running_text.contains("first thought"), "{running_text}");
    assert!(running_text.contains("second thought"), "{running_text}");
    let (_, row) = find_text_position(&running, "Thinking").expect("thinking header");
    assert!(
        matches!(running[(0, row)].symbol(), "│" | "┃"),
        "expected vertical thinking rail glyph, got {:?}",
        running[(0, row)].symbol()
    );
    assert!(matches!(
        running[(0, row)].style().fg,
        Some(Color::Rgb(_, _, _))
    ));

    tokio::time::advance(Duration::from_millis(1500)).await;
    projector.apply(
        RuntimeEvent::AssistantMessageDelta {
            delta: "answer".to_owned(),
            source: source(),
        },
        &mut state,
    );
    let collapsed = render_to_buffer(&state, size.width, size.height);
    let collapsed_text = buffer_text(&collapsed);
    assert!(
        collapsed_text.contains("Thought for 1.5s"),
        "{collapsed_text}"
    );
    assert!(
        !collapsed_text.contains("first thought"),
        "{collapsed_text}"
    );
    assert!(collapsed_text.contains("answer"), "{collapsed_text}");
    assert!(collapsed_text.contains('▸'), "{collapsed_text}");

    click(&mut state, size, point(&collapsed, "Thought"));
    let opened = render_to_buffer(&state, size.width, size.height);
    let opened_text = buffer_text(&opened);
    assert!(opened_text.contains("first thought"), "{opened_text}");
    assert!(opened_text.contains("second thought"), "{opened_text}");
    assert!(opened_text.contains('▾'), "{opened_text}");
    assert!(state.text_selection().is_none());

    click(&mut state, size, point(&opened, "▾"));
    let closed_again = render_to_text(&state, size.width, size.height);
    assert!(!closed_again.contains("first thought"), "{closed_again}");
    assert!(closed_again.contains('▸'), "{closed_again}");
}

#[tokio::test(start_paused = true)]
async fn running_thinking_keeps_only_the_latest_lines() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    let body = (1..=6)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    projector.apply(reasoning(&body), &mut state);

    let rendered = render_to_text(&state, 40, 24);
    assert!(rendered.contains("Thinking…"), "{rendered}");
    assert!(
        rendered.lines().any(|line| {
            line.trim().trim_start_matches(['│', '┃', '▸', '▾']).trim() == "…"
        }),
        "{rendered}"
    );
    assert!(rendered.contains("line-4"), "{rendered}");
    assert!(rendered.contains("line-6"), "{rendered}");
    assert!(!rendered.contains("line-1"), "{rendered}");
    assert!(!rendered.contains('▸'), "{rendered}");
}

#[tokio::test(start_paused = true)]
async fn a_thinking_only_turn_collapses_when_the_step_ends() {
    let endings = [
        RuntimeEvent::StepCompleted { source: source() },
        RuntimeEvent::RunFailed {
            diagnostic: ErrorInfo::new("run_failed", "runtime failed").unwrap(),
            source: source(),
        },
        RuntimeEvent::RunCancelled {
            diagnostic: ErrorInfo::new("run_cancelled", "runtime cancelled").unwrap(),
            source: source(),
        },
        RuntimeEvent::Closed,
    ];
    for event in endings {
        let mut state = state();
        let mut projector = TuiProjector::default();
        projector.apply(reasoning("only thought"), &mut state);
        projector.apply(event, &mut state);
        let rendered = render_to_text(&state, 40, 24);
        assert!(rendered.contains("Thought for 0.0s"), "{rendered}");
        assert!(!rendered.contains("only thought"), "{rendered}");
    }
}

#[test]
fn successful_command_output_uses_a_terminal_panel_by_default() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "process",
                "run_process",
                json!({"command": "printf value", "cwd": "."}),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(ToolCallId::new("process").unwrap()),
            output: Some(ToolOutput::Json {
                json: json!({
                    "kind": "process_action",
                    "status": {"kind": "exited", "code": 0},
                    "stdout": {"text": "panel-line", "bytes": 10, "truncated": false},
                    "stderr": {"text": "", "bytes": 0, "truncated": false},
                })
                .to_string(),
            }),
            source: source(),
        },
        &mut state,
    );

    state.toggle_command_expanded(0);
    let buffer = render_to_buffer(&state, 80, 24);
    let rendered = buffer_text(&buffer);
    assert!(rendered.contains("panel-line"), "{rendered}");
    let style = find_cell_style(&buffer, "panel-line").expect("output cell");
    assert_eq!(
        style.fg,
        TuiTheme::default().color(crate::tui::theme::SemanticColor::TextSecondary)
    );
    assert_eq!(style.bg, Some(Color::Reset));
}

#[tokio::test(start_paused = true)]
async fn running_command_stays_collapsed_until_clicked_to_reveal_live_output() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    let call = pending_call_with_args("live-cmd", "run_process", json!({ "command": "watch" }));
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call,
            source: source(),
        },
        &mut state,
    );

    let size = Size::new(80, 20);
    let initial = render_to_buffer(&state, size.width, size.height);
    let initial_text = buffer_text(&initial);
    assert!(initial_text.contains("Running watch"), "{initial_text}");
    assert!(
        !initial_text.contains('▸'),
        "no chevron before live output arrives:\n{initial_text}"
    );

    projector.apply(
        RuntimeEvent::ToolCallOutputDelta {
            call_id: ToolCallId::new("live-cmd").unwrap(),
            stream: merry_core::ToolOutputStream::Stdout,
            delta: "worker-1 tick 1\nworker-1 tick 2\n".to_owned(),
            source: source(),
        },
        &mut state,
    );

    let collapsed = render_to_buffer(&state, size.width, size.height);
    let collapsed_text = buffer_text(&collapsed);
    assert!(
        collapsed_text.contains('▸'),
        "chevron should appear once live output arrives:\n{collapsed_text}"
    );
    assert!(
        !collapsed_text.contains("worker-1 tick 1"),
        "live output must stay collapsed by default:\n{collapsed_text}"
    );

    click(&mut state, size, point(&collapsed, "watch"));
    let expanded = render_to_buffer(&state, size.width, size.height);
    let expanded_text = buffer_text(&expanded);
    assert!(expanded_text.contains('▾'), "{expanded_text}");
    assert!(expanded_text.contains("worker-1 tick 1"), "{expanded_text}");
    assert!(expanded_text.contains("worker-1 tick 2"), "{expanded_text}");

    // Streaming more lines rolls forward to always show the newest tail with
    // a leading `…` at the top (matching live Thinking), never a trailing `...`.
    let more: String = (3..=12)
        .map(|tick| format!("worker-1 tick {tick}\n"))
        .collect();
    projector.apply(
        RuntimeEvent::ToolCallOutputDelta {
            call_id: ToolCallId::new("live-cmd").unwrap(),
            stream: merry_core::ToolOutputStream::Stdout,
            delta: more,
            source: source(),
        },
        &mut state,
    );
    let rolling = render_to_buffer(&state, size.width, size.height);
    let rolling_text = buffer_text(&rolling);
    assert!(
        !rolling_text.contains("worker-1 tick 1\n") && !rolling_text.contains("worker-1 tick 2"),
        "older lines should scroll off the top:\n{rolling_text}"
    );
    assert!(rolling_text.contains("worker-1 tick 12"), "{rolling_text}");
    assert!(!rolling_text.contains("  ..."), "{rolling_text}");
    let ellipsis_row = find_text_position(&rolling, "…").expect("leading ellipsis");
    let newest_row = find_text_position(&rolling, "worker-1 tick 12").expect("newest line");
    assert!(
        ellipsis_row.1 < newest_row.1,
        "leading `…` must sit above the newest live output:\n{rolling_text}"
    );

    click(&mut state, size, point(&rolling, "▾"));
    let refolded = render_to_text(&state, size.width, size.height);
    assert!(!refolded.contains("worker-1 tick 12"), "{refolded}");
    assert!(refolded.contains('▸'), "{refolded}");
}
