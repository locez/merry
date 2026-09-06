use crate::{
    UserImageInput, UserMessageInput,
    plan::{PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanState, UpdatePlanInput},
    session::tests::session_id,
};
use merry_core::{
    PlanActivationSource, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId,
    PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot,
};
use std::sync::Arc;

fn persisted_image_message() -> UserMessageInput {
    UserMessageInput::new(
        "resume [Image #1] and [Image #2]",
        vec![
            UserImageInput::png(
                "[Image #1]",
                Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 11]),
                6,
                7,
            )
            .expect("valid first image"),
            UserImageInput::png(
                "[Image #2]",
                Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 22]),
                8,
                9,
            )
            .expect("valid second image"),
        ],
    )
    .expect("valid image message")
}

fn current_document() -> serde_json::Value {
    serde_json::json!({
        "format_version": 4,
        "external_tool_catalog": { "format_version": 1, "entries": [] },
        "session_id": session_id(),
        "next_sequence": 0,
        "session_started": false,
        "ledger": [],
        "artifacts": [],
        "compacted_checkpoint": null,
        "archived_ref_manifest": [],
        "prompt_history_projection": { "compacted_through": null },
        "context_entries": [],
        "transcript": {
            "items": [],
            "next_id": 0,
            "model_turns": {},
            "next_model_turn_id": 1
        },
        "resolved_tool_calls": [],
        "usage": null,
        "task_anchor": null,
        "registries": {
            "judgments": { "records": [] },
            "summary_draft_promotions": { "records": [] },
            "action_audits": { "records": [] }
        },
        "active_plan": null,
        "terminal_plans": []
    })
}

fn persisted_test_plan() -> PlanState {
    let mut plan = PlanState::empty(
        PlanId::new("persisted-plan").expect("valid plan id"),
        PlanActivationSource::Coordinator {
            reason: "persist the plan".to_owned(),
            governing_skill_id: None,
        },
        PlanResourcePolicySnapshot::default(),
    );
    plan.update(UpdatePlanInput {
        reason: "define persisted root".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Persist and resume the plan".to_owned(),
                acceptance: vec!["same ids and revisions after load".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Local,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    })
    .expect("valid persisted plan");
    plan
}

fn persisted_plan_leaf(client_key: &str, objective: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: objective.to_owned(),
        acceptance: vec![format!("{objective} is verified")],
        status: None,
        executor_policy: PlanExecutorPolicy::Delegate,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}

fn session_id_with_suffix(value: &str) -> merry_core::SessionId {
    merry_core::SessionId::new(value).expect("valid executor session id")
}

fn persisted_plan_node_id(plan: &PlanState, client_key: &str) -> merry_core::PlanNodeId {
    plan.snapshot()
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some(client_key))
        .map(|node| node.id.clone())
        .expect("client key remains in snapshot")
}

mod plan_state;

mod round_trip;

mod validation;
