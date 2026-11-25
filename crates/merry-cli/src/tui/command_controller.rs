use super::{
    command::{self, PaletteCommand, SlashCommandMatch},
    controller::{ControllerEffect, handle_key_action},
    input::TuiSubmission,
    keymap::KeyAction,
    state::{TimelineItem, TuiState},
};
use merry::profiles::CodingWorkflowCommand;

pub(crate) fn slash_input_effect(state: &mut TuiState) -> Option<ControllerEffect> {
    let raw_input = state.plain_input_text().map(str::to_owned)?;
    let matched = command::match_slash_input(&raw_input);
    match matched {
        SlashCommandMatch::NotCommand => None,
        SlashCommandMatch::Known(command, args) => {
            let owned_args = args.map(str::to_owned);
            state.clear_input();
            match command.command {
                PaletteCommand::TogglePlanMode if owned_args.is_some() => {
                    state.close_overlay();
                    state.follow_latest_if_detached();
                    Some(ControllerEffect::SetPlanMode {
                        enabled: true,
                        initial_prompt: owned_args,
                    })
                }
                PaletteCommand::CompactContext => Some(compact_effect(state, owned_args)),
                PaletteCommand::CodeReview => Some(workflow_submit_effect(
                    state,
                    CodingWorkflowCommand::CodeReview,
                    owned_args.as_deref(),
                    raw_input.trim().to_owned(),
                )),
                PaletteCommand::SecurityReview => Some(workflow_submit_effect(
                    state,
                    CodingWorkflowCommand::SecurityReview,
                    owned_args.as_deref(),
                    raw_input.trim().to_owned(),
                )),
                _ => Some(run_palette_command(command.command, state)),
            }
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
            prepare_timeline_feedback(state);
            state.close_completion_menu();
            state.push_timeline_item(TimelineItem::Muted {
                title: "Unknown command".to_owned(),
                detail,
            });
            Some(ControllerEffect::None)
        }
        SlashCommandMatch::ArgumentsNotSupported(name) => {
            let detail = format!("/{name} does not accept arguments.");
            prepare_timeline_feedback(state);
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
        PaletteCommand::SwitchModel => {
            state.close_overlay();
            if let Some(alias) = state.active_or_first_provider_alias() {
                ControllerEffect::OpenTargetedModelPicker {
                    alias,
                    target: crate::tui::provider_selection::ModelPickerTarget::QuickSwitch,
                }
            } else {
                ControllerEffect::OpenProviderManager
            }
        }
        PaletteCommand::OpenSkills => {
            state.close_overlay();
            if !state.open_skills_picker() {
                prepare_timeline_feedback(state);
                state.push_timeline_item(TimelineItem::LocalCommand {
                    title: "Skills".to_owned(),
                    body: "No skills loaded. Add SKILL.md files under a configured [skills] root (see config.toml), or reference skills inline with $skill-name.".to_owned(),
                });
            }
            ControllerEffect::None
        }
        PaletteCommand::OpenProviders => ControllerEffect::OpenProviderManager,
        PaletteCommand::ShowShortcuts => {
            state.close_overlay();
            state.open_settings_category(crate::tui::settings_overlay::SettingsCategory::Shortcuts);
            ControllerEffect::None
        }
        PaletteCommand::ShowHelp => {
            prepare_timeline_feedback(state);
            let body = command::slash_help_body(state.keymap());
            state.push_timeline_item(TimelineItem::LocalCommand {
                title: "Command help".to_owned(),
                body,
            });
            ControllerEffect::None
        }
        PaletteCommand::ShowStatus => {
            prepare_timeline_feedback(state);
            let body = state.command_status_body();
            state.push_timeline_item(TimelineItem::LocalCommand {
                title: "Session status".to_owned(),
                body,
            });
            ControllerEffect::None
        }
        PaletteCommand::ShowTasks => {
            prepare_timeline_feedback(state);
            ControllerEffect::ShowTasks
        }
        #[cfg(feature = "web")]
        PaletteCommand::OpenSessionInBrowser => {
            state.close_overlay();
            handle_key_action(KeyAction::OpenSessionInBrowser, state)
        }
        PaletteCommand::ReviewPreviousUserInput => {
            state.close_overlay();
            handle_key_action(KeyAction::ReviewPreviousUserInput, state)
        }
        PaletteCommand::JumpToLatestResponse => {
            state.close_overlay();
            handle_key_action(KeyAction::JumpToLatestResponse, state)
        }
        PaletteCommand::OpenCommandDetails => {
            handle_key_action(KeyAction::OpenCommandDetails, state)
        }
        PaletteCommand::FollowLatest => {
            state.close_overlay();
            state.follow_latest();
            ControllerEffect::None
        }
        PaletteCommand::Interrupt => {
            prepare_timeline_feedback(state);
            if state.can_interrupt_run() {
                state.begin_stop_feedback();
                ControllerEffect::Interrupt
            } else if state.is_interrupting() {
                state.repeat_stop_feedback();
                ControllerEffect::None
            } else {
                state.push_timeline_item(TimelineItem::LocalCommand {
                    title: "Nothing to stop".to_owned(),
                    body: "No model or tool run is active.".to_owned(),
                });
                ControllerEffect::None
            }
        }
        PaletteCommand::NewSession => {
            prepare_timeline_feedback(state);
            if state.is_active_run() {
                state.push_timeline_item(TimelineItem::LocalCommand {
                    title: "New session unavailable".to_owned(),
                    body: "Stop the active run before starting a new session.".to_owned(),
                });
                ControllerEffect::None
            } else {
                ControllerEffect::NewSession
            }
        }
        PaletteCommand::ResumeSession => {
            prepare_timeline_feedback(state);
            if state.is_active_run() {
                state.push_timeline_item(TimelineItem::LocalCommand {
                    title: "Resume unavailable".to_owned(),
                    body: "Stop the active run before switching sessions.".to_owned(),
                });
                ControllerEffect::None
            } else {
                ControllerEffect::ResumeSession
            }
        }
        PaletteCommand::CompactContext => compact_effect(state, None),
        PaletteCommand::CopyLastResponse => copy_last_response_effect(state),
        PaletteCommand::SaveSession => {
            prepare_timeline_feedback(state);
            if state.is_active_run() {
                state.push_timeline_item(TimelineItem::LocalCommand {
                    title: "Save unavailable".to_owned(),
                    body: "Stop the active run before saving the session.".to_owned(),
                });
                ControllerEffect::None
            } else {
                ControllerEffect::SaveSession
            }
        }
        PaletteCommand::CodeReview => workflow_submit_effect(
            state,
            CodingWorkflowCommand::CodeReview,
            None,
            format!("/{}", CodingWorkflowCommand::CodeReview.name()),
        ),
        PaletteCommand::SecurityReview => workflow_submit_effect(
            state,
            CodingWorkflowCommand::SecurityReview,
            None,
            format!("/{}", CodingWorkflowCommand::SecurityReview.name()),
        ),
        PaletteCommand::TogglePlanMode | PaletteCommand::ViewPlans => {
            unreachable!("plan command handled above")
        }
        PaletteCommand::Quit => {
            state.close_overlay();
            ControllerEffect::Quit
        }
    }
}

fn copy_last_response_effect(state: &mut TuiState) -> ControllerEffect {
    state.close_overlay();
    match state.latest_assistant_response() {
        Some(text) => match crate::tui::copy_controls::validate_copy_text(text) {
            Ok(valid) => ControllerEffect::CopyText(valid),
            Err(crate::tui::copy_controls::CopyTextError::TooLarge) => {
                ControllerEffect::CopyTextTooLarge
            }
        },
        None => {
            state.show_clipboard_feedback(
                "Nothing to copy: no assistant response yet".to_owned(),
                true,
            );
            ControllerEffect::None
        }
    }
}

fn compact_effect(state: &mut TuiState, focus: Option<String>) -> ControllerEffect {
    prepare_timeline_feedback(state);
    if state.is_active_run() {
        state.push_timeline_item(TimelineItem::LocalCommand {
            title: "Compaction unavailable".to_owned(),
            body: "Stop the active run before compacting context.".to_owned(),
        });
        ControllerEffect::None
    } else {
        ControllerEffect::CompactContext { focus }
    }
}

fn workflow_submit_effect(
    state: &mut TuiState,
    workflow: CodingWorkflowCommand,
    scope: Option<&str>,
    history_text: String,
) -> ControllerEffect {
    prepare_timeline_feedback(state);
    ControllerEffect::Submit(TuiSubmission {
        text: workflow.build_prompt(scope),
        history_text,
        images: Vec::new(),
    })
}

fn prepare_timeline_feedback(state: &mut TuiState) {
    state.close_overlay();
    state.follow_latest();
}
