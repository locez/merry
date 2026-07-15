use crate::cli_error::{CliError, unexpected};
use crate::coding_runtime::{ActionProcessBackend, ActionProcessBackendOptions};
use crate::config::MerryConfig;
use merry_tool_workspace::CODING_LOOP_PROCESS_TOOL;
use std::path::Path;

use super::PERMISSION_NETWORK_SMOKE_ARGV;

pub(crate) fn permission_network_smoke_process_runner(
    workspace_root: &Path,
    merry_config: Option<&MerryConfig>,
) -> Result<ActionProcessBackend, CliError> {
    let path_rules = merry_config
        .map(MerryConfig::trusted_global_path_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    Ok(ActionProcessBackend::from_bwrap_options(
        workspace_root.to_path_buf(),
        ActionProcessBackendOptions {
            path_rules,
            network_allowed: false,
        },
    ))
}

pub(crate) fn permission_network_live_smoke_task() -> String {
    format!(
        "\
You are driving Merry's live permission-network smoke.

Use the available tools, one tool call per step. Do not answer from memory.

Required sequence:
1. Call `{process_tool}` with exactly this argv: [\"{program}\", \"{arg1}\", \"{arg2}\"].
2. The first process call is expected to fail because the default inner sandbox has no network.
3. If that first process call fails, call `request_permissions` for the exact same process action with requested network access:
   - reason: explain that the exact DNS lookup failed under the default inner sandbox and network is needed only for this smoke command.
   - requested: {{\"network\": true}}
   - for_action: {{\"kind\": \"process\", \"argv\": [\"{program}\", \"{arg1}\", \"{arg2}\"]}}
4. After `request_permissions` resolves, inspect the tool result. It should execute the exact planned process action under the approved per-action network profile.
5. Return a concise final answer only after the approved process result succeeds.

Constraints:
- Do not request any filesystem path permission.
- Do not request network before the first process attempt fails.
- Do not use shell strings, scripts, pipelines, env, stdin, git, cargo, curl, wget, or any command other than the exact argv above.
- Do not call any workspace patch/write tool.
",
        process_tool = CODING_LOOP_PROCESS_TOOL,
        program = PERMISSION_NETWORK_SMOKE_ARGV[0],
        arg1 = PERMISSION_NETWORK_SMOKE_ARGV[1],
        arg2 = PERMISSION_NETWORK_SMOKE_ARGV[2],
    )
}
