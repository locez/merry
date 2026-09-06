use crate::tui::{
    overlay::Overlay,
    plan::{PlanCounts, PlanUiState},
    plan_tests::{
        activity, activity_for_task, add_live_attempt, node, node_id, plan_event, snapshot,
        tui_state,
    },
    projector::TuiProjector,
    render::render_to_text,
};
use merry_core::{
    PlanAttemptId, PlanAttemptSnapshot, PlanLeaseId, PlanNodeStatus, PlanPhase,
    SubagentActivityPhase,
};

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
