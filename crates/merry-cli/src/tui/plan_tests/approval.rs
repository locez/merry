use crate::tui::{
    controller::handle_key_event,
    overlay::{Overlay, PaletteCommand},
    plan_tests::{add_live_attempt, key, node_id, plan_commands, plan_event, snapshot, tui_state},
    projector::TuiProjector,
    render::render_to_text,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    PlanApprovalRequirementId, PlanApprovalRequirementKind, PlanApprovalRequirementSnapshot,
    PlanApprovalRequirementStatus, PlanNodeStatus, PlanPhase, ToolName,
};

#[test]
fn plan_palette_commands_follow_runtime_phase() {
    let mut state = tui_state();
    assert_eq!(
        plan_commands(&mut state),
        vec![PaletteCommand::EnterPlanMode]
    );

    let mut planning = snapshot(1, PlanNodeStatus::Pending);
    planning.phase = PlanPhase::Planning;
    state.plan_mut().update_snapshot(planning);
    let commands = plan_commands(&mut state);
    assert!(commands.contains(&PaletteCommand::ApprovePlan));

    let mut awaiting = snapshot(2, PlanNodeStatus::Pending);
    awaiting.phase = PlanPhase::AwaitingApproval;
    state.plan_mut().update_snapshot(awaiting);
    let commands = plan_commands(&mut state);
    assert!(commands.contains(&PaletteCommand::ApprovePlan));
    assert!(commands.contains(&PaletteCommand::RevisePlan));
    assert!(commands.contains(&PaletteCommand::FocusPlan));
    assert!(commands.contains(&PaletteCommand::ClosePlan));
    assert!(!commands.contains(&PaletteCommand::EnterPlanMode));

    let executing = snapshot(3, PlanNodeStatus::InProgress);
    state.plan_mut().update_snapshot(executing.clone());
    let commands = plan_commands(&mut state);
    assert!(!commands.contains(&PaletteCommand::RetryPlanNode));

    let mut blocked = snapshot(4, PlanNodeStatus::Blocked);
    blocked.phase = PlanPhase::Blocked;
    state.plan_mut().update_snapshot(blocked);
    let commands = plan_commands(&mut state);
    assert!(commands.contains(&PaletteCommand::EnterPlanMode));
    assert!(!commands.contains(&PaletteCommand::RevisePlan));
    assert!(!commands.contains(&PaletteCommand::CancelPlan));

    let mut completed = snapshot(5, PlanNodeStatus::Completed);
    completed.phase = PlanPhase::Completed;
    state.plan_mut().update_snapshot(completed);
    assert!(plan_commands(&mut state).contains(&PaletteCommand::EnterPlanMode));

    let mut cancelled = snapshot(6, PlanNodeStatus::Blocked);
    cancelled.phase = PlanPhase::Cancelled;
    state.plan_mut().update_snapshot(cancelled);
    assert!(plan_commands(&mut state).contains(&PaletteCommand::EnterPlanMode));
}

#[test]
fn approve_plan_command_previews_exact_capability_scope_before_dispatch() {
    let mut state = tui_state();
    let mut awaiting = snapshot(2, PlanNodeStatus::Pending);
    awaiting.phase = PlanPhase::AwaitingApproval;
    awaiting.nodes[0].harness.allowed_tools = vec![ToolName::new("run_process").unwrap()];
    awaiting.nodes[0].harness.write_scope = vec!["crates/merry-runtime".to_owned()];
    awaiting.approval_requirements = vec![PlanApprovalRequirementSnapshot {
        requirement_id: PlanApprovalRequirementId::new("approval-permission").unwrap(),
        kind: PlanApprovalRequirementKind::CapabilityOrPermissionExpansion,
        status: PlanApprovalRequirementStatus::Pending,
        created_revision: 2,
        resolution_ref: None,
    }];
    state.plan_mut().update_snapshot(awaiting);
    state.open_command_palette();
    for character in "approve plan".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }

    let first = handle_key_event(key(KeyCode::Enter), &mut state);
    assert_eq!(first, crate::tui::controller::ControllerEffect::None);
    assert!(matches!(state.overlay(), Some(Overlay::PlanApproval(_))));
    let rendered = render_to_text(&state, 80, 24);
    assert!(rendered.contains("Tools: run_process"));
    assert!(rendered.contains("Write scope: crates/merry-runtime"));
    assert!(rendered.contains("capability or permission expansion"));

    let confirmed = handle_key_event(key(KeyCode::Enter), &mut state);
    assert!(matches!(
        confirmed,
        crate::tui::controller::ControllerEffect::ApprovePlan(_)
    ));
}

