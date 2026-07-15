use super::plan::*;
use merry_core::{
    PlanActivationSource, PlanApprovalRequirementId, PlanApprovalRequirementKind,
    PlanApprovalRequirementSnapshot, PlanApprovalRequirementStatus, PlanAttemptId,
    PlanAttemptProgressSnapshot, PlanAttemptSnapshot, PlanDirectiveConstraints, PlanDirectiveId,
    PlanDirectiveKind, PlanDirectiveStatus, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId,
    PlanLeaseId, PlanLeaseSnapshot, PlanLeaseStatus, PlanNodeId, PlanNodeSnapshot, PlanNodeStatus,
    PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, PlanSchedulerStatus,
    PlanSnapshot, SubagentActivityPhase, SubagentActivitySnapshot, SubagentId, SubagentTaskId,
    ToolName,
};
use std::path::PathBuf;

use crate::tui::{
    controller::handle_key_event,
    keymap::Keymap,
    overlay::{Overlay, PaletteCommand},
    projector::TuiProjector,
    render::render_to_text,
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn plan_event_updates_tree_without_resetting_selection() {
    let mut state = tui_state();
    let mut projector = TuiProjector::default();
    projector.apply(plan_event(snapshot(1, PlanNodeStatus::Pending)), &mut state);
    state.plan_mut().select_node(node_id("unrelated"));
    state.plan_mut().toggle_collapse(&node_id("active-parent"));
    state.plan_mut().toggle_collapse(&node_id("unrelated"));

    projector.apply(
        plan_event(snapshot(2, PlanNodeStatus::InProgress)),
        &mut state,
    );

    assert_eq!(state.plan().selected_node_id(), Some(&node_id("unrelated")));
    assert!(!state.plan().is_collapsed(&node_id("active-parent")));
    assert!(state.plan().is_collapsed(&node_id("unrelated")));
}

#[test]
fn active_path_is_revealed_without_unfolding_unrelated_branches() {
    let mut state = PlanUiState::default();
    state.update_snapshot(snapshot(1, PlanNodeStatus::Pending));
    state.toggle_collapse(&node_id("active-parent"));
    state.toggle_collapse(&node_id("unrelated"));

    state.update_snapshot(snapshot(2, PlanNodeStatus::Verifying));

    let visible = state
        .visible_rows()
        .into_iter()
        .map(|row| row.node_id)
        .collect::<Vec<_>>();
    assert!(visible.contains(&node_id("active-leaf")));
    assert!(!visible.contains(&node_id("unrelated-leaf")));
}

#[test]
fn plan_pane_derives_live_ready_and_blocked_counts() {
    let mut snapshot = snapshot(1, PlanNodeStatus::InProgress);
    snapshot
        .nodes
        .iter_mut()
        .find(|node| node.id == node_id("active-leaf"))
        .expect("active leaf exists")
        .execution_summary
        .active = 1;
    snapshot.nodes.push(node(
        "ready",
        Some("root"),
        2,
        PlanNodeStatus::Pending,
        Vec::new(),
    ));
    snapshot.nodes.push(node(
        "blocked",
        Some("root"),
        3,
        PlanNodeStatus::Blocked,
        Vec::new(),
    ));
    snapshot.leases.push(merry_core::PlanLeaseSnapshot {
        lease_id: merry_core::PlanLeaseId::new("lease-active").unwrap(),
        attempt_id: merry_core::PlanAttemptId::new("attempt-active").unwrap(),
        node_id: node_id("active-leaf"),
        node_revision: 2,
        executor_session_id: merry_core::SessionId::new("subagent-active").unwrap(),
        started_at_ms: 10,
        last_heartbeat_at_ms: 20,
        lease_expires_at_ms: 30,
        status: merry_core::PlanLeaseStatus::Live,
    });
    snapshot.attempts.push(PlanAttemptSnapshot {
        attempt_id: PlanAttemptId::new("attempt-active").unwrap(),
        node_id: node_id("active-leaf"),
        node_revision: 2,
        lease_id: Some(PlanLeaseId::new("lease-active").unwrap()),
        executor_session_id: merry_core::SessionId::new("subagent-active").unwrap(),
        harness_fingerprint: "active-harness".to_owned(),
        started_at_ms: 10,
        finished_at_ms: None,
        outcome: None,
        result: None,
        diagnostic: None,
        latest_checkpoint_ref: None,
        last_applied_directive_sequence: 0,
    });
    let mut state = PlanUiState::default();
    state.update_snapshot(snapshot);

    assert_eq!(
        state.counts(),
        PlanCounts {
            live: 1,
            ready: 1,
            blocked: 1,
        }
    );
}

#[test]
fn plan_pane_counts_a_live_local_attempt_without_a_lease() {
    let mut snapshot = snapshot(1, PlanNodeStatus::InProgress);
    snapshot.attempts.push(PlanAttemptSnapshot {
        attempt_id: PlanAttemptId::new("attempt-local").unwrap(),
        node_id: node_id("active-leaf"),
        node_revision: 2,
        lease_id: None,
        executor_session_id: merry_core::SessionId::new("coordinator").unwrap(),
        harness_fingerprint: "local-harness".to_owned(),
        started_at_ms: 10,
        finished_at_ms: None,
        outcome: None,
        result: None,
        diagnostic: None,
        latest_checkpoint_ref: None,
        last_applied_directive_sequence: 0,
    });
    let mut state = PlanUiState::default();
    state.update_snapshot(snapshot);

    assert_eq!(
        state.counts().live,
        0,
        "unbound attempts are not Plan links"
    );
}

#[test]
fn subagent_activity_attaches_only_to_its_linked_plan_node() {
    let mut plan = snapshot(1, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    let mut state = PlanUiState::default();
    state.update_subagent_activity(vec![
        activity(
            "subagent-active",
            SubagentActivityPhase::Running,
            "editing the linked file",
            10,
        ),
        activity(
            "unrelated-agent",
            SubagentActivityPhase::Running,
            "should stay hidden",
            20,
        ),
    ]);
    state.update_snapshot(plan);

    let rows = state.visible_rows();
    let active = rows
        .iter()
        .find(|row| row.node_id == node_id("active-leaf"))
        .expect("linked node should be visible");
    let unrelated = rows
        .iter()
        .find(|row| row.node_id == node_id("unrelated"))
        .expect("unrelated node should be visible");
    assert_eq!(
        active
            .activity
            .as_ref()
            .map(|snapshot| snapshot.summary.as_str()),
        Some("editing the linked file")
    );
    assert!(unrelated.activity.is_none());
}

#[test]
fn same_subagent_activity_for_a_different_task_is_not_attached() {
    let mut plan = snapshot(1, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    let mut state = PlanUiState::default();
    state.update_subagent_activity(vec![activity_for_task(
        "subagent-active",
        "task-other",
        SubagentActivityPhase::Running,
        "stale task activity",
        20,
    )]);
    state.update_snapshot(plan);

    let row = state
        .visible_rows()
        .into_iter()
        .find(|row| row.node_id == node_id("active-leaf"))
        .expect("linked node should be visible");
    assert!(row.activity.is_none());
}

#[test]
fn latest_subagent_activity_replaces_the_prior_summary() {
    let mut plan = snapshot(1, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    let mut state = PlanUiState::default();
    state.update_snapshot(plan);
    state.update_subagent_activity(vec![activity(
        "subagent-active",
        SubagentActivityPhase::Running,
        "old summary",
        10,
    )]);
    state.update_subagent_activity(vec![activity(
        "subagent-active",
        SubagentActivityPhase::Waiting,
        "new summary",
        20,
    )]);

    let row = state
        .visible_rows()
        .into_iter()
        .find(|row| row.node_id == node_id("active-leaf"))
        .expect("linked node should be visible");
    assert_eq!(
        row.activity
            .as_ref()
            .map(|activity| activity.summary.as_str()),
        Some("new summary")
    );
    assert_eq!(
        row.activity.as_ref().map(|activity| activity.phase),
        Some(SubagentActivityPhase::Waiting)
    );
}

#[test]
fn terminal_subagent_activity_is_rendered_on_the_plan_node() {
    let mut state = tui_state();
    let mut plan = snapshot(1, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    state.plan_mut().update_snapshot(plan);
    state.plan_mut().update_subagent_activity(vec![activity(
        "subagent-active",
        SubagentActivityPhase::Completed,
        "finished the linked task",
        30,
    )]);

    let rendered = render_to_text(&state, 140, 40);

    assert!(rendered.contains("completed  finished the linked task"));

    state.plan_mut().open_and_focus();
    state.plan_mut().select_node(node_id("active-leaf"));
    state.plan_mut().open_inspector();
    assert!(
        render_to_text(&state, 140, 40).contains("latest: completed  finished the linked task")
    );
}

#[test]
fn no_subagent_activity_preserves_existing_plan_tree_rendering() {
    let mut state = tui_state();
    let mut plan = snapshot(1, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    state.plan_mut().update_snapshot(plan);
    let before = render_to_text(&state, 140, 40);

    state.plan_mut().update_subagent_activity(Vec::new());

    assert_eq!(render_to_text(&state, 140, 40), before);
}

#[test]
fn long_subagent_summary_does_not_change_tree_geometry_or_bottom_panes() {
    let mut state = tui_state();
    let mut plan = snapshot(1, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    state.plan_mut().update_snapshot(plan);
    let node_count = state.plan().visible_rows().len();
    state.plan_mut().update_subagent_activity(vec![activity(
        "subagent-active",
        SubagentActivityPhase::Running,
        &"a very long bounded summary ".repeat(40),
        40,
    )]);

    let rendered = render_to_text(&state, 80, 24);

    assert_eq!(state.plan().visible_rows().len(), node_count);
    assert_eq!(rendered.lines().count(), 24);
    assert!(rendered.contains("Ready"));
    assert!(!rendered.contains(&"a very long bounded summary ".repeat(10)));
}

#[test]
fn wide_tui_renders_timeline_and_recursive_plan_without_overlap() {
    let mut state = tui_state();
    state.push_timeline_item(TimelineItem::Assistant {
        text: "TIMELINE_SENTINEL".to_owned(),
    });
    let mut plan = snapshot(2, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    plan.nodes.push(node(
        "ready",
        Some("root"),
        2,
        PlanNodeStatus::Pending,
        Vec::new(),
    ));
    plan.nodes.push(node(
        "blocked",
        Some("root"),
        3,
        PlanNodeStatus::Blocked,
        Vec::new(),
    ));
    state.plan_mut().update_snapshot(plan);

    let rendered = render_to_text(&state, 140, 40);

    assert!(rendered.contains("TIMELINE_SENTINEL"));
    assert!(rendered.contains("Objective active-parent"));
    assert!(rendered.contains("Objective active-leaf"));
    assert!(rendered.contains("live 1"));
    assert!(rendered.contains("ready 1"));
    assert!(rendered.contains("blocked 1"));
}

#[test]
fn narrow_tui_renders_full_screen_plan_overlay_when_focused() {
    let mut state = tui_state();
    state.push_timeline_item(TimelineItem::Assistant {
        text: "TIMELINE_SENTINEL".to_owned(),
    });
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));
    state.plan_mut().open_and_focus();

    let rendered = render_to_text(&state, 50, 20);

    assert!(!rendered.contains("TIMELINE_SENTINEL"));
    assert!(rendered.contains("Objective active-leaf"));
    assert!(rendered.contains("Ready"));
    assert_eq!(rendered.lines().count(), 20);
}

#[test]
fn standard_tui_renders_timeline_and_plan_side_by_side() {
    let mut state = tui_state();
    state.push_timeline_item(TimelineItem::Assistant {
        text: "TIMELINE_SENTINEL".to_owned(),
    });
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));

    let rendered = render_to_text(&state, 80, 24);

    assert!(rendered.contains("TIMELINE_SENTINEL"));
    assert!(rendered.contains("Objective active-leaf"));
    assert_eq!(rendered.lines().count(), 24);
}

#[test]
fn plan_node_inspector_renders_bounded_content() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));
    state.plan_mut().open_and_focus();
    state.plan_mut().select_node(node_id("active-leaf"));
    state.plan_mut().open_inspector();

    let rendered = render_to_text(&state, 80, 24);

    assert!(rendered.contains("OBJECTIVE"));
    assert!(rendered.contains("ACCEPTANCE"));
    assert!(rendered.contains("Objective active-leaf"));
    assert_eq!(rendered.lines().count(), 24);
}

#[test]
fn focused_plan_navigation_does_not_edit_chat_input() {
    let mut state = tui_state();
    state
        .plan_mut()
        .update_snapshot(snapshot(2, PlanNodeStatus::InProgress));
    state.plan_mut().open_and_focus();

    handle_key_event(key(KeyCode::Down), &mut state);
    assert_eq!(
        state.plan().selected_node_id(),
        Some(&node_id("active-parent"))
    );
    handle_key_event(key(KeyCode::Right), &mut state);
    assert_eq!(
        state.plan().selected_node_id(),
        Some(&node_id("active-leaf"))
    );
    handle_key_event(key(KeyCode::Enter), &mut state);
    assert!(state.plan().is_inspector_open());
    handle_key_event(key(KeyCode::Down), &mut state);
    assert_eq!(state.plan().inspector_scroll_offset(), 1);
    handle_key_event(key(KeyCode::Char('x')), &mut state);
    assert!(state.input_text().is_empty());

    handle_key_event(key(KeyCode::Esc), &mut state);
    assert!(!state.plan().is_inspector_open());
    assert!(state.plan().is_focused());
    handle_key_event(key(KeyCode::Esc), &mut state);
    assert!(!state.plan().is_focused());
    assert!(state.plan().is_open());
}

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
fn projector_opens_execution_review_for_a_non_empty_planning_draft() {
    let mut state = tui_state();
    let mut projector = TuiProjector::default();
    let mut planning = snapshot(1, PlanNodeStatus::Pending);
    planning.phase = PlanPhase::Planning;
    planning.approval_requirements.clear();

    projector.apply(plan_event(planning), &mut state);

    assert!(matches!(state.overlay(), Some(Overlay::PlanApproval(_))));
    let rendered = render_to_text(&state, 80, 24);
    assert!(rendered.contains("Approve plan and execute?"));
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
fn projector_applies_runtime_heartbeat_progress_to_the_live_plan_snapshot() {
    let mut state = tui_state();
    let mut projector = TuiProjector::default();
    let mut plan = snapshot(4, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    plan.attempt_progress[0].elapsed_ms = 0;
    plan.attempt_progress[0].provider_request_in_flight = false;
    plan.attempt_progress[0].last_subagent_heartbeat_at_ms = Some(1_000);
    projector.apply(plan_event(plan), &mut state);

    let mut heartbeat = state.plan().snapshot().unwrap().attempt_progress[0].clone();
    heartbeat.elapsed_ms = 39_600_000;
    heartbeat.provider_request_in_flight = true;
    heartbeat.last_subagent_heartbeat_at_ms = Some(39_600_000);
    projector.apply(
        merry_core::RuntimeEvent::PlanProgressUpdated {
            progress: heartbeat,
            source: merry_core::RuntimeEventSource::new(
                merry_core::SessionId::new("plan-ui-source").unwrap(),
                2,
            ),
        },
        &mut state,
    );
    state.plan_mut().open_and_focus();
    state.plan_mut().select_node(node_id("active-leaf"));
    state.plan_mut().open_inspector();
    state.plan_mut().scroll_inspector_down_by(12);

    let rendered = render_to_text(&state, 140, 40);
    assert!(rendered.contains("elapsed 11h 00m"));
    assert!(rendered.contains("provider request in flight"));
    assert!(rendered.contains("heartbeat @ 39600000 ms"));
}

#[test]
fn ready_counts_match_recursive_execution_shape() {
    let mut state = PlanUiState::default();
    let mut plan = snapshot(1, PlanNodeStatus::Pending);
    plan.nodes[0].status = PlanNodeStatus::Pending;
    plan.nodes[1].status = PlanNodeStatus::Pending;
    plan.nodes[2].status = PlanNodeStatus::Pending;
    plan.nodes[3].status = PlanNodeStatus::Superseded;
    plan.nodes[4].status = PlanNodeStatus::Superseded;
    state.update_snapshot(plan.clone());
    assert_eq!(state.counts().ready, 1, "only the pending leaf is ready");

    plan.nodes[0].status = PlanNodeStatus::Verifying;
    plan.nodes[1].status = PlanNodeStatus::Completed;
    plan.nodes[2].status = PlanNodeStatus::Completed;
    state.update_snapshot(plan);
    assert_eq!(
        state.counts().ready,
        1,
        "the verifying parent becomes ready"
    );
}

#[test]
fn superseded_nodes_are_hidden_from_the_current_plan_tree() {
    let mut state = tui_state();
    let mut plan = snapshot(4, PlanNodeStatus::Pending);
    plan.nodes.push(node(
        "old-child",
        Some("root"),
        0,
        PlanNodeStatus::Superseded,
        Vec::new(),
    ));

    state.plan_mut().update_snapshot(plan);

    assert!(
        state
            .plan()
            .visible_rows()
            .iter()
            .all(|row| row.node_id != node_id("old-child"))
    );
    assert!(!render_to_text(&state, 140, 40).contains("Objective old-child"));
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

#[test]
fn long_running_node_renders_progress_without_fake_deadline() {
    let mut state = tui_state();
    let mut plan = snapshot(4, PlanNodeStatus::InProgress);
    add_live_attempt(&mut plan);
    state.plan_mut().update_snapshot(plan);
    state.plan_mut().open_and_focus();
    state.plan_mut().select_node(node_id("active-leaf"));
    state.plan_mut().open_inspector();
    state.plan_mut().scroll_inspector_down_by(12);

    let rendered = render_to_text(&state, 140, 40);

    assert!(rendered.contains("elapsed 11h 00m"));
    assert!(rendered.contains("provider request in flight"));
    assert!(rendered.contains("durable progress"));
    assert!(!rendered.to_ascii_lowercase().contains("remaining"));
    assert!(!rendered.to_ascii_lowercase().contains("deadline"));
}

fn add_live_attempt(snapshot: &mut PlanSnapshot) {
    let attempt_id = PlanAttemptId::new("attempt-active").unwrap();
    let lease_id = PlanLeaseId::new("lease-active").unwrap();
    snapshot.attempts.push(PlanAttemptSnapshot {
        attempt_id: attempt_id.clone(),
        node_id: node_id("active-leaf"),
        node_revision: 2,
        lease_id: Some(lease_id.clone()),
        executor_session_id: merry_core::SessionId::new("subagent-active").unwrap(),
        harness_fingerprint: "harness-active".to_owned(),
        started_at_ms: 1_000,
        finished_at_ms: None,
        outcome: None,
        result: None,
        diagnostic: None,
        latest_checkpoint_ref: Some("checkpoint-17".to_owned()),
        last_applied_directive_sequence: 0,
    });
    snapshot.leases.push(PlanLeaseSnapshot {
        lease_id: lease_id.clone(),
        attempt_id: attempt_id.clone(),
        node_id: node_id("active-leaf"),
        node_revision: 2,
        executor_session_id: merry_core::SessionId::new("subagent-active").unwrap(),
        started_at_ms: 1_000,
        last_heartbeat_at_ms: 39_600_000,
        lease_expires_at_ms: 39_630_000,
        status: PlanLeaseStatus::Live,
    });
    snapshot.attempt_progress.push(PlanAttemptProgressSnapshot {
        attempt_id: attempt_id.clone(),
        node_id: node_id("active-leaf"),
        elapsed_ms: 39_600_000,
        model_turns: 42,
        reported_usage: None,
        last_subagent_heartbeat_at_ms: Some(39_600_000),
        last_runtime_activity_at_ms: 39_600_000,
        last_durable_progress_at_ms: Some(39_540_000),
        provider_request_in_flight: true,
        tool_call_in_flight: false,
        observable_side_effects: 0,
        artifacts_created: 7,
        artifact_refs: Vec::new(),
        changed_paths: vec!["crates/merry-runtime/src/plan.rs".to_owned()],
        acceptance_evidence: Vec::new(),
        repeated_failure_fingerprint: None,
        summary: Some("Acceptance fixtures are still advancing".to_owned()),
        next_action: Some("finish the final deterministic fixture".to_owned()),
        request_coordinator_review: true,
    });
    let plan_id = snapshot.plan_id.clone();
    if let Some(node) = snapshot
        .nodes
        .iter_mut()
        .find(|node| node.id == node_id("active-leaf"))
    {
        node.execution_summary.active = 1;
        node.links.push(merry_core::PlanLinkSnapshot {
            plan_id,
            node_id: node.id.clone(),
            binding_id: merry_core::PlanBindingId::new("binding-active").unwrap(),
            subagent_id: merry_core::SubagentId::new("subagent-active").unwrap(),
            task_id: merry_core::SubagentTaskId::new("task-active").unwrap(),
            status: merry_core::PlanLinkStatus::Active,
            linked_at_ms: 1_000,
            terminal_at_ms: None,
            superseded_by: None,
        });
    }
    snapshot
        .directives
        .push(merry_core::CoordinatorDirectiveSnapshot {
            directive_id: PlanDirectiveId::new("directive-converge").unwrap(),
            sequence: 1,
            plan_id: snapshot.plan_id.clone(),
            node_id: node_id("active-leaf"),
            node_revision: 2,
            attempt_id,
            lease_id,
            kind: PlanDirectiveKind::Converge,
            reason: "The acceptance target is already clear".to_owned(),
            instruction: Some("Finish the fixture and report evidence".to_owned()),
            constraints: PlanDirectiveConstraints::default(),
            requested_output: vec!["verification evidence".to_owned()],
            issued_at_ms: 39_600_000,
            status: PlanDirectiveStatus::Queued,
            delivered_at_ms: None,
            acknowledged_at_ms: None,
            applied_at_ms: None,
        });
}

fn plan_commands(state: &mut TuiState) -> Vec<PaletteCommand> {
    state.open_command_palette();
    let commands = match state.overlay().unwrap() {
        Overlay::CommandPalette(palette) => palette
            .visible_commands()
            .into_iter()
            .filter(|command| command.category == "Plan")
            .map(|command| command.command)
            .collect(),
        _ => panic!("expected command palette"),
    };
    state.close_overlay();
    commands
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn tui_state() -> TuiState {
    TuiState::new(
        PathBuf::from("/workspace/merry"),
        "model-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn plan_event(snapshot: PlanSnapshot) -> merry_core::RuntimeEvent {
    let summary = merry_core::PlanRevisionSummary::new(
        snapshot.revision,
        &format!("plan revision {}", snapshot.revision),
    )
    .unwrap();
    merry_core::RuntimeEvent::PlanUpdated {
        snapshot,
        summary,
        source: merry_core::RuntimeEventSource::new(
            merry_core::SessionId::new("plan-ui-source").unwrap(),
            1,
        ),
    }
}

fn snapshot(revision: u64, active_leaf_status: PlanNodeStatus) -> PlanSnapshot {
    PlanSnapshot {
        plan_id: PlanId::new("plan-ui").unwrap(),
        revision,
        phase: PlanPhase::Executing,
        activation_source: PlanActivationSource::User,
        root_node_id: Some(node_id("root")),
        coordinator_node_id: Some(node_id("root")),
        execution_contract_fingerprint: Some("contract".to_owned()),
        execution_authorization_refs: Vec::new(),
        authorized_capability_envelope: None,
        approval_requirements: Vec::new(),
        nodes: vec![
            node("root", None, 0, PlanNodeStatus::Expanded, Vec::new()),
            node(
                "active-parent",
                Some("root"),
                0,
                PlanNodeStatus::Expanded,
                Vec::new(),
            ),
            node(
                "active-leaf",
                Some("active-parent"),
                0,
                active_leaf_status,
                Vec::new(),
            ),
            node(
                "unrelated",
                Some("root"),
                1,
                PlanNodeStatus::Expanded,
                Vec::new(),
            ),
            node(
                "unrelated-leaf",
                Some("unrelated"),
                0,
                PlanNodeStatus::Pending,
                vec![node_id("active-leaf")],
            ),
        ],
        attempts: Vec::new(),
        leases: Vec::new(),
        attempt_progress: Vec::new(),
        directives: Vec::new(),
        resource_policy_snapshot: PlanResourcePolicySnapshot::default(),
        max_concurrency_hint: Some(3),
        scheduler_status: PlanSchedulerStatus::Active,
        revision_summaries: Vec::new(),
    }
}

fn node(
    id: &str,
    parent_id: Option<&str>,
    sibling_order: u16,
    status: PlanNodeStatus,
    depends_on: Vec<PlanNodeId>,
) -> PlanNodeSnapshot {
    PlanNodeSnapshot {
        id: node_id(id),
        client_key: None,
        parent_id: parent_id.map(node_id),
        sibling_order,
        objective: format!("Objective {id}"),
        acceptance: vec![format!("Accept {id}")],
        status,
        executor_policy: PlanExecutorPolicy::Auto,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on,
        result: None,
        created_revision: 1,
        updated_revision: 1,
        declared_status: status,
        execution_summary: Default::default(),
        links: Vec::new(),
    }
}

fn node_id(value: &str) -> PlanNodeId {
    PlanNodeId::new(value).unwrap()
}

fn activity(
    subagent_id: &str,
    phase: SubagentActivityPhase,
    summary: &str,
    updated_at_ms: u64,
) -> SubagentActivitySnapshot {
    activity_for_task(subagent_id, "task-active", phase, summary, updated_at_ms)
}

fn activity_for_task(
    subagent_id: &str,
    task_id: &str,
    phase: SubagentActivityPhase,
    summary: &str,
    updated_at_ms: u64,
) -> SubagentActivitySnapshot {
    SubagentActivitySnapshot {
        subagent_id: SubagentId::new(subagent_id).unwrap(),
        task_id: SubagentTaskId::new(task_id).unwrap(),
        phase,
        summary: summary.to_owned(),
        updated_at_ms,
    }
}
