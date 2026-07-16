use super::{
    command::{self, PaletteCommand, SlashCommandMatch},
    controller::{ControllerEffect, handle_key_action},
    keymap::KeyAction,
    state::{TimelineItem, TuiState},
};

pub(crate) fn slash_input_effect(state: &mut TuiState) -> Option<ControllerEffect> {
    let matched = state
        .plain_input_text()
        .map(command::match_slash_input)
        .unwrap_or(SlashCommandMatch::NotCommand);
    match matched {
        SlashCommandMatch::NotCommand => None,
        SlashCommandMatch::Known(command) => {
            state.clear_input();
            Some(run_palette_command(command.command, state))
        }
        SlashCommandMatch::Unknown(name) => {
            let detail = if name.is_empty() {
                "Type /help to list commands, or prefix the text with a space to send it."
                    .to_owned()
            } else {
                format!(
                    "/{name} is not available. Type /help, or prefix the text with a space to send it."
                )
            };
            state.close_completion_menu();
            state.push_timeline_item(TimelineItem::Muted {
                title: "Unknown command".to_owned(),
                detail,
            });
            Some(ControllerEffect::None)
        }
        SlashCommandMatch::ArgumentsNotSupported(name) => {
            let detail = format!("/{name} does not accept arguments.");
            state.close_completion_menu();
            state.push_timeline_item(TimelineItem::Muted {
                title: "Command not run".to_owned(),
                detail,
            });
            Some(ControllerEffect::None)
        }
    }
}

pub(crate) fn run_palette_command(
    command: PaletteCommand,
    state: &mut TuiState,
) -> ControllerEffect {
    if let Some(effect) = super::plan_controller::palette_effect(command, state) {
        return effect;
    }
    match command {
        PaletteCommand::OpenSettings => {
            state.close_overlay();
            state.open_settings();
            ControllerEffect::None
        }
        PaletteCommand::OpenProviders => ControllerEffect::OpenProviderManager,
        PaletteCommand::ShowShortcuts => {
            state.open_shortcuts();
            ControllerEffect::None
        }
        PaletteCommand::ShowHelp => {
            let body = command::slash_help_body(state.keymap());
            state.close_overlay();
            state.push_timeline_item(TimelineItem::Expanded {
                title: "Command help".to_owned(),
                body,
            });
            ControllerEffect::None
        }
        PaletteCommand::ShowStatus => {
            let body = state.command_status_body();
            state.close_overlay();
            state.push_timeline_item(TimelineItem::Expanded {
                title: "Session status".to_owned(),
                body,
            });
            ControllerEffect::None
        }
        PaletteCommand::FollowLatest => {
            state.close_overlay();
            handle_key_action(KeyAction::FollowLatestArtifact, state)
        }
        PaletteCommand::ReviewPreviousArtifact => {
            state.close_overlay();
            handle_key_action(KeyAction::ReviewPreviousArtifact, state)
        }
        PaletteCommand::ReviewNextArtifact => {
            state.close_overlay();
            handle_key_action(KeyAction::ReviewNextArtifact, state)
        }
        PaletteCommand::ReviewPreviousUserInput => {
            state.close_overlay();
            handle_key_action(KeyAction::ReviewPreviousUserInput, state)
        }
        PaletteCommand::Interrupt => {
            state.close_overlay();
            if state.can_interrupt_run() {
                state.set_run_state(merry_core::InteractiveRunState::Interrupting);
                state.push_timeline_item(TimelineItem::Muted {
                    title: "Stopping".to_owned(),
                    detail: "Interrupt requested for the active run.".to_owned(),
                });
                ControllerEffect::Interrupt
            } else if state.is_interrupting() {
                state.push_timeline_item(TimelineItem::Muted {
                    title: "Stop already requested".to_owned(),
                    detail: "Waiting for the active run to reach a cancellation boundary."
                        .to_owned(),
                });
                ControllerEffect::None
            } else {
                state.push_timeline_item(TimelineItem::Muted {
                    title: "Nothing to stop".to_owned(),
                    detail: "No model or tool run is active.".to_owned(),
                });
                ControllerEffect::None
            }
        }
        PaletteCommand::ResumeSuspended => {
            state.close_overlay();
            handle_key_action(KeyAction::ResumeSuspended, state)
        }
        PaletteCommand::DiscardSuspended => {
            state.close_overlay();
            handle_key_action(KeyAction::DiscardSuspended, state)
        }
        PaletteCommand::SaveSession => {
            state.close_overlay();
            if state.is_active_run() {
                state.push_timeline_item(TimelineItem::Muted {
                    title: "Save unavailable".to_owned(),
                    detail: "Stop the active run before saving the session.".to_owned(),
                });
                ControllerEffect::None
            } else {
                ControllerEffect::SaveSession
            }
        }
        PaletteCommand::EnterPlanMode
        | PaletteCommand::ApprovePlan
        | PaletteCommand::RevisePlan
        | PaletteCommand::OpenPlan
        | PaletteCommand::FocusPlan
        | PaletteCommand::ClosePlan
        | PaletteCommand::RetryPlanNode
        | PaletteCommand::CancelPlan => unreachable!("plan command handled above"),
        PaletteCommand::Quit => {
            state.close_overlay();
            ControllerEffect::Quit
        }
    }
}
