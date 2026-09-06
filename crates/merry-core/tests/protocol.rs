use merry_core::{
    PlanActivationSource, PlanExecutorPolicy, PlanId, PlanNodeId, PlanNodeSnapshot, PlanNodeStatus,
    PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, PlanRevisionSummary,
    PlanSchedulerStatus, PlanSnapshot,
};
use schemars::{JsonSchema, Schema};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

fn assert_json_round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_string(value).expect("value should serialize");
    let decoded = serde_json::from_str::<T>(&encoded).expect("value should deserialize");
    assert_eq!(&decoded, value);
}

fn assert_schema_compiles<T: JsonSchema>() {
    let _schema = schemars::schema_for!(T);
}

fn json_schema(value: Value) -> Schema {
    Schema::try_from(value).expect("test schema should be JSON schema")
}

fn assert_uuid_v4_string(value: &str) {
    assert_eq!(value.len(), 36);
    for (index, byte) in value.bytes().enumerate() {
        match index {
            8 | 13 | 18 | 23 => assert_eq!(byte, b'-'),
            14 => assert_eq!(byte, b'4'),
            19 => assert!(
                matches!(byte, b'8' | b'9' | b'a' | b'b'),
                "uuid variant nibble should be RFC 4122"
            ),
            _ => assert!(byte.is_ascii_hexdigit()),
        }
    }
}

fn sample_plan_snapshot() -> PlanSnapshot {
    let plan_id = PlanId::new("plan-1").expect("valid plan id");
    let root_node_id = PlanNodeId::new("node-root").expect("valid root id");
    PlanSnapshot {
        plan_id,
        revision: 2,
        phase: PlanPhase::Executing,
        activation_source: PlanActivationSource::Coordinator {
            reason: "coordinate independent work".to_owned(),
            governing_skill_id: None,
        },
        root_node_id: Some(root_node_id.clone()),
        coordinator_node_id: None,
        execution_contract_fingerprint: Some("contract-sha256".to_owned()),
        execution_authorization_refs: vec!["user-task-authority".to_owned()],
        authorized_capability_envelope: None,
        approval_requirements: Vec::new(),
        nodes: vec![PlanNodeSnapshot {
            id: root_node_id,
            client_key: None,
            parent_id: None,
            sibling_order: 0,
            objective: "Complete the recursive plan acceptance".to_owned(),
            acceptance: vec!["all deterministic checks pass".to_owned()],
            status: PlanNodeStatus::InProgress,
            executor_policy: PlanExecutorPolicy::Local,
            harness: Default::default(),
            recovery_policy: PlanRecoveryPolicySnapshot {
                max_transient_attempts: 2,
                retry_backoff_ms: 0,
                retry_only_before_observable_side_effects: true,
            },
            depends_on: Vec::new(),
            result: None,
            created_revision: 1,
            updated_revision: 2,
            declared_status: PlanNodeStatus::InProgress,
            execution_summary: Default::default(),
            links: Vec::new(),
        }],
        attempts: Vec::new(),
        leases: Vec::new(),
        attempt_progress: Vec::new(),
        directives: Vec::new(),
        resource_policy_snapshot: PlanResourcePolicySnapshot::default(),
        max_concurrency_hint: Some(2),
        scheduler_status: PlanSchedulerStatus::Active,
        revision_summaries: vec![
            PlanRevisionSummary::new(2, "root execution started").expect("valid revision summary"),
        ],
    }
}

#[path = "protocol/events.rs"]
mod events;

#[path = "protocol/evidence.rs"]
mod evidence;

#[path = "protocol/identifiers.rs"]
mod identifiers;

#[path = "protocol/plans.rs"]
mod plans;

#[path = "protocol/subagents.rs"]
mod subagents;

#[path = "protocol/tools.rs"]
mod tools;

#[path = "protocol/usage.rs"]
mod usage;

#[path = "protocol/validation.rs"]
mod validation;
