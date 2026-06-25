use super::{
    keymap::KeyAction,
    layout::{BottomPaneHeights, cockpit_layout},
    projector::TuiProjector,
    render,
    runtime::TuiRuntimeSession,
    state::TuiState,
    terminal::{TerminalEvent, TerminalSession},
};
use crate::cli_error::{CliError, unexpected};
use crossterm::event::{KeyCode, KeyEvent};
use futures_util::StreamExt;
use merry_core::QueuedInputLane;
use merry_runtime::InterruptReason;
use ratatui::layout::{Position, Rect, Size};
use std::time::Duration;
use tokio::time;

const TIMELINE_SCROLL_STEP: usize = 5;
const FOCUS_SCROLL_STEP: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerEffect {
    None,
    SubmitNext(String),
    SubmitBacklog(String),
    Interrupt,
    ResumeSuspended,
    DiscardSuspended,
    Quit,
}

pub(crate) fn handle_key_action(action: KeyAction, state: &mut TuiState) -> ControllerEffect {
    match action {
        KeyAction::SubmitNext => {
            if exit_review_if_active(state) {
                return ControllerEffect::None;
            }
            state
                .take_input_for_submit()
                .map_or(ControllerEffect::None, ControllerEffect::SubmitNext)
        }
        KeyAction::SubmitBacklog => {
            if exit_review_if_active(state) {
                return ControllerEffect::None;
            }
            state
                .take_input_for_submit()
                .map_or(ControllerEffect::None, ControllerEffect::SubmitBacklog)
        }
        KeyAction::CancelInputOrQuit => {
            if state.cancel_input_or_mark_quit() {
                ControllerEffect::Quit
            } else {
                ControllerEffect::None
            }
        }
        KeyAction::InsertNewline => {
            state.insert_input_newline();
            ControllerEffect::None
        }
        KeyAction::Interrupt => ControllerEffect::Interrupt,
        KeyAction::Quit => ControllerEffect::Quit,
        KeyAction::ScrollUp => {
            state.scroll_timeline_up_by(TIMELINE_SCROLL_STEP);
            ControllerEffect::None
        }
        KeyAction::ScrollDown => {
            state.scroll_timeline_down_by(TIMELINE_SCROLL_STEP);
            ControllerEffect::None
        }
        KeyAction::ReviewPreviousUserInput => {
            state.jump_to_previous_user_input();
            ControllerEffect::None
        }
        KeyAction::ReviewPreviousArtifact => {
            state.select_previous_artifact();
            ControllerEffect::None
        }
        KeyAction::ReviewNextArtifact => {
            state.select_next_artifact();
            ControllerEffect::None
        }
        KeyAction::FollowLatestArtifact => {
            state.exit_artifact_review();
            ControllerEffect::None
        }
        KeyAction::HistoryPrevious => {
            state.previous_input_history();
            ControllerEffect::None
        }
        KeyAction::HistoryNext => {
            state.next_input_history();
            ControllerEffect::None
        }
        KeyAction::ResumeSuspended => ControllerEffect::ResumeSuspended,
        KeyAction::DiscardSuspended => ControllerEffect::DiscardSuspended,
        _ => ControllerEffect::None,
    }
}

fn exit_review_if_active(state: &mut TuiState) -> bool {
    let was_reviewing = state.is_timeline_reviewing() || state.is_artifact_reviewing();
    if state.is_timeline_reviewing() {
        state.exit_timeline_review();
    }
    if state.is_artifact_reviewing() {
        state.exit_artifact_review();
    }
    was_reviewing
}

pub(crate) fn handle_key_event(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    if state.completion_menu().is_some() {
        match key.code {
            KeyCode::Enter | KeyCode::Tab => {
                state.accept_completion();
                return ControllerEffect::None;
            }
            KeyCode::Down => {
                state.select_next_completion();
                return ControllerEffect::None;
            }
            KeyCode::Up => {
                state.select_previous_completion();
                return ControllerEffect::None;
            }
            KeyCode::Esc => {
                state.close_completion_menu();
                return ControllerEffect::None;
            }
            _ => {}
        }
    }

    if let Some(action) = state.keymap().action_for(key.into()) {
        return handle_key_action(action, state);
    }
    state.handle_input_key(key);
    ControllerEffect::None
}

