//! Plan Mode & Plan Review command/effect translation for the TUI controller.

use super::{
    controller::ControllerEffect,
    overlay::PaletteCommand,
    runtime::TuiRuntimeSession,
    state::{TimelineItem, TuiState},
};

pub(crate) fn palette_effect(
    command: PaletteCommand,
    state: &mut TuiState,
) -> Option<ControllerEffect> {
    let effect = match command {
        PaletteCommand::TogglePlanMode => {
            state.close_overlay();
            ControllerEffect::SetPlanMode {
                enabled: !state.plan().is_plan_mode_on(),
                initial_prompt: None,
            }
        }
        PaletteCommand::ViewPlans => {
            state.close_overlay();
            if state.plan().has_plans() {
                state.open_plan_view(None);
            } else {
                state.push_timeline_item(TimelineItem::Muted {
                    title: "Plans".to_owned(),
                    detail:
                        "No plans in this session yet. Use /plan or Shift+Tab to enter Plan Mode."
                            .to_owned(),
                });
            }
            ControllerEffect::None
        }
        _ => return None,
    };
    Some(effect)
}

pub(crate) async fn dispatch_effect(
    effect: &ControllerEffect,
    session: &mut TuiRuntimeSession,
    state: &mut TuiState,
) -> bool {
    match effect {
        ControllerEffect::SetPlanMode {
            enabled,
            initial_prompt,
        } => {
            if let Err(error) = session.control.set_plan_mode(*enabled).await {
                state.show_error_dialog("Plan Mode failed", error.to_string());
                return true;
            }
            if let Some(prompt) = initial_prompt
                && let Ok(message) = merry_runtime::UserMessageInput::text_only(prompt)
            {
                if let Err(error) = session.submit_message(message, prompt).await {
                    state.show_error_dialog("Plan prompt failed", error.to_string());
                } else if !state.is_active_run() && !state.queue_preview().paused {
                    state.push_local_user_echo(prompt.clone());
                    state.project_local_run_start();
                }
            }
        }
        ControllerEffect::ApprovePlan { plan_id, note } => {
            if let Err(error) = session
                .control
                .approve_plan(plan_id.clone(), note.clone())
                .await
            {
                state.show_error_dialog("Plan approval failed", error.to_string());
            }
        }
        ControllerEffect::RequestPlanChanges { plan_id, feedback } => {
            if let Err(error) = session
                .control
                .request_plan_changes(plan_id.clone(), feedback.clone())
                .await
            {
                state.show_error_dialog("Plan revision failed", error.to_string());
            }
        }
        ControllerEffect::AbandonPlan(plan_id) => {
            if let Err(error) = session.control.abandon_plan(plan_id.clone()).await {
                state.show_error_dialog("Plan abandon failed", error.to_string());
            }
        }
        _ => return false,
    }
    true
}
