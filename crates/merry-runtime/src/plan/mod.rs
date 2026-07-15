mod attempt_binding;
pub(crate) mod control;
mod controller;
mod domain;
pub(crate) mod execution;
pub(crate) mod projection;
mod protocol;
pub(crate) mod recovery;
mod subagent;
#[allow(dead_code)]
mod subagent_scope;
pub(crate) mod tools;
mod validation;

pub(crate) const PLAN_READ_MAX_DEPTH: u8 = 16;

pub use controller::PlanControllerError;
pub(crate) use controller::{PlanController, PlanControllerEventReceiver};
pub use domain::PlanError;
pub(crate) use domain::{PersistedPlanState, PlanState};
pub use protocol::PlanUpdateOutput;
pub(crate) use protocol::update_plan_define_example;
#[allow(unused_imports)]
pub use protocol::{
    BeginPlanInput, BeginPlanOutput, ControlPlanAttemptInput, PlanApprovalInput, PlanChangeInput,
    PlanDecompositionInput, PlanExecutionIntent, PlanNodeInput, PlanNodeReferenceInput,
    ReadPlanInput, ReportPlanAttemptInput, ReportPlanProgressInput, SubagentPlanChangeInput,
    SubagentPlanUpdateInput, UpdatePlanInput,
};
pub(crate) use subagent::PlanArtifactPromotion;
pub use subagent::PlanSubagentControl;
pub(crate) use subagent_scope::PlanSubagentScope;

pub(crate) fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
