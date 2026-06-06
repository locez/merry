mod assertions;
mod commands;
mod constants;
mod fixture;
mod permission;
mod prompts;
mod provider;
mod report;
mod runtime;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use assertions::assert_coding_loop_task_smoke_uses_small_patch;
pub(crate) use assertions::{
    assert_coding_loop_live_smoke_tool_sequence, assert_coding_loop_smoke_result,
    assert_coding_loop_subagent_live_smoke_result, assert_coding_loop_task_live_smoke_result,
    assert_coding_loop_task_live_smoke_tool_sequence, assert_coding_loop_task_smoke_result,
    assert_permission_network_smoke_result,
};
pub(crate) use commands::{
    run_live_smoke, run_permission_network_smoke, run_smoke, run_subagent_live_smoke,
    run_task_live_smoke, run_task_smoke,
};
pub(crate) use constants::{
    CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE, CODING_LOOP_LIVE_SMOKE_SESSION_ID,
    CODING_LOOP_LIVE_SMOKE_TARGET_VALUE, CODING_LOOP_SMOKE_SESSION_ID,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE, CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID, CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
    CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID, CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES,
    CODING_LOOP_TASK_SMOKE_SESSION_ID, PERMISSION_NETWORK_SMOKE_ARGV,
    PERMISSION_NETWORK_SMOKE_SESSION_ID,
};
pub(crate) use fixture::{
    CodingLoopTaskSmokeFixture, coding_loop_smoke_patched_source,
    prepare_coding_loop_smoke_fixture, prepare_coding_loop_subagent_live_smoke_fixture,
    prepare_coding_loop_task_fixture,
};
#[cfg(test)]
pub(crate) use fixture::{coding_loop_smoke_initial_source, coding_loop_task_fixture_manifest};
pub(crate) use permission::{
    permission_network_live_smoke_task, permission_network_smoke_process_runner,
};
pub(crate) use prompts::{coding_loop_live_smoke_task, coding_loop_subagent_live_smoke_task};
pub(crate) use provider::{CodingLoopSmokeProvider, CodingLoopTaskSmokeProvider};
#[cfg(test)]
pub(crate) use provider::{
    PermissionNetworkSmokeProvider, PermissionNetworkSmokeReviewProvider, coding_loop_process_call,
    coding_loop_tool_call, coding_loop_workspace_call,
};
pub(crate) use report::{
    write_coding_loop_subagent_live_smoke_report, write_coding_loop_task_live_smoke_report,
    write_permission_network_smoke_report,
};
#[cfg(test)]
pub(crate) use runtime::build_scripted_permission_network_smoke_runtime;
pub(crate) use runtime::{
    CodingLoopLiveRuntimeOptions, build_coding_loop_live_smoke_runtime,
    build_coding_loop_smoke_runtime, build_coding_loop_subagent_live_smoke_runtime,
    build_coding_loop_task_live_smoke_runtime, build_coding_loop_task_smoke_runtime,
    build_permission_network_smoke_runtime, coding_loop_subagent_live_smoke_config,
};