pub(crate) fn handle_mouse_scroll_up(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) {
    if position_in_focus_pane(position, terminal_size, state) {
        state.scroll_focus_up_by(FOCUS_SCROLL_STEP);
    } else {
        state.scroll_timeline_up_by(TIMELINE_SCROLL_STEP);
    }
}

pub(crate) fn handle_mouse_scroll_down(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) {
    if position_in_focus_pane(position, terminal_size, state) {
        state.scroll_focus_down_by(FOCUS_SCROLL_STEP);
    } else {
        state.scroll_timeline_down_by(TIMELINE_SCROLL_STEP);
    }
}

fn position_in_focus_pane(position: Position, terminal_size: Size, state: &TuiState) -> bool {
    let area = Rect::new(0, 0, terminal_size.width, terminal_size.height);
    let pane_heights = render::pane_heights_for_area(state, area);
    let rects = cockpit_layout(
        area,
        BottomPaneHeights {
            queue: pane_heights.queue,
            completion: pane_heights.completion,
            interaction: render::INTERACTION_HEIGHT,
            input: pane_heights.input,
            status: render::STATUS_HEIGHT,
        },
    );
    rects.focus.is_some_and(|focus| focus.contains(position))
}

pub(crate) async fn run_controller(
    mut terminal: TerminalSession,
    mut session: TuiRuntimeSession,
    mut state: TuiState,
) -> Result<(), CliError> {
    let mut projector = TuiProjector::default();
    let mut redraw = time::interval(Duration::from_millis(33));

    render_once(&mut terminal, &state)?;

    loop {
        tokio::select! {
            _ = redraw.tick() => {
                render_once(&mut terminal, &state)?;
            }
            event = terminal.next_event() => {
                let Some(event) = event.map_err(unexpected)? else {
                    break;
                };

                match event {
                    TerminalEvent::Key(key) => {
                        let effect = handle_key_event(key, &mut state);
                        project_local_effect(&effect, &mut state);
                        render_once(&mut terminal, &state)?;
                        let should_quit = dispatch_effect(
                            effect,
                            &mut session,
                        )
                        .await?;
                        if should_quit {
                            break;
                        }
                    }
                    TerminalEvent::MouseScrollUp(position) => {
                        let size = terminal.size().map_err(unexpected)?;
                        handle_mouse_scroll_up(position, size, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::MouseScrollDown(position) => {
                        let size = terminal.size().map_err(unexpected)?;
                        handle_mouse_scroll_down(position, size, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::Paste(text) => {
                        state.insert_input_paste(&text);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::Resize => {
                        render_once(&mut terminal, &state)?;
                    }
                }
            }
            event = session.stream.next() => {
                let Some(event) = event else {
                    break;
                };
                projector.apply(event, &mut state);
                render_once(&mut terminal, &state)?;
            }
        }
    }

    Ok(())
}

async fn dispatch_effect(
    effect: ControllerEffect,
    session: &mut TuiRuntimeSession,
) -> Result<bool, CliError> {
    match effect {
        ControllerEffect::None => Ok(false),
        ControllerEffect::SubmitNext(text) => {
            session.input.submit_next(&text).await.map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::SubmitBacklog(text) => {
            session.input.enqueue(&text).await.map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::Interrupt => {
            session
                .control
                .interrupt(InterruptReason::User)
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::ResumeSuspended => {
            session
                .control
                .resume_suspended()
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::DiscardSuspended => {
            session
                .control
                .discard_suspended()
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::Quit => {
            let control = session.control.clone();
            tokio::spawn(async move {
                let _ = control.close().await;
            });
            Ok(true)
        }
    }
}

fn project_local_effect(effect: &ControllerEffect, state: &mut TuiState) {
    match effect {
        ControllerEffect::SubmitNext(text) => {
            state.push_local_user_echo(text.clone(), QueuedInputLane::Next);
        }
        ControllerEffect::SubmitBacklog(text) => {
            state.push_local_user_echo(text.clone(), QueuedInputLane::Backlog);
        }
        _ => {}
    }
}

fn render_once(terminal: &mut TerminalSession, state: &TuiState) -> Result<(), CliError> {
    terminal
        .draw(|frame| render::render(frame, state))
        .map_err(unexpected)?;
    Ok(())
}
