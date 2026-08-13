use crate::cli_error::{CliError, unexpected};
use crate::coding_runtime::ActionProcessBackend;
use crate::config::MerryConfig;
use crate::runtime_config::action_process_backend_options;
use merry_tool_workspace::CODING_LOOP_PROCESS_TOOL;
use std::path::Path;

use super::PERMISSION_NETWORK_SMOKE_ARGV;

pub(crate) fn permission_network_smoke_process_runner(
    workspace_root: &Path,
    merry_config: Option<&MerryConfig>,
) -> Result<ActionProcessBackend, CliError> {
    let mut options = action_process_backend_options(merry_config).map_err(unexpected)?;
    // This smoke must prove that the first attempt is denied by the default
    // inner profile even when the user enables network access globally.
    options.network_allowed = false;
    ActionProcessBackend::from_bwrap_options(workspace_root.to_path_buf(), options)
        .map_err(Into::into)
}

pub(crate) fn permission_network_live_smoke_task() -> String {
    format!(
        "\
You are driving Merry's live permission-network smoke.

Use the available tools, one tool call per step. Do not answer from memory.

Required sequence:
1. Call `{process_tool}` with exactly this command: `{program} {arg1} {arg2}` and cwd null.
2. The first process call is expected to fail because the default inner sandbox has no network.
3. If that first process call fails, call `request_permissions` for the exact same process action with requested network access:
   - reason: explain that the exact DNS lookup failed under the default inner sandbox and network is needed only for this smoke command.
   - requested: {{\"network\": true}}
   - for_action: {{\"kind\": \"process\", \"command\": \"{program} {arg1} {arg2}\", \"cwd\": null}}
4. After `request_permissions` resolves, inspect the tool result. It should execute the exact planned process action under the approved per-action network profile.
5. Return a concise final answer only after the approved process result succeeds.

Constraints:
- Do not request any filesystem path permission.
- Do not request network before the first process attempt fails.
- Do not use scripts, pipelines, env, stdin, git, cargo, curl, wget, or any command other than the exact command above.
- Do not call any workspace patch/write tool.
",
        process_tool = CODING_LOOP_PROCESS_TOOL,
        program = PERMISSION_NETWORK_SMOKE_ARGV[0],
        arg1 = PERMISSION_NETWORK_SMOKE_ARGV[1],
        arg2 = PERMISSION_NETWORK_SMOKE_ARGV[2],
    )
}
