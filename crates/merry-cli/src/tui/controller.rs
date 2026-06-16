use super::{
    keymap::KeyAction,
    projector::TuiProjector,
    render,
    runtime::TuiRuntimeSession,
    state::TuiState,
    terminal::{TerminalEvent, TerminalSession},
};
use crate::cli_error::{CliError, unexpected};
use futures_util::StreamExt;
use merry_runtime::InterruptReason;
use std::time::Duration;
use tokio::time;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerEffect {
    None,
    SubmitNext(String),
    SubmitBacklog(String),
    Interrupt,
    Quit,
}

pub(crate) fn handle_key_action(action: KeyAction, state: &mut TuiState) -> ControllerEffect {
    match action {
        KeyAction::SubmitNext => state
            .input_mut()
            .take_trimmed()
            .map_or(ControllerEffect::None, ControllerEffect::SubmitNext),
        KeyAction::SubmitBacklog => state
            .input_mut()
            .take_trimmed()
            .map_or(ControllerEffect::None, ControllerEffect::SubmitBacklog),
        KeyAction::Interrupt => ControllerEffect::Interrupt,
        KeyAction::Quit => ControllerEffect::Quit,
        _ => ControllerEffect::None,
    }
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
                        if let Some(action) = state.keymap().action_for(key.into()) {
                            let should_quit = dispatch_effect(
                                handle_key_action(action, &mut state),
                                &mut session,
                            )
                            .await?;
                            if should_quit {
                                break;
                            }
                        } else {
                            state.input_mut().handle_key(key);
                        }
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
        ControllerEffect::Quit => {
            session.control.close().await.map_err(unexpected)?;
            Ok(true)
        }
    }
}

fn render_once(terminal: &mut TerminalSession, state: &TuiState) -> Result<(), CliError> {
    terminal
        .draw(|frame| render::render(frame, state))
        .map_err(unexpected)?;
    Ok(())
}