#[test]
fn approval_uses_root_forbidden_paths_not_stricter_child_only_paths() {
    let mut state = tui_state();
    let mut awaiting = snapshot(2, PlanNodeStatus::Pending);
    awaiting.phase = PlanPhase::AwaitingApproval;
    awaiting.nodes[0].harness.forbidden_paths = vec![".git".to_owned()];
    awaiting.nodes[1].harness.forbidden_paths =
        vec![".git".to_owned(), "private-child-cache".to_owned()];
    state.plan_mut().update_snapshot(awaiting);

    let input = state
        .plan()
        .approval_input()
        .expect("valid recursive plan has approval material");
    assert_eq!(
        input
            .capability_envelope
            .expect("approval carries an envelope")
            .forbidden_paths,
        vec![".git"]
    );
}

#[test]
fn projector_opens_plan_approval_when_runtime_enters_awaiting_approval() {
    let mut state = tui_state();
    let mut projector = TuiProjector::default();
    let mut planning = snapshot(1, PlanNodeStatus::Pending);
    planning.phase = PlanPhase::Planning;
    planning.root_node_id = None;
    planning.nodes.clear();
    planning.approval_requirements.clear();
    projector.apply(plan_event(planning), &mut state);
    assert!(state.overlay().is_none());

    let mut awaiting = snapshot(2, PlanNodeStatus::Pending);
    awaiting.phase = PlanPhase::AwaitingApproval;
    awaiting.approval_requirements = vec![PlanApprovalRequirementSnapshot {
        requirement_id: PlanApprovalRequirementId::new("approval-user-review").unwrap(),
        kind: PlanApprovalRequirementKind::UserReviewRequested,
        status: PlanApprovalRequirementStatus::Pending,
        created_revision: 2,
        resolution_ref: None,
    }];
    projector.apply(plan_event(awaiting), &mut state);

    assert!(matches!(state.overlay(), Some(Overlay::PlanApproval(_))));
    let rendered = render_to_text(&state, 80, 24);
    assert!(rendered.contains("Approve plan and execute?"));
    assert!(rendered.contains("user review"));
    assert!(matches!(
        handle_key_event(key(KeyCode::Enter), &mut state),
        crate::tui::controller::ControllerEffect::ApprovePlan(_)
    ));
}

#[test]
fn projector_refreshes_an_open_approval_when_the_plan_revision_changes() {
    let mut state = tui_state();
    let mut projector = TuiProjector::default();
    let mut first = snapshot(1, PlanNodeStatus::Pending);
    first.phase = PlanPhase::Planning;
    projector.apply(plan_event(first), &mut state);
    assert!(render_to_text(&state, 80, 24).contains("Plan revision 1"));

    let mut second = snapshot(2, PlanNodeStatus::Pending);
    second.phase = PlanPhase::Planning;
    second.nodes[0].harness.allowed_tools = vec![ToolName::new("run_process").unwrap()];
    projector.apply(plan_event(second), &mut state);

    let rendered = render_to_text(&state, 80, 24);
    assert!(rendered.contains("Plan revision 2"));
    assert!(rendered.contains("Tools: run_process"));
    assert!(!rendered.contains("Plan revision 1"));
}

#[test]
fn plan_inspector_renders_approval_requirements_and_directive_status() {
    let mut state = tui_state();
    let mut plan = snapshot(4, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    plan.approval_requirements = vec![PlanApprovalRequirementSnapshot {
        requirement_id: PlanApprovalRequirementId::new("approval-review").unwrap(),
        kind: PlanApprovalRequirementKind::UserReviewRequested,
        status: PlanApprovalRequirementStatus::Pending,
        created_revision: 4,
        resolution_ref: None,
    }];
    state.plan_mut().update_snapshot(plan);
    state.plan_mut().open_and_focus();
    state.plan_mut().select_node(node_id("active-leaf"));
    state.plan_mut().open_inspector();
    state.plan_mut().scroll_inspector_down_by(18);

    let rendered = render_to_text(&state, 140, 40);

    assert!(rendered.contains("DIRECTIVES"));
    assert!(rendered.contains("converge"));
    assert!(rendered.contains("queued"));
    assert!(rendered.contains("APPROVALS"));
    assert!(rendered.contains("user review"));
}
