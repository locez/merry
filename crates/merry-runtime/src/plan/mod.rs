//! Session-scoped Markdown Plan Mode state machine and intrinsic tools.

pub(crate) mod tools;
mod tracker;

pub(crate) use tools::{EnterPlanModeInput, ExitPlanModeInput, ReadPlanInput, WritePlanInput};
pub use tracker::PlanError;
pub(crate) use tracker::{PersistedPlanModeState, PlanSessionState, validate_plan_file_name};

pub(crate) fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}
