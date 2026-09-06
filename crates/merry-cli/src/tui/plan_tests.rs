use crate::tui::{
    keymap::Keymap,
    overlay::{Overlay, PaletteCommand},
    state::TuiState,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    PlanActivationSource, PlanAttemptId, PlanAttemptProgressSnapshot, PlanAttemptSnapshot,
    PlanDirectiveConstraints, PlanDirectiveId, PlanDirectiveKind, PlanDirectiveStatus,
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanLeaseId, PlanLeaseSnapshot,
    PlanLeaseStatus, PlanNodeId, PlanNodeSnapshot, PlanNodeStatus, PlanPhase,
    PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, PlanSchedulerStatus, PlanSnapshot,
    SubagentActivityPhase, SubagentActivitySnapshot, SubagentId, SubagentTaskId,
};
use std::path::PathBuf;

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

fn key_ctrl(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
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

mod approval;

mod interaction;

mod projection;
