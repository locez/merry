use crate::tui::{
    keymap::Keymap,
    projector::TuiProjector,
    render::{render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    tests::{find_cell_color, find_text_position, pending_call_with_args, source, text_artifact},
    theme::TuiTheme,
};
use merry_core::{
    ErrorInfo, RuntimeEvent, TOOL_CANCELLED_BY_USER_CODE, ToolCallId, ToolCallResult, ToolOutput,
};
use merry_runtime::SessionTranscriptItem;
use ratatui::style::Color;
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

fn start_command(projector: &mut TuiProjector, state: &mut TuiState, id: &str, command: &str) {
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                id,
                "run_process",
                json!({"command": command, "cwd": "."}),
            ),
            source: source(),
        },
        state,
    );
}

fn process_output(
    exit_code: i64,
    stdout: &str,
    stderr: &str,
    stdout_truncated: bool,
    stderr_truncated: bool,
) -> ToolOutput {
    ToolOutput::Json {
        json: json!({
            "kind": "process_action",
            "status": {"kind": "exited", "code": exit_code},
            "stdout": {"text": stdout, "bytes": stdout.len(), "truncated": stdout_truncated},
            "stderr": {"text": stderr, "bytes": stderr.len(), "truncated": stderr_truncated},
        })
        .to_string(),
    }
}

fn process_result(id: &str, exit_code: i64) -> ToolCallResult {
    let call_id = ToolCallId::new(id).unwrap();
    let artifact = text_artifact(&format!("{id}-output"));
    if exit_code == 0 {
        ToolCallResult::succeeded(call_id, artifact)
    } else {
        ToolCallResult::failed(
            call_id,
            artifact,
            ErrorInfo::new(
                "process_action_failed",
                &format!("process exited with code {exit_code}"),
            )
            .unwrap(),
        )
    }
}

fn finish_command(
    projector: &mut TuiProjector,
    state: &mut TuiState,
    id: &str,
    exit_code: i64,
    output: ToolOutput,
) {
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: process_result(id, exit_code),
            output: Some(output),
            source: source(),
        },
        state,
    );
}

fn rendered_preview(state: &TuiState, title: &str) -> Vec<String> {
    let rendered = render_to_text(state, 120, 24);
    let rows = rendered.lines().map(str::trim_end).collect::<Vec<_>>();
    let title_index = rows
        .iter()
        .position(|row| row.strip_prefix(' ') == Some(title))
        .expect("command title should render");
    rows[title_index + 1..]
        .iter()
        .take_while(|row| row.starts_with("   "))
        .map(|row| row.trim().to_owned())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn slow_command_duration_is_visible_and_stops_after_completion() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    start_command(&mut projector, &mut state, "slow", "cargo test");
    assert!(!render_to_text(&state, 120, 24).contains("0.0s"));
    tokio::time::advance(Duration::from_secs(13)).await;
    let running = render_to_text(&state, 120, 24);
    assert!(running.contains("Running"));
    assert!(running.contains("cargo test (.)  13.0s"));
    tokio::time::advance(Duration::from_secs(2)).await;
    finish_command(
        &mut projector,
        &mut state,
        "slow",
        128,
        process_output(128, "failure", "", false, false),
    );
    let finished = render_to_text(&state, 120, 24);
    assert!(finished.contains("Ran cargo test (.) -> 128  15.0s"));
    let buffer = render_to_buffer(&state, 120, 24);
    assert_eq!(find_cell_color(&buffer, "128"), Some(Color::LightRed));
    assert_eq!(find_cell_color(&buffer, "Ran"), Some(Color::LightCyan));
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(finished, render_to_text(&state, 120, 24));
}

