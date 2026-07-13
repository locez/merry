use super::{
    controller::ControllerEffect, overlay::PaletteCommand, runtime::TuiRuntimeSession,
    state::TuiState,
};
use crossterm::event::{KeyCode, KeyEvent};

const PLAN_SCROLL_STEP: usize = 5;

pub(crate) fn handle_navigation_key(key: KeyEvent, state: &mut TuiState) -> bool {
    match key.code {
        KeyCode::Up => {
            if state.plan().is_inspector_open() {
                state.plan_mut().scroll_inspector_up_by(1);
            } else {
                state.plan_mut().select_previous();
            }
        }
        KeyCode::Down => {
            if state.plan().is_inspector_open() {
                state.plan_mut().scroll_inspector_down_by(1);
            } else {
                state.plan_mut().select_next();
            }
        }
        KeyCode::PageUp => {
            if state.plan().is_inspector_open() {
                state.plan_mut().scroll_inspector_up_by(PLAN_SCROLL_STEP);
            } else {
                state.plan_mut().scroll_up_by(PLAN_SCROLL_STEP);
            }
        }
        KeyCode::PageDown => {
            if state.plan().is_inspector_open() {
                state.plan_mut().scroll_inspector_down_by(PLAN_SCROLL_STEP);
            } else {
                state.plan_mut().scroll_down_by(PLAN_SCROLL_STEP);
            }
        }
        KeyCode::Left => {
            if !state.plan_mut().close_inspector() {
                state.plan_mut().select_parent_or_collapse();
            }
        }
        KeyCode::Right => {
            if !state.plan().is_inspector_open() {
                state.plan_mut().select_child_or_expand();
            }
        }
        KeyCode::Enter => {
            state.plan_mut().open_inspector();
        }
        KeyCode::Esc => {
            if !state.plan_mut().close_inspector() {
                state.plan_mut().leave_focus();
            }
        }
        _ => return false,
    }
    true
}

pub(crate) fn palette_effect(
    command: PaletteCommand,
    state: &mut TuiState,
) -> Option<ControllerEffect> {
    let effect = match command {
        PaletteCommand::EnterPlanMode => {
            state.close_overlay();
            ControllerEffect::EnterPlanMode
        }
        PaletteCommand::ApprovePlan => {
            state.open_plan_approval();
            ControllerEffect::None
        }
        PaletteCommand::RevisePlan => {
            state.close_overlay();
            ControllerEffect::RevisePlan
        }
        PaletteCommand::OpenPlan | PaletteCommand::FocusPlan => {
            state.close_overlay();
            state.plan_mut().open_and_focus();
            ControllerEffect::None
        }
        PaletteCommand::ClosePlan => {
            state.close_overlay();
            state.plan_mut().close();
            ControllerEffect::None
        }
        PaletteCommand::PausePlan => {
            state.close_overlay();
            ControllerEffect::PausePlan
        }
        PaletteCommand::ResumePlan => {
            state.close_overlay();
            ControllerEffect::ResumePlan
        }
        PaletteCommand::RetryPlanNode => {
            state.close_overlay();
            state.plan().selected_node_id().cloned().map_or_else(
                || {
                    state.show_error_dialog(
                        "Plan retry unavailable",
                        "No plan node is selected.".to_owned(),
                    );
                    ControllerEffect::None
                },
                ControllerEffect::RetryPlanNode,
            )
        }
        PaletteCommand::CancelPlan => {
            state.close_overlay();
            ControllerEffect::CancelPlan
        }
        _ => return None,
    };
    Some(effect)
}

pub(crate) async fn dispatch_effect(
    effect: &ControllerEffect,
    session: &mut TuiRuntimeSession,
    state: &mut TuiState,
) -> Option<bool> {
    match effect {
        ControllerEffect::EnterPlanMode => {
            if let Err(error) = session
                .control
                .enter_plan_mode("user entered Plan Mode from the TUI")
                .await
            {
                state.show_error_dialog("Plan control failed", error.to_string());
            }
        }
        ControllerEffect::ApprovePlan => {
            let input = match state.plan().approval_input() {
                Ok(input) => input,
                Err(error) => {
                    state.show_error_dialog("Plan approval unavailable", error);
                    return Some(false);
                }
            };
            if let Err(error) = session.control.approve_plan(input).await {
                state.show_error_dialog("Plan approval failed", error.to_string());
            }
        }
        ControllerEffect::RevisePlan => {
            if let Err(error) = session
                .control
                .revise_plan("user requested plan revision from the TUI")
                .await
            {
                state.show_error_dialog("Plan revision failed", error.to_string());
            }
        }
        ControllerEffect::PausePlan => {
            if let Err(error) = session
                .control
                .pause_plan_scheduling("user paused plan scheduling from the TUI")
                .await
            {
                state.show_error_dialog("Plan pause failed", error.to_string());
            }
        }
        ControllerEffect::ResumePlan => {
            if let Err(error) = session
                .control
                .resume_plan_scheduling("user resumed plan scheduling from the TUI")
                .await
            {
                state.show_error_dialog("Plan resume failed", error.to_string());
            }
        }
        ControllerEffect::RetryPlanNode(node_id) => {
            if let Err(error) = session
                .control
                .retry_interrupted_plan_node(
                    node_id.clone(),
                    "user explicitly retried interrupted plan work from the TUI",
                )
                .await
            {
                state.show_error_dialog("Plan retry failed", error.to_string());
            }
        }
        ControllerEffect::CancelPlan => {
            if let Err(error) = session
                .control
                .cancel_plan("user cancelled the plan from the TUI")
                .await
            {
                state.show_error_dialog("Plan cancellation failed", error.to_string());
            }
        }
        _ => return None,
    }
    Some(false)
}
