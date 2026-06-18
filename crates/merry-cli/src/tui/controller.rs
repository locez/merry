use super::{
    keymap::KeyAction,
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
use std::time::Duration;
use tokio::time;

const TIMELINE_SCROLL_STEP: usize = 5;

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
    if !state.is_timeline_reviewing() {
        return false;
    }
    state.exit_timeline_review();
    true
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
                    TerminalEvent::MouseScrollUp => {
                        state.scroll_timeline_up_by(TIMELINE_SCROLL_STEP);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::MouseScrollDown => {
                        state.scroll_timeline_down_by(TIMELINE_SCROLL_STEP);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::Paste(text) => {
                        state.insert_input_str(&text);
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
