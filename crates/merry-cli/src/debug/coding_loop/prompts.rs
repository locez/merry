use super::{
    CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE, CODING_LOOP_LIVE_SMOKE_TARGET_VALUE,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE, CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
};

pub(crate) fn coding_loop_live_smoke_task(relative_cwd: Option<&str>) -> String {
    let cwd = relative_cwd.unwrap_or(".");
    format!(
        "\
You are driving Merry's minimal live coding-loop smoke.

Use the available tools, one tool call per step. Do not answer from memory.

Required sequence:
1. Call `{process_tool}` with argv `[\"rg\", \"--files\"]` and cwd `{cwd}` to inspect the fixture.
2. Call `{read_tool}` with path `src/lib.rs` to read exact source.
3. Call `{patch_tool}` with one `patch` string:
   *** Begin Workspace Patch
   *** Update File: src/lib.rs
   -    \"{initial}\"
   +    \"{target}\"
   *** End Workspace Patch
4. Call `{process_tool}` with argv `[\"rg\", \"{target}\"]` and cwd `{cwd}` to verify.
5. After verification succeeds, return a concise final answer.

Constraints:
- Do not use shell strings, scripts, pipelines, env, stdin, git, cargo, or any command except the two exact rg argv values above.
- Do not modify any file except `src/lib.rs` through `{patch_tool}`.
- The final file must equal:

pub fn greeting() -> &'static str {{
    \"{target}\"
}}
",
        process_tool = CODING_LOOP_PROCESS_TOOL,
        read_tool = WORKSPACE_READ_FILE_TOOL,
        patch_tool = WORKSPACE_PATCH_TOOL,
        initial = CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE,
        target = CODING_LOOP_LIVE_SMOKE_TARGET_VALUE,
    )
}

pub(crate) fn coding_loop_subagent_live_smoke_task() -> String {
    format!(
        "\
You are driving Merry's minimal live subagent smoke.

You must delegate the work to a child agent before you finish.

Required sequence:
1. Call `spawn_subagents` with exactly one child task.
2. The child task must use `workspace_read_file` and `workspace_patch` only.
3. The child task must read `{file}` and patch it from:
   {initial}to:
   {target}
4. The child task must declare `allowed_tools` as `[\"workspace_read_file\", \"workspace_patch\"]`.
5. The child task must declare `read_scope` and `write_scope` as `[\"{file}\"]`.
6. After spawning, call `wait_subagents` for the returned child id with mode `all`.
7. After the child reports completion, call `workspace_read_file` on `{file}` and verify the exact final content.
8. Return a concise final answer only after the verification read succeeds.

Constraints:
- The parent agent must not patch the fixture directly.
- Do not use more than one child task.
- Do not answer from memory.
- Keep the final result short.
",
        file = CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE,
        initial = CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
        target = CODING_LOOP_SUBAGENT_LIVE_SMOKE_TARGET,
    )
}