#[test]
fn wrapped_command_continuations_align_with_the_command_body() {
    let command = format!("printf '%s' '{}end'", "long-argument-".repeat(10));
    let mut state = state();
    let mut projector = TuiProjector::default();
    start_command(&mut projector, &mut state, "wrapped", &command);
    for finished in [false, true] {
        if finished {
            finish_command(
                &mut projector,
                &mut state,
                "wrapped",
                0,
                process_output(0, "", "", false, false),
            );
        }
        let rendered = render_to_text(&state, 36, 32);
        let rows = rendered.lines().collect::<Vec<_>>();
        let start = rows.iter().position(|row| row.contains("printf")).unwrap();
        let byte_column = rows[start].find("printf").unwrap();
        let column = unicode_width::UnicodeWidthStr::width(&rows[start][..byte_column]);
        let continuation_prefix = " ".repeat(column);
        let continuations = rows[start + 1..]
            .iter()
            .take_while(|row| !row.trim().is_empty() && row.starts_with(&continuation_prefix))
            .collect::<Vec<_>>();
        assert!(!continuations.is_empty(), "{rendered}");
        let mut joined = rows[start][byte_column..].to_owned();
        for line in continuations {
            joined.push_str(&line[column..]);
        }
        assert_eq!(
            joined.split_whitespace().collect::<String>(),
            format!("{command} (.)")
                .split_whitespace()
                .collect::<String>()
        );
        assert!(!joined.contains("..."));
    }
}

#[tokio::test(start_paused = true)]
async fn running_command_animates_and_completion_stops_the_animation() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    start_command(&mut projector, &mut state, "process", "git status");

    let first = render_to_text(&state, 120, 24);
    assert!(first.contains("Running "));
    assert!(first.contains("git status (.)"));
    assert!(!first.contains("Ran "));
    let buffer = render_to_buffer(&state, 120, 24);
    assert_eq!(find_cell_color(&buffer, "Running"), Some(Color::LightCyan));
    assert_eq!(find_cell_color(&buffer, "git"), Some(Color::LightBlue));

    tokio::time::advance(Duration::from_millis(100)).await;
    let second = render_to_text(&state, 120, 24);
    assert!(second.contains("Running "));
    assert_ne!(first, second);

    finish_command(
        &mut projector,
        &mut state,
        "process",
        0,
        process_output(0, "clean", "", false, false),
    );
    assert_eq!(state.timeline().len(), 1);
    let completed = render_to_text(&state, 120, 24);
    assert!(completed.contains("Ran git status (.)"));
    assert!(!completed.contains("Running "));
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_eq!(completed, render_to_text(&state, 120, 24));
}

#[test]
fn successful_commands_hide_output_by_default_and_form_a_compact_list() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    for id in ["cmd1", "cmd2"] {
        start_command(&mut projector, &mut state, id, id);
        finish_command(
            &mut projector,
            &mut state,
            id,
            0,
            process_output(0, "hidden-stdout", "hidden-stderr", true, true),
        );
        assert!(rendered_preview(&state, &format!("Ran {id} (.)")).is_empty());
    }
    let rendered = render_to_text(&state, 120, 24);
    let rows = rendered.lines().map(str::trim_end).collect::<Vec<_>>();
    assert!(
        rows.windows(2)
            .any(|rows| rows == [" Ran cmd1 (.)", " Ran cmd2 (.)"])
    );
    assert!(!rendered.contains("hidden-stdout"));
    assert!(!rendered.contains("hidden-stderr"));
}

#[test]
fn timeline_gutter_insets_commands_outputs_and_assistant_text() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    start_command(&mut projector, &mut state, "process", "printf value");
    finish_command(
        &mut projector,
        &mut state,
        "process",
        128,
        process_output(128, "first\nsecond", "", false, false),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "assistant message".to_owned(),
    });

    let buffer = render_to_buffer(&state, 60, 20);
    let (command_column, command_row) =
        find_text_position(&buffer, "Ran printf value").expect("command should be visible");
    let (assistant_column, assistant_row) =
        find_text_position(&buffer, "assistant message").expect("assistant should be visible");
    assert_eq!(command_column, 1);
    assert_eq!(assistant_column, 1);
    for output in ["first", "second"] {
        let (column, _) = find_text_position(&buffer, output).expect("output should be visible");
        assert_eq!(column, 3);
    }
    for row in command_row..=assistant_row {
        assert_eq!(buffer[(0, row)].symbol(), " ");
    }
}

