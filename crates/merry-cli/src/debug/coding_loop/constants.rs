pub(crate) const CODING_LOOP_SMOKE_SESSION_ID: &str = "coding-loop-smoke";
pub(crate) const CODING_LOOP_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-live-smoke";
pub(crate) const CODING_LOOP_TASK_SMOKE_SESSION_ID: &str = "coding-loop-task-smoke";
pub(crate) const CODING_LOOP_TASK_LIVE_SMOKE_SESSION_ID: &str = "coding-loop-task-live-smoke";
pub(crate) const CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID: &str =
    "coding-loop-subagent-live-smoke";
pub(crate) const PERMISSION_NETWORK_SMOKE_SESSION_ID: &str = "permission-network-smoke";
pub(crate) const PERMISSION_NETWORK_SMOKE_ARGV: [&str; 3] = ["getent", "hosts", "example.com"];
pub(crate) const CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE: &str = "unfixed";
pub(crate) const CODING_LOOP_LIVE_SMOKE_TARGET_VALUE: &str = "fixed-by-live-llm";
pub(crate) const CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE: &str = "subagent-output.txt";
pub(crate) const CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL: &str = "status: pending\n";
pub(crate) const CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET: &str =
    "status: subagent-live-smoke-complete\n";
pub(crate) const CODING_LOOP_TASK_SMOKE_MAX_PATCH_BYTES: usize = 256;