#[test]
fn timeline_gutter_stays_empty_on_wrapped_command_rows() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    let command = format!("printf {} command-tail", "0123456789".repeat(20));
    start_command(&mut projector, &mut state, "process", &command);

    let buffer = render_to_buffer(&state, 24, 28);
    let (_, first_row) = find_text_position(&buffer, "Running").expect("command should be visible");
    let (_, last_row) =
        find_text_position(&buffer, "command-tail").expect("command tail should be visible");
    assert!(last_row > first_row);
    for row in first_row..=last_row {
        assert_eq!(
            buffer[(0, row)].symbol(),
            " ",
            "wrapped row {row} should keep the gutter"
        );
    }
}

#[test]
fn padded_timeline_keeps_wrapped_command_tail_visible_after_scrolling() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    let command = format!("echo {} TAIL_SENTINEL", "abcdefghij".repeat(60));
    start_command(&mut projector, &mut state, "process", &command);
    finish_command(
        &mut projector,
        &mut state,
        "process",
        0,
        process_output(0, "", "", false, false),
    );

    let bottom = render_to_text(&state, 20, 12);
    assert!(bottom.contains("TAIL_SENTINEL"));
    state.scroll_timeline_up_by(3);
    assert!(!render_to_text(&state, 20, 12).contains("TAIL_SENTINEL"));
    state.scroll_timeline_down_by(3);
    assert_eq!(render_to_text(&state, 20, 12), bottom);
}

#[test]
fn command_previews_respect_the_output_toggle_and_both_truncation_boundaries() {
    for exit_code in [0, 128] {
        for show_output in [false, true] {
            for (stdout, stderr, stdout_truncated, stderr_truncated, expected) in [
                ("", "", false, false, &[][..]),
                (
                    "one\ntwo\nthree\nfour\nfive\n",
                    "",
                    false,
                    false,
                    &["one", "two", "three", "four", "five"][..],
                ),
                (
                    "one\ntwo\nthree\nfour\nfive\nsix\n",
                    "",
                    false,
                    false,
                    &["one", "two", "three", "four", "five", "..."][..],
                ),
                (
                    "one\ntwo\nthree\nfour\nfive\n",
                    "stderr",
                    false,
                    false,
                    &["one", "two", "three", "four", "five", "..."][..],
                ),
                (
                    "one\ntwo\nthree\n",
                    "four\nfive\nsix\n",
                    false,
                    false,
                    &["one", "two", "three", "four", "five", "..."][..],
                ),
                ("one\ntwo\n", "", true, false, &["one", "two", "..."][..]),
                ("", "stderr\n", false, true, &["stderr", "..."][..]),
                (
                    "one\ntwo\nthree\nfour\nfive\n\n",
                    "\n",
                    false,
                    false,
                    &["one", "two", "three", "four", "five"][..],
                ),
                ("", "", true, false, &["..."][..]),
            ] {
                let mut state = state().with_successful_command_output(show_output);
                let mut projector = TuiProjector::default();
                start_command(&mut projector, &mut state, "process", "cmd");
                finish_command(
                    &mut projector,
                    &mut state,
                    "process",
                    exit_code,
                    process_output(
                        exit_code,
                        stdout,
                        stderr,
                        stdout_truncated,
                        stderr_truncated,
                    ),
                );

                let title = if exit_code == 0 {
                    "Ran cmd (.)"
                } else {
                    "Ran cmd (.) -> 128"
                };
                let expected = if exit_code == 0 && !show_output {
                    &[][..]
                } else {
                    expected
                };
                assert_eq!(
                    rendered_preview(&state, title),
                    expected,
                    "exit={exit_code}, show={show_output}, stdout={stdout:?}, stderr={stderr:?}"
                );
            }
        }
    }
}

#[test]
fn long_commands_wrap_without_losing_arguments_or_the_working_directory() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    let command = format!(
        "printf '%s' '{}' && echo command-tail",
        "中文-long-token-".repeat(20)
    );
    let cwd = format!("src/{}directory-tail", "nested/".repeat(20));
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "long-process",
                "run_process",
                json!({"command": command, "cwd": cwd}),
            ),
            source: source(),
        },
        &mut state,
    );

    let expected = format!("{command} ({cwd})")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    for running in [true, false] {
        if !running {
            finish_command(
                &mut projector,
                &mut state,
                "long-process",
                0,
                process_output(0, "", "", false, false),
            );
        }
        let rendered = render_to_text(&state, 40, 64);
        let compact = rendered
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(
            compact.contains(&expected),
            "complete command should wrap while running={running}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_commands_stop_animating_without_showing_a_successful_result() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    start_command(&mut projector, &mut state, "process", "sleep 30");
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("process").unwrap(),
                text_artifact("cancelled-output"),
                ErrorInfo::new(TOOL_CANCELLED_BY_USER_CODE, "cancelled by user").unwrap(),
            ),
            output: None,
            source: source(),
        },
        &mut state,
    );

    let rendered = render_to_text(&state, 120, 24);
    assert!(rendered.contains("Ran sleep 30 (.) -> cancelled"));
    assert!(!rendered.contains("Running "));
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_eq!(rendered, render_to_text(&state, 120, 24));
    assert!(matches!(
        crate::tui::controller::handle_key_action(
            crate::tui::keymap::KeyAction::OpenCommandDetails,
            &mut state
        ),
        crate::tui::controller::ControllerEffect::LoadCommandOutput(_)
    ));
}

#[test]
fn terminal_run_events_stop_pending_command_animations() {
    for (event, status) in [
        (
            RuntimeEvent::RunFailed {
                diagnostic: ErrorInfo::new("run_failed", "runtime failure").unwrap(),
                source: source(),
            },
            "failed",
        ),
        (
            RuntimeEvent::RunCancelled {
                diagnostic: ErrorInfo::new("run_cancelled", "runtime cancelled").unwrap(),
                source: source(),
            },
            "cancelled",
        ),
        (RuntimeEvent::Closed, "interrupted"),
    ] {
        let mut state = state();
        let mut projector = TuiProjector::default();
        start_command(&mut projector, &mut state, "pending", "sleep 30");
        projector.apply(event, &mut state);
        let rendered = render_to_text(&state, 120, 24);
        assert!(rendered.contains(&format!("Ran sleep 30 (.) -> {status}")));
        assert!(!rendered.contains("Running "));
    }
}

#[test]
fn resumed_commands_use_the_same_completion_and_output_policy() {
    for exit_code in [0, 128] {
        let mut state = state();
        let mut projector = TuiProjector::default();
        projector.apply_transcript_item(
            SessionTranscriptItem::ToolCall {
                call: pending_call_with_args(
                    "saved",
                    "run_process",
                    json!({"command": "cmd", "cwd": "."}),
                ),
            },
            &mut state,
        );
        projector.apply_transcript_item(
            SessionTranscriptItem::ToolResult {
                call_id: ToolCallId::new("saved").unwrap(),
                result: process_result("saved", exit_code),
                output: Some(process_output(exit_code, "output", "", false, false)),
            },
            &mut state,
        );

        if exit_code == 0 {
            assert!(rendered_preview(&state, "Ran cmd (.)").is_empty());
        } else {
            assert_eq!(rendered_preview(&state, "Ran cmd (.) -> 128"), ["output"]);
        }
        assert!(!render_to_text(&state, 120, 24).contains("Running "));
    }
}
